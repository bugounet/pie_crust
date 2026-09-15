//! Local Streamable HTTP MCP server sharing the desktop's workbench.
//!
//! Authentication is required on every HTTP request. The listener is always
//! loopback-only; a caller must supply a random token of at least 32 characters.

use std::{
    future::IntoFuture,
    net::{Ipv4Addr, TcpListener},
    path::Path,
    sync::{Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use pie_crust_core::{SharedWorkbench, Workbench};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_PORT: u16 = 43127;
const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Owns a local MCP listener. Dropping this handle requests shutdown.
///
/// No token is retained here or included in diagnostic output.
pub struct ServerHandle {
    pub endpoint: String,
    cancellation: CancellationToken,
    thread: Option<JoinHandle<()>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl ServerHandle {
    pub fn is_running(&self) -> bool {
        !self.cancellation.is_cancelled()
            && self
                .thread
                .as_ref()
                .is_some_and(|thread| !thread.is_finished())
    }

    pub fn error(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|failure| failure.clone())
    }

    /// Stop and wait for the listener to close. Ordinary UI teardown can just drop it.
    pub fn shutdown(mut self) {
        self.cancellation.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Start MCP at `http://127.0.0.1:<port>/mcp` on its own runtime.
///
/// Port zero requests an available ephemeral port. Binding and runtime creation
/// happen before returning, so a reported startup success owns the endpoint.
pub fn start_server(workbench: SharedWorkbench, port: u16, token: String) -> Result<ServerHandle> {
    if token.len() < 32 || token.len() > 512 || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!("MCP requires a random bearer token of 32–512 visible ASCII characters");
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .with_context(|| format!("Cannot bind the local MCP server on port {port}"))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let endpoint = format!("http://127.0.0.1:{port}/mcp");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_name("pie_crust-mcp-worker")
        .enable_all()
        .build()
        .context("Cannot start the MCP runtime")?;
    let listener = {
        let _entered = runtime.enter();
        tokio::net::TcpListener::from_std(listener)?
    };
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    let failure = Arc::new(Mutex::new(None));
    let thread_failure = failure.clone();
    let thread = std::thread::Builder::new()
        .name("pie_crust-mcp".into())
        .spawn(move || {
            runtime.block_on(async move {
                let config = StreamableHttpServerConfig::default()
                    .with_allowed_hosts([format!("127.0.0.1:{port}"), format!("localhost:{port}")])
                    .with_allowed_origins([
                        format!("http://127.0.0.1:{port}"),
                        format!("http://localhost:{port}"),
                    ])
                    .with_max_request_body_bytes(MAX_REQUEST_BYTES)
                    .with_cancellation_token(shutdown.child_token());
                let tools = WorkbenchTools::new(workbench);
                let service = StreamableHttpService::new(
                    move || Ok(tools.clone()),
                    Arc::new(LocalSessionManager::default()),
                    config,
                );
                let app = Router::new().route_service("/mcp", service).layer(
                    middleware::from_fn_with_state(Arc::new(token), authenticate),
                );
                let serving = axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.clone().cancelled_owned())
                    .into_future();
                // Cancelling sessions ends SSE responses. Bound the remaining
                // drain time so an idle client cannot hold the desktop open.
                tokio::pin!(serving);
                let outcome = tokio::select! {
                    result = &mut serving => Some(result),
                    () = shutdown.cancelled() => {
                        tokio::time::timeout(Duration::from_secs(2), &mut serving).await.ok()
                    }
                };
                if let Some(Err(error)) = outcome
                    && let Ok(mut failure) = thread_failure.lock()
                {
                    *failure = Some(format!("MCP listener stopped: {error}"));
                }
            });
            runtime.shutdown_timeout(Duration::from_secs(1));
        })
        .context("Cannot start the MCP listener thread")?;
    Ok(ServerHandle {
        endpoint,
        cancellation,
        thread: Some(thread),
        failure,
    })
}

async fn authenticate(
    State(token): State<Arc<String>>,
    mut request: Request,
    next: Next,
) -> Response {
    let mut authorization = request.headers().get_all(header::AUTHORIZATION).iter();
    let supplied = authorization
        .next()
        .and_then(|header| header.to_str().ok())
        .and_then(|header| header.strip_prefix("Bearer "));
    let valid = authorization.next().is_none()
        && supplied.is_some_and(|supplied| bool::from(supplied.as_bytes().ct_eq(token.as_bytes())));
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer realm=\"pie_crust\"")],
            "A valid pie_crust bearer token is required",
        )
            .into_response();
    }
    // The SDK exposes HTTP parts to handlers; no handler or tracing layer needs
    // to receive the credential after it has been checked.
    request.headers_mut().remove(header::AUTHORIZATION);
    next.run(request).await
}

