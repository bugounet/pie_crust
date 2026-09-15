use crate::config::{Config, ScopeConfig};
use crate::document::{
    Document, MAX_TEXT_BYTES, digest, internal_path, resolve_buffer_path, resolve_file,
};
use crate::worktree::WorktreeInfo;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use ignore::WalkBuilder;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IndexStats {
    pub files: usize,
    pub updated: usize,
    pub removed: usize,
}

/// Owned work description; execute on a worker without holding the workbench mutex.
#[derive(Clone, Debug)]
pub struct IndexRequest {
    worktree: WorktreeInfo,
    path: PathBuf,
    config: Config,
}

#[derive(Debug)]
struct Stamp {
    modified: String,
    length: u64,
}

impl Stamp {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos().to_string())
            .unwrap_or_default();
        Ok(Self {
            modified,
            length: metadata.len(),
        })
    }
}

impl IndexRequest {
    pub(crate) fn new(worktree: WorktreeInfo, project_root: &Path) -> Result<Self> {
        let path = project_root
            .join(".pie_crust/indexes")
            .join(format!("{}.sqlite", worktree.id));
        Ok(Self {
            worktree,
            path,
            config: Config::load(project_root)?,
        })
    }

    pub fn index_path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> Result<Connection> {
        // Serialize connection setup across processes before anyone opens SQLite.
        // SQLite's busy handler cannot resolve competing journal-mode changes;
        // a reader observing the empty schema can otherwise block WAL activation.
        // The guard is released when this function returns, so established readers
        // and index writers still run concurrently under SQLite's WAL locking.
        let initialization_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("sqlite.lock"))?;
        initialization_lock
            .lock_exclusive()
            .context("Cannot lock index initialization")?;
        let connection = Connection::open(&self.path)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        let schema_ready: bool = connection.query_row(
            "SELECT count(*) = 2 FROM sqlite_master WHERE name IN ('metadata', 'code_search')",
            [],
            |row| row.get(0),
        )?;
        if schema_ready {
            self.validate_metadata(&connection)?;
            return Ok(connection);
        }
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY, hash TEXT NOT NULL, modified TEXT NOT NULL,
                byte_length INTEGER NOT NULL, content TEXT NOT NULL
             );",
        )?;
        for (key, expected) in [
            ("schema_version", "1"),
            ("worktree_id", self.worktree.id.as_str()),
        ] {
            connection.execute(
                "INSERT OR IGNORE INTO metadata(key, value) VALUES (?1, ?2)",
                params![key, expected],
            )?;
        }
        self.validate_metadata(&connection)?;
        let fts_exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='code_search')",
            [],
            |row| row.get(0),
        )?;
        connection.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS code_search USING fts5(
                 content, content='files', content_rowid='rowid', tokenize='trigram case_sensitive 1'
             );
             CREATE TRIGGER IF NOT EXISTS files_insert AFTER INSERT ON files BEGIN
                 INSERT INTO code_search(rowid, content) VALUES(new.rowid, new.content);
             END;
             CREATE TRIGGER IF NOT EXISTS files_delete AFTER DELETE ON files BEGIN
                 INSERT INTO code_search(code_search, rowid, content) VALUES('delete', old.rowid, old.content);
             END;
             CREATE TRIGGER IF NOT EXISTS files_update AFTER UPDATE OF content ON files BEGIN
                 INSERT INTO code_search(code_search, rowid, content) VALUES('delete', old.rowid, old.content);
                 INSERT INTO code_search(rowid, content) VALUES(new.rowid, new.content);
             END;"
        )?;
        if !fts_exists {
            connection.execute("INSERT INTO code_search(code_search) VALUES('rebuild')", [])?;
        }
        connection.execute_batch("COMMIT")?;
        Ok(connection)
    }

    fn validate_metadata(&self, connection: &Connection) -> Result<()> {
        for (key, expected) in [
            ("schema_version", "1"),
            ("worktree_id", self.worktree.id.as_str()),
        ] {
            let actual: String = connection.query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                [key],
                |row| row.get(0),
            )?;
            if actual != expected {
                bail!("Index metadata mismatch for {key}: {}", self.path.display());
            }
        }
        Ok(())
    }

    /// Reconciles additions, changes, and removals in a single SQLite transaction.
    /// Unchanged entries are reused from the persistent index using their size and
    /// modification time, so reopening a project does not reread every source file.
    pub fn refresh(&self) -> Result<IndexStats> {
        self.reconcile(false)
    }

    /// Fully reconciles the index, revalidating every source hash. This also
    /// catches equal-size edits whose modification time was restored manually.
    pub fn rebuild(&self) -> Result<IndexStats> {
        self.reconcile(true)
    }

    fn reconcile(&self, verify_hashes: bool) -> Result<IndexStats> {
        let paths = walk_scope(&self.worktree.root, &self.config.index)?;
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        transaction.execute_batch("CREATE TEMP TABLE seen_files(path TEXT PRIMARY KEY);")?;
        let mut stats = IndexStats::default();
        for relative in paths {
            let (absolute, canonical_relative) = resolve_file(&self.worktree.root, &relative)?;
            if canonical_relative != relative {
                continue;
            }
            let before = Stamp::read(&absolute)?;
            let path = path_key(&relative)?;
            let previous: Option<(String, Stamp)> = transaction
                .query_row(
                    "SELECT hash, modified, byte_length FROM files WHERE path = ?1",
                    [&path],
                    |row| {
                        Ok((
                            row.get(0)?,
                            Stamp {
                                modified: row.get(1)?,
                                length: row.get(2)?,
                            },
                        ))
                    },
                )
                .optional()?;
            if !verify_hashes
                && previous.as_ref().is_some_and(|(_, stamp)| {
                    stamp.modified == before.modified && stamp.length == before.length
                })
            {
                transaction.execute("INSERT INTO seen_files(path) VALUES (?1)", [&path])?;
                stats.files += 1;
                continue;
            }
            let Some(text) = index_text(&absolute)? else {
                continue;
            };
            let after = Stamp::read(&absolute)?;
            if before.modified != after.modified || before.length != after.length {
                bail!(
                    "File changed during indexing; retry: {}",
                    relative.display()
                );
            }
            let hash = digest(text.as_bytes());
            transaction.execute("INSERT INTO seen_files(path) VALUES (?1)", [&path])?;
            if previous.as_ref().map(|(hash, _)| hash.as_str()) != Some(&hash) {
                transaction.execute(
                    "INSERT INTO files(path, hash, modified, byte_length, content) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(path) DO UPDATE SET hash=excluded.hash, modified=excluded.modified,
                        byte_length=excluded.byte_length, content=excluded.content",
                    params![path, hash, after.modified, after.length, text],
                )?;
                stats.updated += 1;
            } else {
                transaction.execute(
                    "UPDATE files SET modified=?2, byte_length=?3 WHERE path=?1",
                    params![path, after.modified, after.length],
                )?;
            }
            stats.files += 1;
        }
        stats.removed = transaction.execute(
            "DELETE FROM files WHERE path NOT IN (SELECT path FROM seen_files)",
            [],
        )?;
        transaction.commit()?;
        Ok(stats)
    }

    pub fn files(&self) -> Result<Vec<FileEntry>> {
        let mut paths = walk_scope(&self.worktree.root, &self.config.index)?;
        paths.extend(walk_scope(&self.worktree.root, &self.config.search)?);
        Ok(paths.into_iter().map(|path| FileEntry { path }).collect())
    }

    /// Explicit, transient library browsing. Never changes the project index.
    pub fn environment_files(&self) -> Result<Vec<FileEntry>> {
        Ok(
            walk_scope_mode(&self.worktree.root, &self.config.search, true)?
                .into_iter()
                .map(|path| FileEntry { path })
                .collect(),
        )
    }

    pub fn search<'a>(
        &self,
        query: &str,
        limit: usize,
        documents: impl Iterator<Item = &'a Document>,
    ) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, limit, documents, |_| true)
    }

    /// Apply path filters before limiting matches, including dirty overlays.
    pub fn search_filtered<'a>(
        &self,
        query: &str,
        limit: usize,
        documents: impl Iterator<Item = &'a Document>,
        allow_path: impl Fn(&Path) -> bool,
    ) -> Result<Vec<SearchHit>> {
        self.search_filtered_with_environments(query, limit, documents, allow_path, false)
    }

    /// Opt-in library search reads files on demand, without indexing them.
    pub fn search_filtered_with_environments<'a>(
        &self,
        query: &str,
        limit: usize,
        documents: impl Iterator<Item = &'a Document>,
        allow_path: impl Fn(&Path) -> bool,
        include_environments: bool,
    ) -> Result<Vec<SearchHit>> {
        self.search_filtered_with_environments_streaming(
            query,
            limit,
            documents,
            allow_path,
            include_environments,
            |_| {},
        )
    }

    /// Search like [`Self::search_filtered_with_environments`], publishing each
    /// non-empty group of matches as soon as a file has been inspected.
    pub fn search_filtered_with_environments_streaming<'a>(
        &self,
        query: &str,
        limit: usize,
        documents: impl Iterator<Item = &'a Document>,
        allow_path: impl Fn(&Path) -> bool,
        include_environments: bool,
        mut publish: impl FnMut(&[SearchHit]),
    ) -> Result<Vec<SearchHit>> {
        if query.is_empty() {
            bail!("Search query must not be empty");
        }
        if query.contains('\0') {
            bail!("Search query cannot contain a NUL character");
        }
        if query.len() > 4096 {
            bail!("Search query is too long");
        }
        let limit = limit.min(1000);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut searchable = walk_scope(&self.worktree.root, &self.config.search)?;
        if include_environments {
            searchable.extend(walk_scope_mode(
                &self.worktree.root,
                &self.config.search,
                true,
            )?);
        }
        let indexable = walk_scope(&self.worktree.root, &self.config.index)?;
        let overlays: BTreeMap<_, _> = documents
            .filter(|document| {
                document.kind == crate::DocumentKind::Source
                    && document.dirty
                    && document.worktree_id == self.worktree.id
            })
            .map(|document| (document.path.clone(), document))
            .collect();
        for relative in overlays.keys() {
            if searchable.contains(relative) || self.worktree.root.join(relative).try_exists()? {
                continue;
            }
            let (_, validated) = resolve_buffer_path(&self.worktree.root, relative)?;
            if validated == *relative
                && ((include_environments
                    && crate::project::is_python_environment_path(&self.worktree.root, relative))
                    || self
                        .config
                        .search
                        .allows_missing_path(&self.worktree.root, relative)?)
            {
                searchable.insert(relative.clone());
            }
        }
        let mut stamps = BTreeMap::new();
        let mut candidates = BTreeMap::new();
        if self.path.exists() {
            let mut connection = self.connection()?;
            let transaction = connection.transaction()?;
            {
                let mut statement =
                    transaction.prepare("SELECT path, modified, byte_length FROM files")?;
                for row in statement.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, u64>(2)?,
                    ))
                })? {
                    let (path, modified, length) = row?;
                    stamps.insert(PathBuf::from(path), Stamp { modified, length });
                }
                // A quoted FTS phrase preserves punctuation and cannot inject MATCH operators.
                // Verify candidate contents literally so the public contract stays exact.
                let use_trigrams = query.chars().count() >= 3;
                let (sql, parameter) = if use_trigrams {
                    ("SELECT files.path, files.content FROM files JOIN code_search ON code_search.rowid=files.rowid
                      WHERE code_search MATCH ?1 ORDER BY files.path", format!("\"{}\"", query.replace('"', "\"\"")))
                } else {
                    (
                        "SELECT path, content FROM files WHERE instr(content, ?1) > 0 ORDER BY path",
                        query.to_owned(),
                    )
                };
                let mut statement = transaction.prepare(sql)?;
                for row in statement.query_map([parameter], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })? {
                    let (path, text) = row?;
                    if text.contains(query) {
                        candidates.insert(PathBuf::from(path), text);
                    }
                }
            }
            transaction.commit()?;
        }
        let mut hits = Vec::new();
        for relative in searchable {
            if !allow_path(&relative) {
                continue;
            }
            let previous_len = hits.len();
            if let Some(document) = overlays.get(&relative) {
                collect_hits(&relative, &document.text, query, limit, &mut hits);
            } else {
                let (absolute, canonical_relative) = resolve_file(&self.worktree.root, &relative)?;
                if canonical_relative != relative {
                    continue;
                }
                let current = Stamp::read(&absolute)?;
                let fresh = indexable.contains(&relative)
                    && stamps.get(&relative).is_some_and(|stamp| {
                        stamp.modified == current.modified && stamp.length == current.length
                    });
                if fresh {
                    if let Some(text) = candidates.get(&relative) {
                        collect_hits(&relative, text, query, limit, &mut hits);
                    }
                } else if let Some(text) = index_text(&absolute)? {
                    collect_hits(&relative, &text, query, limit, &mut hits);
                }
            }
            if hits.len() > previous_len {
                publish(&hits[previous_len..]);
            }
            if hits.len() >= limit {
                break;
            }
        }
        Ok(hits)
    }
}

