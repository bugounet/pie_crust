//! These exercise MCP over a real TCP/HTTP connection, including the SDK's
//! initialize/session handshake rather than calling tool handlers directly.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use pie_crust_core::{SharedWorkbench, Workbench};
use pie_crust_mcp::{ServerHandle, start_server};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};

const TOKEN: &str = "test-only-token-0123456789abcdef0123456789abcdef";
const PROTOCOL: &str = "2025-03-26";

struct Fixture {
    _directory: tempfile::TempDir,
    workbench: SharedWorkbench,
    server: ServerHandle,
    client: Client,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("main.py"),
            "def greet(name: str) -> str:\n    return f'hello {name}'\n",
        )
        .unwrap();
        std::fs::write(directory.path().join("private.py"), "private = True\n").unwrap();
        let workbench = Arc::new(Mutex::new(Workbench::open(&root).unwrap()));
        let server = start_server(workbench.clone(), 0, TOKEN.into()).unwrap();
        let client = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            _directory: directory,
            workbench,
            server,
            client,
        }
    }

    fn post(&self, payload: &Value, session: Option<&str>) -> RequestBuilder {
        let mut request = self
            .client
            .post(&self.server.endpoint)
            .bearer_auth(TOKEN)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL)
            .json(payload);
        if let Some(session) = session {
            request = request.header("Mcp-Session-Id", session);
        }
        request
    }

    async fn initialize(&self) -> String {
        let payload = initialize_payload();
        let response = self.post(&payload, None).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let session = response
            .headers()
            .get("Mcp-Session-Id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let response = rpc_body(response.text().await.unwrap(), &json!(1));
        assert!(response["result"]["capabilities"]["tools"].is_object());
        let response = self
            .post(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                Some(&session),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        session
    }

    async fn rpc(&self, session: &str, id: u64, method: &str, params: Value) -> Value {
        let response = self
            .post(
                &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
                Some(session),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        rpc_body(response.text().await.unwrap(), &json!(id))
    }

    async fn tool(&self, session: &str, id: u64, name: &str, arguments: Value) -> Value {
        let response = self
            .rpc(
                session,
                id,
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            )
            .await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
}

fn initialize_payload() -> Value {
    json!({
        "jsonrpc":"2.0", "id":1, "method":"initialize",
        "params":{
            "protocolVersion":PROTOCOL,
            "capabilities":{},
            "clientInfo":{"name":"pie_crust-integration-test","version":"0.1.0"}
        }
    })
}

fn rpc_body(body: String, id: &Value) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(&body) {
        return value;
    }
    // Streamable HTTP also permits SSE for an individual POST response.
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value.get("id") == Some(id))
        .unwrap_or_else(|| panic!("No JSON-RPC response found: {body}"))
}

#[tokio::test]
async fn authenticated_mcp_exposes_real_workbench_operations() {
    let fixture = Fixture::new();
    let session = fixture.initialize().await;
    let listed = fixture.rpc(&session, 2, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6);
    for name in [
        "workspace_list",
        "workspace_focus",
        "editor_focus",
        "document_read",
        "code_search",
        "index_rebuild",
    ] {
        assert!(tools.iter().any(|tool| tool["name"] == name));
    }
    let listed = fixture.tool(&session, 3, "workspace_list", json!({})).await;
    let worktree = listed["structuredContent"]["active_worktree_id"]
        .as_str()
        .unwrap();
    let indexed = fixture
        .tool(
            &session,
            4,
            "index_rebuild",
            json!({"worktree_id":worktree}),
        )
        .await;
    assert_ne!(indexed["isError"], true, "{indexed}");
    assert!(
        indexed["structuredContent"]["stats"]["files"]
            .as_u64()
            .unwrap()
            >= 1
    );
    let searched = fixture
        .tool(
            &session,
            5,
            "code_search",
            json!({"worktree_id":worktree,"query":"greet"}),
        )
        .await;
    let hits = searched["structuredContent"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["path"], "main.py");
    assert_eq!(hits[0]["line"], 1);
    let read = fixture
        .tool(
            &session,
            6,
            "document_read",
            json!({"worktree_id":worktree,"path":"main.py","start_line":2,"line_count":1}),
        )
        .await;
    assert_eq!(
        read["structuredContent"]["text"],
        "    return f'hello {name}'\n"
    );
    let document_id = read["structuredContent"]["document_id"].as_str().unwrap();
    let version = read["structuredContent"]["version"].as_u64().unwrap();
    fixture
        .workbench
        .lock()
        .unwrap()
        .edit_document(document_id, version, "# unsaved\nanswer = 42\n".into())
        .unwrap();
    let read = fixture
        .tool(
            &session,
            7,
            "document_read",
            json!({"worktree_id":worktree,"path":"main.py"}),
        )
        .await;
    assert_eq!(
        read["structuredContent"]["text"],
        "# unsaved\nanswer = 42\n"
    );
    assert_eq!(read["structuredContent"]["dirty"], true);
    let focused = fixture
        .tool(
            &session,
            8,
            "editor_focus",
            json!({"worktree_id":worktree,"path":"main.py","line":2,"column":3}),
        )
        .await;
    assert_eq!(focused["structuredContent"]["status"], "focus_requested");
    let focus = fixture.workbench.lock().unwrap().focus().unwrap();
    assert_eq!((focus.line, focus.column), (2, 3));
    assert!(focus.serial > 0);
    let selected = fixture
        .tool(
            &session,
            9,
            "workspace_focus",
            json!({"worktree_id":worktree}),
        )
        .await;
    assert_eq!(selected["structuredContent"]["status"], "focus_requested");
    fixture.server.shutdown();
}

#[tokio::test]
async fn http_rejects_missing_or_invalid_auth_and_untrusted_hosts_and_origins() {
    let fixture = Fixture::new();
    let request = initialize_payload();
    let missing = fixture
        .client
        .post(&fixture.server.endpoint)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert!(missing.headers().contains_key("WWW-Authenticate"));
    let wrong = fixture
        .client
        .post(&fixture.server.endpoint)
        .bearer_auth("incorrect")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    for (name, value) in [
        ("Host", "attacker.invalid"),
        ("Origin", "https://attacker.invalid"),
        ("Origin", "null"),
    ] {
        let response = fixture
            .post(&request, None)
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "header {name}: {value}"
        );
    }
    let session = fixture.initialize().await;
    let no_auth = fixture
        .client
        .post(&fixture.server.endpoint)
        .header("Mcp-Session-Id", &session)
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        no_auth.status(),
        StatusCode::UNAUTHORIZED,
        "Sessions must not bypass authentication"
    );
    let oversized = fixture
        .post(&json!({"padding":"x".repeat(70_000)}), None)
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    fixture.server.shutdown();
}

