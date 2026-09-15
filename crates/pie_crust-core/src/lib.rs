//! Portable workbench state shared by the desktop application and MCP.
//!
//! Paths in public results are relative to their worktree. Positions are
//! one-based Unicode scalar positions. Search is literal and case-sensitive.

mod config;
mod document;
mod index;
mod project;
pub mod python;
mod scratch;
mod worktree;

pub use config::validate_config_text;
pub use document::{Document, DocumentInfo, DocumentKind, FocusTarget};
pub use index::{FileEntry, IndexRequest, IndexStats, SearchHit};
pub use project::{
    PythonEnvironment, PythonProjectLayout, discover_python_environment, is_python_environment_path,
};
pub use python::{PythonSource, PythonWorkspace};
pub use scratch::ScratchInfo;
pub use worktree::WorktreeInfo;

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub type SharedWorkbench = Arc<Mutex<Workbench>>;

pub struct Workbench {
    project_root: PathBuf,
    worktrees: Vec<WorktreeInfo>,
    active_worktree_id: String,
    documents: BTreeMap<String, Document>,
    focus: Option<FocusTarget>,
    focus_serial: u64,
}

impl Workbench {
    pub fn open(root: &Path) -> Result<Self> {
        let (project_root, worktrees, active_worktree_id) = worktree::discover(root)?;
        Ok(Self {
            project_root,
            worktrees,
            active_worktree_id,
            documents: BTreeMap::new(),
            focus: None,
            focus_serial: 0,
        })
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    pub fn worktrees(&self) -> &[WorktreeInfo] {
        &self.worktrees
    }
    pub fn active_worktree_id(&self) -> &str {
        &self.active_worktree_id
    }
    pub fn focus(&self) -> Option<FocusTarget> {
        self.focus.clone()
    }

    fn worktree(&self, id: &str) -> Result<&WorktreeInfo> {
        self.worktrees
            .iter()
            .find(|wt| wt.id == id)
            .with_context(|| format!("Unknown worktree: {id}"))
    }

    pub fn focus_worktree(&mut self, id: &str) -> Result<()> {
        self.worktree(id)?;
        self.active_worktree_id = id.to_owned();
        if self
            .focus
            .as_ref()
            .is_some_and(|focus| focus.worktree_id != id)
        {
            self.focus = None;
        }
        Ok(())
    }

    pub fn read_document(&mut self, wt: &str, path: &Path) -> Result<Document> {
        let root = self.worktree(wt)?.root.clone();
        let (_, buffered_relative) = document::resolve_buffer_path(&root, path)?;
        let buffered_id = document::document_id(wt, &buffered_relative);
        if let Some(document) = self
            .documents
            .get(&buffered_id)
            .filter(|document| document.dirty)
        {
            return Ok(document.clone());
        }
        let (absolute, relative) = document::resolve_file(&root, path)?;
        let id = document::document_id(wt, &relative);
        if let Some(document) = self.documents.get_mut(&id) {
            document.refresh(&absolute)?;
            return Ok(document.clone());
        }
        let document = Document::load(id.clone(), wt.to_owned(), relative, &absolute)?;
        self.documents.insert(id, document.clone());
        Ok(document)
    }

    pub fn document(&self, id: &str) -> Option<&Document> {
        self.documents.get(id)
    }

    /// Lists project-wide scratch files, stored outside every worktree index.
    pub fn list_scratches(&self) -> Result<Vec<ScratchInfo>> {
        scratch::list(&self.project_root)
    }

    /// Creates an immediately persistent scratch without overwriting an existing file.
    pub fn create_scratch(&mut self, name: &str, text: &str) -> Result<Document> {
        scratch::validate_name(name)?;
        if self
            .documents
            .contains_key(&scratch::document_id(&self.project_root, name))
        {
            bail!("Scratch is already open: {name}");
        }
        scratch::create(&self.project_root, name, text)?;
        self.read_scratch(name)
    }

    /// Scratch identity is project-wide. Its worktree is the first-open context
    /// for this session; focusing the scratch does not switch the active worktree.
    pub fn read_scratch(&mut self, name: &str) -> Result<Document> {
        scratch::validate_name(name)?;
        let id = scratch::document_id(&self.project_root, name);
        if let Some(document) = self.documents.get(&id).filter(|document| document.dirty) {
            return Ok(document.clone());
        }
        let absolute = scratch::resolve(&self.project_root, name)?;
        if let Some(document) = self.documents.get_mut(&id) {
            document.refresh(&absolute)?;
            return Ok(document.clone());
        }
        let mut document = Document::load(
            id.clone(),
            self.active_worktree_id.clone(),
            scratch::relative_path(name),
            &absolute,
        )?;
        document.kind = DocumentKind::Scratch;
        self.documents.insert(id, document.clone());
        Ok(document)
    }

    pub fn focus_scratch(&mut self, name: &str, line: usize, column: usize) -> Result<FocusTarget> {
        let document = self.read_scratch(name)?;
        self.focus_loaded_document(document, self.active_worktree_id.clone(), line, column)
    }

    pub fn documents(&self) -> Vec<DocumentInfo> {
        self.documents.values().map(DocumentInfo::from).collect()
    }

    pub fn edit_document(&mut self, id: &str, expected_version: u64, text: String) -> Result<u64> {
        let document = self
            .documents
            .get_mut(id)
            .with_context(|| format!("Unknown document: {id}"))?;
        document.edit(expected_version, text)
    }

    pub fn save_document(&mut self, id: &str) -> Result<()> {
        let document = self
            .documents
            .get(id)
            .with_context(|| format!("Unknown document: {id}"))?;
        let absolute = match document.kind {
            DocumentKind::Source => {
                let root = &self.worktree(&document.worktree_id)?.root;
                document::resolve_file(root, &document.path)?.0
            }
            DocumentKind::Scratch => {
                let name = document
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("Invalid scratch document path")?;
                scratch::resolve(&self.project_root, name)?
            }
        };
        self.documents
            .get_mut(id)
            .expect("document validated")
            .save(&absolute)
    }

    pub fn focus_document(
        &mut self,
        wt: &str,
        path: &Path,
        line: usize,
        column: usize,
    ) -> Result<FocusTarget> {
        let document = self.read_document(wt, path)?;
        self.focus_loaded_document(document, wt.to_owned(), line, column)
    }

    fn focus_loaded_document(
        &mut self,
        document: Document,
        wt: String,
        line: usize,
        column: usize,
    ) -> Result<FocusTarget> {
        if line == 0 || column == 0 {
            bail!("Line and column start at 1");
        }
        let selected_line = document
            .text
            .split('\n')
            .nth(line - 1)
            .with_context(|| format!("Line {line} is outside the document"))?;
        if column > selected_line.trim_end_matches('\r').chars().count() + 1 {
            bail!("Column {column} is outside line {line}");
        }
        self.focus_worktree(&wt)?;
        self.focus_serial += 1;
        let focus = FocusTarget {
            document_id: document.id,
            worktree_id: wt,
            line,
            column,
            serial: self.focus_serial,
        };
        self.focus = Some(focus.clone());
        Ok(focus)
    }

    pub fn index_request(&self, wt: &str) -> Result<IndexRequest> {
        IndexRequest::new(self.worktree(wt)?.clone(), &self.project_root)
    }

    pub fn search(&self, wt: &str, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let request = self.index_request(wt)?;
        request.search(
            query,
            limit,
            self.documents.values().filter(|document| {
                document.kind == DocumentKind::Source && document.worktree_id == wt
            }),
        )
    }

    /// Captures unsaved buffers for a search worker, without holding the UI mutex.
    pub fn search_snapshot(&self, wt: &str) -> Result<(IndexRequest, Vec<Document>)> {
        Ok((
            self.index_request(wt)?,
            self.documents
                .values()
                .filter(|document| {
                    document.kind == DocumentKind::Source
                        && document.worktree_id == wt
                        && document.dirty
                })
                .cloned()
                .collect(),
        ))
    }

    pub fn files(&self, wt: &str) -> Result<Vec<FileEntry>> {
        self.index_request(wt)?.files()
    }
}
