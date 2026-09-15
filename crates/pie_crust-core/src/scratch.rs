use crate::document::{MAX_TEXT_BYTES, digest};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScratchInfo {
    pub name: String,
    /// Relative to the central project root, never a linked worktree root.
    pub path: PathBuf,
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
    {
        bail!("Scratch names must be a single portable filename of at most 128 bytes");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        bail!("Scratch name is reserved on Windows");
    }
    Ok(())
}

pub(crate) fn relative_path(name: &str) -> PathBuf {
    Path::new(".pie_crust").join("scratches").join(name)
}

pub(crate) fn document_id(root: &Path, name: &str) -> String {
    let normalized = if cfg!(windows) {
        name.to_lowercase()
    } else {
        name.to_owned()
    };
    format!(
        "scratch-{}",
        &digest(format!("{}\0{normalized}", root.display()).as_bytes())[..24]
    )
}

/// Refuse redirected storage before creating or opening anything beneath it.
fn directory(root: &Path, create: bool) -> Result<Option<PathBuf>> {
    let storage = root.join(".pie_crust");
    if storage
        .canonicalize()
        .context("Cannot resolve pie_crust storage")?
        != storage
    {
        bail!("Scratch storage cannot use a redirected .pie_crust directory");
    }
    let directory = storage.join("scratches");
    match fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error).context("Cannot create scratch storage"),
            }
        }
        Err(error) => return Err(error).context("Cannot inspect scratch storage"),
    }
    if directory
        .canonicalize()
        .context("Cannot resolve scratch storage")?
        != directory
        || !directory.is_dir()
    {
        bail!("Scratch storage must be a directory directly inside .pie_crust");
    }
    Ok(Some(directory))
}

pub(crate) fn resolve(root: &Path, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let directory = directory(root, false)?.context("Scratch storage does not exist")?;
    let path = directory.join(name);
    let absolute = path
        .canonicalize()
        .with_context(|| format!("Cannot open scratch {name}"))?;
    // Canonical paths can change letter case on Windows. Comparing the parent
    // preserves containment without opening aliases to another scratch file.
    if absolute.parent() != Some(directory.as_path())
        || fs::symlink_metadata(&path)?.file_type().is_symlink()
    {
        bail!("Scratch files cannot be symlinks or escape their storage directory");
    }
    if !absolute.is_file() {
        bail!("Scratch is not a file: {name}");
    }
    Ok(absolute)
}

pub(crate) fn create(root: &Path, name: &str, text: &str) -> Result<PathBuf> {
    validate_name(name)?;
    if text.len() as u64 > MAX_TEXT_BYTES || text.contains('\0') {
        bail!("Unsupported document content");
    }
    let directory = directory(root, true)?.expect("created scratch directory");
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(directory.join(name))
        .with_context(|| {
            format!("Cannot create scratch {name}; an existing file is never overwritten")
        })?;
    resolve(root, name)
}

pub(crate) fn list(root: &Path) -> Result<Vec<ScratchInfo>> {
    let Some(directory) = directory(root, false)? else {
        return Ok(Vec::new());
    };
    let mut scratches = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if validate_name(&name).is_err() || resolve(root, &name).is_err() {
            continue;
        }
        if entry.metadata()?.len() > MAX_TEXT_BYTES {
            continue;
        }
        scratches.push(ScratchInfo {
            path: relative_path(&name),
            name,
        });
    }
    scratches.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(scratches)
}