#[tokio::test]
async fn current_stateless_protocol_works_without_a_legacy_handshake() {
    let fixture = Fixture::new();
    let metadata = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "pie_crust-current-protocol-test", "version": "0.1.0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    for (id, method, params) in [
        (1, "tools/list", json!({"_meta": metadata})),
        (
            2,
            "tools/call",
            json!({"_meta": metadata, "name": "workspace_list", "arguments": {}}),
        ),
    ] {
        // The current protocol requires HTTP routing headers to agree with
        // the JSON-RPC method and, for named operations, its target name.
        let mut request = fixture
            .client
            .post(&fixture.server.endpoint)
            .bearer_auth(TOKEN)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2026-07-28")
            .header("Mcp-Method", method);
        if let Some(name) = params.get("name").and_then(Value::as_str) {
            request = request.header("Mcp-Name", name);
        }
        let response = request
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        assert!(!response.headers().contains_key("Mcp-Session-Id"));
        let body = response.text().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{method}: {body}");
        let result = rpc_body(body, &json!(id));
        assert!(result.get("error").is_none(), "{result}");
        if method == "tools/list" {
            assert_eq!(result["result"]["tools"].as_array().unwrap().len(), 6);
        } else {
            assert!(result["result"]["structuredContent"]["active_worktree_id"].is_string());
        }
    }
    fixture.server.shutdown();
}

