use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub(crate) const MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    #[default]
    Source,
    Scratch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub worktree_id: String,
    pub path: PathBuf,
    pub text: String,
    pub version: u64,
    pub dirty: bool,
    #[serde(default)]
    pub kind: DocumentKind,
    #[serde(skip)]
    disk_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentInfo {
    pub id: String,
    pub worktree_id: String,
    pub path: PathBuf,
    pub version: u64,
    pub dirty: bool,
    #[serde(default)]
    pub kind: DocumentKind,
}

impl From<&Document> for DocumentInfo {
    fn from(value: &Document) -> Self {
        Self {
            id: value.id.clone(),
            worktree_id: value.worktree_id.clone(),
            path: value.path.clone(),
            version: value.version,
            dirty: value.dirty,
            kind: value.kind,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FocusTarget {
    pub document_id: String,
    pub worktree_id: String,
    pub line: usize,
    pub column: usize,
    pub serial: u64,
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn document_id(worktree_id: &str, path: &Path) -> String {
    format!(
        "doc-{}",
        &digest(format!("{worktree_id}\0{}", path.to_string_lossy()).as_bytes())[..24]
    )
}

pub(crate) fn internal_path(path: &Path) -> bool {
    path.components().any(|component| {
        let part = component.as_os_str().to_string_lossy();
        if cfg!(windows) {
            part.eq_ignore_ascii_case(".pie_crust") || part.eq_ignore_ascii_case(".git")
        } else {
            part == ".pie_crust" || part == ".git"
        }
    })
}

pub(crate) fn resolve_file(root: &Path, path: &Path) -> Result<(PathBuf, PathBuf)> {
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let absolute = candidate
        .canonicalize()
        .with_context(|| format!("Cannot open {}", path.display()))?;
    let relative = absolute
        .strip_prefix(root)
        .with_context(|| format!("Path escapes its worktree: {}", path.display()))?
        .to_owned();
    relative
        .to_str()
        .context("Source paths must be valid UTF-8")?;
    if internal_path(&relative) {
        bail!("IDE and Git internal files cannot be opened as source documents");
    }
    if !absolute.is_file() {
        bail!("Not a file: {}", path.display());
    }
    Ok((absolute, relative))
}

/// Resolves a previously opened buffer even if its file or parent was deleted.
/// The nearest existing ancestor is canonicalized to preserve the same boundary.
pub(crate) fn resolve_buffer_path(root: &Path, path: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    };
    let mut missing = Vec::new();
    let mut absolute = loop {
        match candidate.canonicalize() {
            Ok(absolute) => break absolute,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    candidate
                        .file_name()
                        .context("Invalid missing document path")?
                        .to_owned(),
                );
                if !candidate.pop() {
                    bail!("Document path has no existing ancestor");
                }
            }
            Err(error) => return Err(error).context("Cannot resolve document path"),
        }
    };
    if !absolute.starts_with(root) {
        bail!("Path escapes its worktree: {}", path.display());
    }
    for component in missing.into_iter().rev() {
        absolute.push(component);
    }
    let relative = absolute.strip_prefix(root)?.to_owned();
    if internal_path(&relative) {
        bail!("IDE and Git internal files cannot be opened as source documents");
    }
    relative
        .to_str()
        .context("Source paths must be valid UTF-8")?;
    Ok((absolute, relative))
}

pub(crate) fn read_text(path: &Path) -> Result<String> {
    let metadata =
        fs::metadata(path).with_context(|| format!("Cannot inspect {}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_TEXT_BYTES {
        bail!(
            "Only text files up to {} MiB are supported: {}",
            MAX_TEXT_BYTES / 1024 / 1024,
            path.display()
        );
    }
    let bytes = fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        bail!("File grew beyond the text limit");
    }
    if bytes.contains(&0) {
        bail!("Binary file is not supported: {}", path.display());
    }
    String::from_utf8(bytes)
        .with_context(|| format!("Only UTF-8 text is supported: {}", path.display()))
}

impl Document {
    pub(crate) fn load(
        id: String,
        worktree_id: String,
        path: PathBuf,
        absolute: &Path,
    ) -> Result<Self> {
        let text = read_text(absolute)?;
        let disk_hash = digest(text.as_bytes());
        Ok(Self {
            id,
            worktree_id,
            path,
            text,
            version: 1,
            dirty: false,
            kind: DocumentKind::Source,
            disk_hash,
        })
    }

    pub(crate) fn refresh(&mut self, absolute: &Path) -> Result<()> {
        if self.dirty {
            return Ok(());
        }
        let text = read_text(absolute)?;
        let hash = digest(text.as_bytes());
        if self.disk_hash != hash {
            self.text = text;
            self.disk_hash = hash;
            self.version += 1;
        }
        Ok(())
    }

    pub(crate) fn edit(&mut self, expected_version: u64, text: String) -> Result<u64> {
        if self.version != expected_version {
            bail!(
                "Stale document version: expected {expected_version}, current {}",
                self.version
            );
        }
        if text.len() as u64 > MAX_TEXT_BYTES || text.contains('\0') {
            bail!("Unsupported document content");
        }
        if self.text != text {
            self.text = text;
            self.version += 1;
            self.dirty = digest(self.text.as_bytes()) != self.disk_hash;
        }
        Ok(self.version)
    }

    pub(crate) fn save(&mut self, absolute: &Path) -> Result<()> {
        if !self.dirty {
            return self.refresh(absolute);
        }
        self.check_disk(absolute)?;
        let permissions = fs::metadata(absolute)?.permissions();
        let parent = absolute
            .parent()
            .context("Document has no parent directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(self.text.as_bytes())?;
        temporary.as_file().set_permissions(permissions)?;
        temporary.as_file().sync_all()?;
        // Revalidate after the write so a slow disk does not widen the conflict window.
        self.check_disk(absolute)?;
        temporary
            .persist(absolute)
            .with_context(|| format!("Cannot save {}", absolute.display()))?;
        self.disk_hash = digest(self.text.as_bytes());
        self.dirty = false;
        Ok(())
    }

    fn check_disk(&self, absolute: &Path) -> Result<()> {
        let current = read_text(absolute)
            .context("Document changed externally or is no longer readable; buffer retained")?;
        if digest(current.as_bytes()) != self.disk_hash {
            bail!(
                "Document changed externally; save rejected and buffer retained: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}