fn path_key(path: &Path) -> Result<String> {
    let value = path.to_str().context("Source paths must be valid UTF-8")?;
    Ok(if cfg!(windows) {
        value.replace('\\', "/")
    } else {
        value.to_owned()
    })
}

fn index_text(path: &Path) -> Result<Option<String>> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_TEXT_BYTES {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES || bytes.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn walk_scope(root: &Path, scope: &ScopeConfig) -> Result<BTreeSet<PathBuf>> {
    walk_scope_mode(root, scope, false)
}

fn walk_scope_mode(
    root: &Path,
    scope: &ScopeConfig,
    environments_only: bool,
) -> Result<BTreeSet<PathBuf>> {
    if !root.is_dir() {
        bail!("Worktree is no longer accessible: {}", root.display());
    }
    let matcher = scope.matcher()?;
    let canonical_root = root.canonicalize()?;
    if canonical_root != root {
        bail!("Worktree location changed; reopen the project");
    }
    let filter_root = root.to_owned();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        .parents(false)
        .ignore(scope.respect_gitignore && !environments_only)
        .git_ignore(scope.respect_gitignore && !environments_only)
        .git_global(false)
        .git_exclude(scope.respect_gitignore && !environments_only)
        .require_git(false)
        .max_filesize(Some(MAX_TEXT_BYTES));
    builder.filter_entry(move |entry| {
        let Ok(relative) = entry.path().strip_prefix(&filter_root) else {
            return false;
        };
        let in_environment = environments_only
            && (crate::project::is_python_environment_path(&filter_root, relative)
                || (entry.file_type().is_some_and(|kind| kind.is_dir())
                    && crate::project::is_environment_directory(entry.path())));
        if internal_path(relative) || (!in_environment && matcher.is_match(relative)) {
            return false;
        }
        if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
            return false;
        }
        // Prune before descending: never enumerate/hash dependencies copied into
        // each worktree, even when gitignore is disabled or exclusions are empty.
        if !environments_only
            && entry.file_type().is_some_and(|kind| kind.is_dir())
            && crate::project::is_environment_directory(entry.path())
        {
            return false;
        }
        // Reject junction directories before the walker descends into them.
        if let Ok(absolute) = entry.path().canonicalize()
            && !absolute.starts_with(&filter_root)
        {
            return false;
        }
        true
    });
    let mut paths = BTreeSet::new();
    for entry in builder.build() {
        let entry = entry.context("Cannot traverse worktree; previous index was preserved")?;
        if entry
            .file_type()
            .is_some_and(|kind| kind.is_file() && !kind.is_symlink())
        {
            let relative = entry.path().strip_prefix(root)?.to_owned();
            if environments_only && !crate::project::is_python_environment_path(root, &relative) {
                continue;
            }
            // This also checks Windows junctions and other reparse points.
            let absolute = entry.path().canonicalize()?;
            if !absolute.starts_with(root) {
                continue;
            }
            if internal_path(absolute.strip_prefix(root)?) {
                continue;
            }
            path_key(&relative)?;
            paths.insert(relative);
        }
    }
    Ok(paths)
}

fn collect_hits(path: &Path, text: &str, query: &str, limit: usize, hits: &mut Vec<SearchHit>) {
    let mut line = 1;
    let mut line_start = 0;
    let mut scanned_to = 0;
    for (offset, _) in text.match_indices(query) {
        let segment = &text[scanned_to..offset];
        line += segment.bytes().filter(|byte| *byte == b'\n').count();
        if let Some(last_newline) = segment.rfind('\n') {
            line_start = scanned_to + last_newline + 1;
        }
        scanned_to = offset;
        let column = text[line_start..offset].chars().count() + 1;
        let content = text[line_start..]
            .split('\n')
            .next()
            .unwrap_or_default()
            .trim_end_matches('\r');
        let start = column.saturating_sub(81);
        let mut preview: String = content.chars().skip(start).take(240).collect();
        if start > 0 {
            preview.insert(0, '…');
        }
        if content.chars().count() > start + 240 {
            preview.push('…');
        }
        hits.push(SearchHit {
            path: path.to_owned(),
            line,
            column,
            preview,
        });
        if hits.len() >= limit {
            return;
        }
    }
}
