//! Read-only inspection: cargo run -p pie_crust-core --example discover_project -- <root>

use anyhow::{Context, Result};
use pie_crust_core::{PythonProjectLayout, discover_python_environment};
use std::path::PathBuf;

fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("Expected a worktree path")?,
    );
    let layout = PythonProjectLayout::discover(&root)?;
    println!(
        "Primary source root: {}",
        layout.primary_source_root.display()
    );
    println!("Manifests:");
    for manifest in &layout.manifests {
        println!("  {}", manifest.display());
    }
    println!("Source roots:");
    for source_root in &layout.source_roots {
        println!("  {}", source_root.display());
    }
    match discover_python_environment(&root, &layout.primary_source_root) {
        Some(environment) => {
            println!("Environment: {}", environment.root.display());
            println!("Interpreter: {}", environment.interpreter.display());
        }
        None => println!("Environment: none found"),
    }
    Ok(())
}