#[tokio::test]
async fn document_reads_resume_long_unicode_lines_without_mixing_versions() {
    let fixture = Fixture::new();
    let session = fixture.initialize().await;
    let worktree = fixture
        .workbench
        .lock()
        .unwrap()
        .active_worktree_id()
        .to_owned();
    let original = format!("{}\nlast\n", "😀é".repeat(50_000));
    let document = fixture
        .workbench
        .lock()
        .unwrap()
        .read_document(&worktree, Path::new("main.py"))
        .unwrap();
    let version = fixture
        .workbench
        .lock()
        .unwrap()
        .edit_document(&document.id, document.version, original.clone())
        .unwrap();
    let first = fixture
        .tool(
            &session,
            2,
            "document_read",
            json!({
                "worktree_id": worktree, "path": "main.py", "max_bytes": 262144
            }),
        )
        .await;
    let first = &first["structuredContent"];
    let cursor = first["next_byte_offset"].as_u64().unwrap();
    assert_eq!(first["version"], version);
    assert_eq!(first["start_line"], 1);
    assert!(cursor > 0 && cursor <= 262144);
    let second = fixture
        .tool(
            &session,
            3,
            "document_read",
            json!({
                "worktree_id": worktree, "path": "main.py", "byte_offset": cursor,
                "expected_version": version, "max_bytes": 262144
            }),
        )
        .await;
    let second = &second["structuredContent"];
    assert_eq!(second["start_line"], 1);
    assert!(second["next_byte_offset"].is_null());
    assert_eq!(
        format!(
            "{}{}",
            first["text"].as_str().unwrap(),
            second["text"].as_str().unwrap()
        ),
        original
    );
    fixture
        .workbench
        .lock()
        .unwrap()
        .edit_document(&document.id, version, format!("{original}# edited\n"))
        .unwrap();
    let stale = fixture.tool(&session, 4, "document_read", json!({
        "worktree_id": worktree, "path": "main.py", "byte_offset": cursor, "expected_version": version
    })).await;
    assert_eq!(stale["isError"], true);
    let missing_version = fixture
        .tool(
            &session,
            5,
            "document_read",
            json!({
                "worktree_id": worktree, "path": "main.py", "byte_offset": cursor
            }),
        )
        .await;
    assert_eq!(missing_version["isError"], true);
    fixture.server.shutdown();
}

#[tokio::test]
async fn tools_reject_outside_paths_unknown_worktrees_and_unbounded_requests() {
    let fixture = Fixture::new();
    let session = fixture.initialize().await;
    let worktree = fixture
        .workbench
        .lock()
        .unwrap()
        .active_worktree_id()
        .to_owned();
    for (index, name, arguments) in [
        (
            2,
            "document_read",
            json!({"worktree_id":worktree,"path":"../private.py"}),
        ),
        (
            3,
            "document_read",
            json!({"worktree_id":worktree,"path":"main.py","max_bytes":262145}),
        ),
        (4, "code_search", json!({"worktree_id":worktree,"query":""})),
        (
            5,
            "code_search",
            json!({"worktree_id":worktree,"query":"x","limit":201}),
        ),
        (6, "workspace_focus", json!({"worktree_id":"unknown"})),
        (
            7,
            "editor_focus",
            json!({"worktree_id":worktree,"path":"main.py","line":0}),
        ),
    ] {
        let result = fixture.tool(&session, index, name, arguments).await;
        assert_eq!(result["isError"], true, "{name}: {result}");
    }
    assert!(
        fixture
            .workbench
            .lock()
            .unwrap()
            .read_document(&worktree, Path::new("main.py"))
            .is_ok()
    );
    fixture.server.shutdown();
}

#[test]
fn startup_errors_are_synchronous_and_do_not_disclose_tokens() {
    let fixture = Fixture::new();
    let error = start_server(fixture.workbench.clone(), 0, "secret".into())
        .err()
        .unwrap()
        .to_string();
    assert!(!error.contains("secret"));
    let port = fixture
        .server
        .endpoint
        .split(':')
        .nth(2)
        .unwrap()
        .split('/')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(start_server(fixture.workbench.clone(), port, TOKEN.into()).is_err());
    fixture.server.shutdown();
}
