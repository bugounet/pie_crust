use crate::document::digest;
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub branch: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RegistryEntry {
    id: String,
    identity: String,
    root: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    schema_version: u32,
    worktrees: Vec<RegistryEntry>,
}

struct Discovered {
    root: PathBuf,
    branch: Option<String>,
    bare: bool,
}

fn git(root: &Path, args: &[&str]) -> std::io::Result<Output> {
    let mut command = Command::new("git");
    command.arg("-C").arg(root).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command.output()
}

fn parse_worktrees(bytes: &[u8]) -> Result<Vec<Discovered>> {
    let mut entries = Vec::new();
    let mut current: Option<Discovered> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        let value = std::str::from_utf8(field).context("Worktree paths must be valid UTF-8")?;
        if let Some(path) = value.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(Discovered {
                root: PathBuf::from(path),
                branch: None,
                bare: false,
            });
        } else if let Some(entry) = current.as_mut() {
            if let Some(branch) = value.strip_prefix("branch ") {
                entry.branch = Some(
                    branch
                        .strip_prefix("refs/heads/")
                        .unwrap_or(branch)
                        .to_owned(),
                );
            } else if value == "bare" {
                entry.bare = true;
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    Ok(entries)
}

pub(crate) fn discover(root: &Path) -> Result<(PathBuf, Vec<WorktreeInfo>, String)> {
    let opened_root = root
        .canonicalize()
        .with_context(|| format!("Cannot open project {}", root.display()))?;
    if !opened_root.is_dir() {
        bail!("Project root must be a directory");
    }
    let git_result = git(&opened_root, &["worktree", "list", "--porcelain", "-z"]);
    let mut discovered = match git_result {
        Ok(output) if output.status.success() => parse_worktrees(&output.stdout)?,
        _ => vec![Discovered {
            root: opened_root.clone(),
            branch: None,
            bare: false,
        }],
    };
    discovered.retain(|entry| !entry.bare && entry.root.is_dir());
    if discovered.is_empty() {
        bail!("The repository has no accessible working tree");
    }
    for entry in &mut discovered {
        entry.root = entry.root.canonicalize()?;
    }
    // Git emits its main worktree first. A bare repository uses its first checkout.
    let project_root = discovered[0].root.clone();
    let storage = project_root.join(".pie_crust");
    fs::create_dir_all(storage.join("indexes"))?;
    let registry_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(storage.join("worktrees.lock"))?;
    registry_lock
        .lock_exclusive()
        .context("Cannot lock the worktree registry")?;
    let registry_path = storage.join("worktrees.json");
    let mut registry: Registry = if registry_path.exists() {
        serde_json::from_slice(&fs::read(&registry_path)?)
            .context("Invalid .pie_crust/worktrees.json; it was preserved")?
    } else {
        Registry {
            schema_version: 1,
            worktrees: Vec::new(),
        }
    };
    if registry.schema_version != 1 {
        bail!(
            "Unsupported worktree registry schema: {}",
            registry.schema_version
        );
    }
    let mut known_ids = BTreeSet::new();
    let mut known_identities = BTreeSet::new();
    for entry in &registry.worktrees {
        validate_id(&entry.id)?;
        if !known_ids.insert(&entry.id) || !known_identities.insert(&entry.identity) {
            bail!("Duplicate worktree identity in registry; no index was opened");
        }
    }
    let mut worktrees = Vec::new();
    let mut assigned_ids = BTreeSet::new();
    for entry in discovered {
        let identity = match git(&entry.root, &["rev-parse", "--absolute-git-dir"]) {
            Ok(output) if output.status.success() => {
                let text = std::str::from_utf8(&output.stdout)?.trim_end_matches(['\r', '\n']);
                format!("git:{}", Path::new(text).canonicalize()?.to_string_lossy())
            }
            _ => format!("directory:{}", entry.root.to_string_lossy()),
        };
        let id = if let Some(known) = registry
            .worktrees
            .iter_mut()
            .find(|known| known.identity == identity)
        {
            known.identity.clone_from(&identity);
            known.root.clone_from(&entry.root);
            known.id.clone()
        } else {
            let id = format!("wt-{}", &digest(identity.as_bytes())[..24]);
            if registry.worktrees.iter().any(|known| known.id == id) {
                bail!("Worktree identifier collision in registry; no index was opened");
            }
            registry.worktrees.push(RegistryEntry {
                id: id.clone(),
                identity,
                root: entry.root.clone(),
            });
            id
        };
        // Registry identifiers are used as filenames; never trust editable JSON here.
        validate_id(&id)?;
        if !assigned_ids.insert(id.clone()) {
            bail!("Multiple worktrees have the same identifier");
        }
        let name = entry
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        worktrees.push(WorktreeInfo {
            id,
            name,
            root: entry.root,
            branch: entry.branch,
        });
    }
    let active = worktrees
        .iter()
        .filter(|entry| opened_root.starts_with(&entry.root))
        .max_by_key(|entry| entry.root.components().count())
        .unwrap_or(&worktrees[0])
        .id
        .clone();
    let mut temporary = tempfile::NamedTempFile::new_in(&storage)?;
    serde_json::to_writer_pretty(&mut temporary, &registry)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&registry_path)
        .context("Cannot save worktree registry")?;
    Ok((project_root, worktrees, active))
}

fn validate_id(id: &str) -> Result<()> {
    if !id.starts_with("wt-")
        || id.len() < 4
        || id.len() > 80
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("Invalid worktree identifier in registry");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_output_handles_spaces_and_newlines() {
        let entries = parse_worktrees(
            b"worktree /a b\nnext\0HEAD abc\0branch refs/heads/main\0\0worktree /c\0detached\0\0",
        )
        .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].root, Path::new("/a b\nnext"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].branch, None);
    }
}