#[derive(Clone)]
struct WorkbenchTools {
    workbench: SharedWorkbench,
    rebuild_permit: Arc<Semaphore>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkspaceInput {
    /// Stable worktree ID returned by workspace_list.
    worktree_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FocusInput {
    /// Stable worktree ID returned by workspace_list.
    worktree_id: String,
    /// File path relative to that worktree, or an absolute path inside it.
    path: String,
    /// One-based line, default 1.
    line: Option<usize>,
    /// One-based Unicode scalar column, default 1.
    column: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    /// Stable worktree ID returned by workspace_list.
    worktree_id: String,
    /// File path relative to that worktree, or an absolute path inside it.
    path: String,
    /// First line to read, one-based; default 1.
    start_line: Option<usize>,
    /// Zero-based UTF-8 byte cursor returned as next_byte_offset. Cannot combine with start_line. Requires expected_version.
    byte_offset: Option<usize>,
    /// Document version from the previous result. Required with byte_offset; rejects a changed buffer instead of mixing versions.
    expected_version: Option<u64>,
    /// Maximum lines, from 1 to 2000; default 200.
    line_count: Option<usize>,
    /// Maximum UTF-8 bytes, from 1 to 262144; default 65536.
    max_bytes: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    /// Stable worktree ID returned by workspace_list.
    worktree_id: String,
    /// Case-sensitive literal text, 1 to 4096 UTF-8 bytes.
    query: String,
    /// Maximum matches, from 1 to 200; default 50.
    limit: Option<usize>,
}

#[tool_router]
impl WorkbenchTools {
    fn new(workbench: SharedWorkbench) -> Self {
        Self {
            workbench,
            rebuild_permit: Arc::new(Semaphore::new(1)),
        }
    }

    #[tool(
        description = "List this IDE project's registered worktrees and current selection. IDs remain stable when branches change.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn workspace_list(&self) -> CallToolResult {
        self.with_workbench(|workbench| {
            Ok(json!({
                "project_root": workbench.project_root(),
                "active_worktree_id": workbench.active_worktree_id(),
                "worktrees": workbench.worktrees(),
            }))
        })
        .await
    }

    #[tool(
        description = "Select a registered worktree in the IDE. Requests UI navigation; does not guarantee the OS brings the window to the foreground.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn workspace_focus(
        &self,
        Parameters(input): Parameters<WorkspaceInput>,
    ) -> CallToolResult {
        self.with_workbench(move |workbench| {
            workbench.focus_worktree(&input.worktree_id)?;
            Ok(json!({"status": "focus_requested", "worktree_id": workbench.active_worktree_id()}))
        })
        .await
    }

