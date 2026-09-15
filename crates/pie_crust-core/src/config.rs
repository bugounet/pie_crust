use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct ScopeConfig {
    pub respect_gitignore: bool,
    pub exclude: Vec<String>,
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self {
            respect_gitignore: true,
            exclude: vec![
                "**/.venv/**".into(),
                "**/__pycache__/**".into(),
                "**/.pytest_cache/**".into(),
                "**/node_modules/**".into(),
                "**/target/**".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Config {
    pub search: ScopeConfig,
    pub index: ScopeConfig,
}

impl Config {
    fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text).context("Invalid configuration structure")?;
        config
            .search
            .matcher()
            .context("Invalid search exclusions")?;
        config.index.matcher().context("Invalid index exclusions")?;
        Ok(config)
    }

    pub fn load(project_root: &Path) -> Result<Self> {
        let path = project_root.join(".pie_crust/config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::parse(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("Invalid configuration in {}", path.display()))
    }
}

/// Checks the configuration structure and compiles both exclusion scopes
/// without writing a file or starting an index operation.
pub fn validate_config_text(text: &str) -> Result<()> {
    Config::parse(text).map(|_| ())
}

impl ScopeConfig {
    pub fn matcher(&self) -> Result<GlobSet> {
        let mut builder = GlobSetBuilder::new();
        for pattern in &self.exclude {
            let pattern = pattern.trim_start_matches('/').trim_end_matches('/');
            let base = pattern.strip_suffix("/**").unwrap_or(pattern);
            for expanded in [base.to_owned(), format!("{base}/**")] {
                builder.add(
                    GlobBuilder::new(&expanded)
                        .literal_separator(true)
                        .case_insensitive(cfg!(windows))
                        .build()
                        .with_context(|| format!("Invalid exclusion glob: {pattern}"))?,
                );
            }
        }
        builder.build().context("Cannot compile exclusion rules")
    }

    /// Applies traversal rules to an unsaved buffer whose disk entry is gone.
    /// Evaluate each ancestor first, so a child negation cannot reopen an excluded directory.
    pub fn allows_missing_path(&self, root: &Path, relative: &Path) -> Result<bool> {
        if crate::project::is_python_environment_path(root, relative) {
            return Ok(false);
        }
        let matcher = self.matcher()?;
        let mut rules = BufferIgnoreRules::default();
        if self.respect_gitignore {
            rules.add_directory(root)?;
        }
        let components: Vec<_> = relative.components().collect();
        let mut absolute = root.to_owned();
        for (index, component) in components.iter().enumerate() {
            absolute.push(component.as_os_str());
            let path = absolute.strip_prefix(root)?;
            let is_directory = index + 1 != components.len();
            if crate::document::internal_path(path) || matcher.is_match(path) {
                return Ok(false);
            }
            if self.respect_gitignore && rules.is_ignored(&absolute, is_directory) {
                return Ok(false);
            }
            if self.respect_gitignore && is_directory {
                rules.add_directory(&absolute)?;
            }
        }
        Ok(true)
    }
}

#[derive(Default)]
struct BufferIgnoreRules {
    ignore: Vec<Gitignore>,
    gitignore: Vec<Gitignore>,
    exclude: Vec<Gitignore>,
}

impl BufferIgnoreRules {
    fn add_directory(&mut self, directory: &Path) -> Result<()> {
        add_ignore_file(&mut self.ignore, directory, &directory.join(".ignore"))?;
        let dot_git = directory.join(".git");
        if dot_git.try_exists()? {
            // Like ignore::WalkBuilder, stop inherited Git rules at nested repositories.
            self.gitignore.clear();
            self.exclude.clear();
            let git_directory = if dot_git.is_dir() {
                dot_git
            } else {
                let contents = std::fs::read_to_string(&dot_git)?;
                let value = contents
                    .trim_end_matches(['\r', '\n'])
                    .strip_prefix("gitdir: ")
                    .context("Invalid .git worktree pointer")?;
                directory.join(value)
            };
            let common_path = git_directory.join("commondir");
            let common = if common_path.try_exists()? {
                git_directory
                    .join(std::fs::read_to_string(common_path)?.trim_end_matches(['\r', '\n']))
            } else {
                git_directory
            };
            add_ignore_file(&mut self.exclude, directory, &common.join("info/exclude"))?;
        }
        add_ignore_file(
            &mut self.gitignore,
            directory,
            &directory.join(".gitignore"),
        )?;
        Ok(())
    }

    fn is_ignored(&self, path: &Path, is_directory: bool) -> bool {
        // .ignore overrides Git rules; within each category the nearest directory wins.
        for category in [&self.ignore, &self.gitignore, &self.exclude] {
            for matcher in category.iter().rev() {
                let result = matcher.matched(path, is_directory);
                if !result.is_none() {
                    return result.is_ignore();
                }
            }
        }
        false
    }
}

fn add_ignore_file(target: &mut Vec<Gitignore>, directory: &Path, path: &Path) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    if !path.is_file() {
        bail!("Ignore rules must be a file: {}", path.display());
    }
    let mut builder = GitignoreBuilder::new(directory);
    if let Some(error) = builder.add(path) {
        return Err(error).context("Cannot read ignore rules for unsaved buffer");
    }
    target.push(builder.build()?);
    Ok(())
}