    #[tool(
        description = "Open a file and request editor focus at a one-based line and Unicode scalar column. Paths must remain inside the selected worktree. Does not guarantee OS window activation.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn editor_focus(&self, Parameters(input): Parameters<FocusInput>) -> CallToolResult {
        self.with_workbench(move |workbench| {
            check_path(&input.path)?;
            let target = workbench.focus_document(
                &input.worktree_id,
                Path::new(&input.path),
                input.line.unwrap_or(1),
                input.column.unwrap_or(1),
            )?;
            Ok(json!({"status": "focus_requested", "target": target}))
        })
        .await
    }

    #[tool(
        description = "Read a bounded range of a UTF-8 file from a registered worktree. The current unsaved editor buffer takes precedence over disk. Continue truncated results using next_byte_offset as byte_offset and version as expected_version; this also handles very long lines.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn document_read(&self, Parameters(input): Parameters<ReadInput>) -> CallToolResult {
        self.with_workbench(move |workbench| {
            check_path(&input.path)?;
            let start = input.start_line.unwrap_or(1);
            let count = input.line_count.unwrap_or(200);
            let max_bytes = input.max_bytes.unwrap_or(65536);
            if start == 0
                || !(1..=2000).contains(&count)
                || !(1..=MAX_DOCUMENT_BYTES).contains(&max_bytes)
            {
                bail!("Use start_line >= 1, line_count 1–2000 and max_bytes 1–262144");
            }
            if input.byte_offset.is_some() && input.expected_version.is_none() {
                bail!("Reading from byte_offset requires expected_version from the previous result");
            }
            let document = workbench.read_document(&input.worktree_id, Path::new(&input.path))?;
            if input.expected_version.is_some_and(|version| version != document.version) {
                bail!("Document version changed; restart the read without byte_offset or expected_version");
            }
            let excerpt = excerpt(&document.text, input.start_line, input.byte_offset, count, max_bytes)?;
            Ok(json!({
                "document_id": document.id,
                "worktree_id": document.worktree_id,
                "path": document.path,
                "version": document.version,
                "dirty": document.dirty,
                "start_line": excerpt.start_line,
                "byte_offset": excerpt.byte_offset,
                "next_byte_offset": excerpt.next_byte_offset,
                "text": excerpt.text,
                "truncated": excerpt.truncated,
                "total_lines": excerpt.total_lines,
            }))
        })
        .await
    }

    #[tool(
        description = "Search literal code text in one worktree using its independent index and configured exclusions. Rebuild the index after external changes. This is text search, not a typed caller hierarchy.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn code_search(&self, Parameters(input): Parameters<SearchInput>) -> CallToolResult {
        let workbench = self.workbench.clone();
        task_result(
            tokio::task::spawn_blocking(move || {
                let limit = input.limit.unwrap_or(50);
                if input.query.is_empty()
                    || input.query.len() > 4096
                    || !(1..=MAX_SEARCH_RESULTS).contains(&limit)
                {
                    bail!("Use a query of 1–4096 UTF-8 bytes and a limit of 1–200");
                }
                let (request, documents) = workbench
                    .lock()
                    .map_err(|_| anyhow!("The workbench is unavailable after an internal error"))?
                    .search_snapshot(&input.worktree_id)?;
                let mut hits = request.search(&input.query, limit, documents.iter())?;
                for hit in &mut hits {
                    if hit.preview.len() > 2048 {
                        let end = utf8_prefix_len(&hit.preview, 2048);
                        hit.preview.truncate(end);
                    }
                }
                Ok(json!({"worktree_id": input.worktree_id, "hits": hits, "limit": limit}))
            })
            .await,
        )
    }

    #[tool(
        description = "Reconcile the independent SQLite code index for a registered worktree under .pie_crust/indexes. Runs in a worker without holding the workbench lock. Only one MCP rebuild can run at a time.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn index_rebuild(&self, Parameters(input): Parameters<WorkspaceInput>) -> CallToolResult {
        let permit = match self.rebuild_permit.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return tool_error("An index rebuild is already running; retry when it finishes");
            }
        };
        let workbench = self.workbench.clone();
        task_result(
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let request = workbench
                    .lock()
                    .map_err(|_| anyhow!("The workbench is unavailable after an internal error"))?
                    .index_request(&input.worktree_id)?;
                let stats = request.rebuild()?;
                Ok(json!({"worktree_id": input.worktree_id, "stats": stats}))
            })
            .await,
        )
    }
}

impl WorkbenchTools {
    async fn with_workbench<F>(&self, operation: F) -> CallToolResult
    where
        F: FnOnce(&mut Workbench) -> Result<Value> + Send + 'static,
    {
        let workbench = self.workbench.clone();
        task_result(
            tokio::task::spawn_blocking(move || {
                let mut workbench = workbench
                    .lock()
                    .map_err(|_| anyhow!("The workbench is unavailable after an internal error"))?;
                operation(&mut workbench)
            })
            .await,
        )
    }
}

#[tool_handler]
impl ServerHandler for WorkbenchTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("pie_crust", env!("CARGO_PKG_VERSION")))
            .with_instructions("pie_crust exposes the running desktop's workbench. Start with workspace_list and always pass an explicit worktree_id. Paths and positions belong to that worktree. Focus tools request navigation; they do not claim OS window activation. Refactorings and typed caller navigation are not implemented in this first increment.")
    }
}

fn task_result(
    result: std::result::Result<Result<Value>, tokio::task::JoinError>,
) -> CallToolResult {
    match result {
        Ok(Ok(value)) => CallToolResult::structured(value),
        Ok(Err(error)) => tool_error(format!("{error:#}")),
        Err(_) => tool_error("The workbench operation failed in its worker"),
    }
}

fn tool_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::structured_error(json!({"error": message.into()}))
}

fn check_path(path: &str) -> Result<()> {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        bail!("File path must contain 1–4096 UTF-8 bytes and no NUL characters");
    }
    Ok(())
}

fn utf8_prefix_len(text: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

struct Excerpt {
    text: String,
    start_line: usize,
    byte_offset: usize,
    next_byte_offset: Option<usize>,
    total_lines: usize,
    truncated: bool,
}

fn excerpt(
    text: &str,
    start_line: Option<usize>,
    byte_offset: Option<usize>,
    count: usize,
    max_bytes: usize,
) -> Result<Excerpt> {
    if start_line.is_some() && byte_offset.is_some() {
        bail!("Choose either start_line or byte_offset, not both");
    }
    let start = start_line.unwrap_or(1);
    let total_lines = text.bytes().filter(|byte| *byte == b'\n').count() + 1;
    if start > total_lines {
        bail!("start_line exceeds the document's {total_lines} lines");
    }
    let from = if let Some(offset) = byte_offset {
        if offset > text.len() || !text.is_char_boundary(offset) {
            bail!("byte_offset must be a UTF-8 character boundary inside the document");
        }
        offset
    } else if start == 1 {
        0
    } else {
        text.match_indices('\n')
            .nth(start - 2)
            .map(|(index, _)| index + 1)
            .unwrap_or(text.len())
    };
    let remaining = &text[from..];
    let line_end = remaining
        .match_indices('\n')
        .nth(count - 1)
        .map(|(index, _)| index + 1)
        .unwrap_or(remaining.len());
    let end = utf8_prefix_len(&remaining[..line_end], max_bytes);
    if end == 0 && !remaining.is_empty() {
        bail!("max_bytes is too small to include the next complete UTF-8 character");
    }
    Ok(Excerpt {
        text: remaining[..end].to_owned(),
        start_line: text[..from].bytes().filter(|byte| *byte == b'\n').count() + 1,
        byte_offset: from,
        next_byte_offset: (end < remaining.len()).then_some(from + end),
        total_lines,
        truncated: end < remaining.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts_preserve_line_endings_and_utf8_boundaries() {
        let result = excerpt("one\r\néclair\r\nlast", Some(2), None, 1, 100).unwrap();
        assert_eq!(result.text, "éclair\r\n");
        assert_eq!(result.total_lines, 3);
        assert!(result.truncated);
        let result = excerpt("aé😀z", None, None, 1, 6).unwrap();
        assert_eq!(result.text, "aé");
        assert!(result.truncated);
        assert_eq!(result.next_byte_offset, Some(3));
        let next = excerpt("aé😀z", None, result.next_byte_offset, 1, 6).unwrap();
        assert_eq!(next.text, "😀z");
        assert_eq!(next.next_byte_offset, None);
        assert!(excerpt("aé😀z", None, Some(2), 1, 6).is_err());
        assert!(excerpt("😀z", None, None, 1, 1).is_err());
        let result = excerpt("line\n", Some(2), None, 1, 100).unwrap();
        assert_eq!(result.text, "");
        assert!(!result.truncated);
        assert!(excerpt("line", Some(2), None, 1, 100).is_err());
    }
}
