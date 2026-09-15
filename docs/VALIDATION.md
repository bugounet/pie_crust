# Validation of pie_crust

## Windows rename and launch — September 15, 2026

- Project copied to `E:\Documents\workspace\pie_crust`, with the cloned Git repository preserved. Components `pie_crust-*`, application `pie_crust`, executable `crusty`, `.pie_crust` storage, and `PIE_CRUST_MCP_TOKEN` variable, with no backward compatibility or automatic migration.
- `cargo test --workspace --locked --offline -- --test-threads=4`: **158 tests passed** (62 core, 86 interface, 3 Windows launch, 7 MCP). Tests requiring an external Python environment remain conditional on their configuration variables.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` and `cargo fmt --all -- --check`: passed. A pre-existing Markdown preview warning was fixed with `clamp`.
- After this fix, the 89 desktop component tests (86 interface and 3 Windows launch) were rerun successfully on the final binary.
- The three new tests read the PE subsystem of the actual executable and launch its help and error options with redirected output without a console. The test program remains a console application; the actual binary uses the Windows GUI subsystem, including in debug mode.
- User installation through `scripts/install-windows.ps1`, `crusty` command resolution, and help were verified without the Rust tools PATH. The executable requires only Windows system DLLs.
- Installed binary test: creation of the native `pie_crust` window, then normal closure with exit code 0. This startup check does not constitute a complete visual inspection of the IDE.

The build verified here is a Windows x64 GNU development build. Portable tools were copied into the new folder; the old folder's cache was reused for these checks. No new macOS test or release build was performed for this rename.

## Previous validations

Checks performed on September 14, 2026, on Windows x64 with Rust 1.98.1, target `x86_64-pc-windows-gnu`, and GCC-MinGW 16.2.0. Dependencies come from the supplied `Cargo.lock`; the latest checks use the local offline cache.

## Tests run

`cargo test --workspace --locked --offline`: **105 tests passed, no failures**.

| Component | Tests | Main cases |
| --- | ---: | --- |
| Core | 41 | Index, exclusions, filters before limit, Unicode, Windows junctions, worktrees, drafts, and configuration; 17 Python analysis tests: scopes, imports, annotations, ambiguities, fixtures, fixes, and extraction. |
| Interface and commands | 57 | Shortcuts, quick search with line and retained input, completion/undo, fixes, smartdef, Unicode indentation, CRLF, keyboard event order, tab closing and moving, autosave and conflicts, real extraction; Packages page and real persistent shells with keyboard navigation, history, cwd, and distinct variables. |
| MCP | 7 | Unicode splitting, HTTP exchanges over local TCP, protocol with initialization and current protocol, six real tools, reading modified buffers, long-line pagination with version control, authentication, and request limits. |

Interface tests execute egui frames without a native window and verify focus, keyboard events, and document state. They do not constitute a visual inspection of GPU rendering or a Finder test. MCP tests use an HTTP client separate from the server; they do not merely call its functions directly.

A regression also verifies that Ctrl/Cmd+H and Ctrl/Cmd+Enter, reserved for Python analysis, do not modify TOML or Markdown files. Empty paste leaves the selection intact.

The demonstration Python project passes its **3 unittest tests**, without an external Python dependency. The macOS packaging script syntax and TOML files were also checked.

### Theme and contrast fix

After a report of a light Markdown preview on a light background, two regressions were added and remain included in interface tests: arrival of the light system theme at startup followed by theme changes with native scales of 100%, 150%, and 200%; and contrast of text actually emitted by Markdown rendering, including headings and code, on light and dark backgrounds. These checks remain egui tests without a native window.

### New layout and drafts

Tests produce real keyboard and mouse events: activation of the nine tools, opening Ctrl+R without implicit execution, switching between distinct files in two groups, and expanding their parents in the explorer. Simple, column, and row views keep a short `.pyi` draft visible at 22 points without unnecessary content height.

Drafts are reread after reopening the core and changing worktrees. Tests reject name collisions, redirected paths, concurrent saves, and invalid content without overwriting. Modified settings trigger close protection; saving them validates types and exclusions before replacing the file.

Commands use real local processes: current directory, separate output, UTF-8 and fragmented ANSI sequences, standard input, stopping, and exit code. A launch first saves modified sources from the worktree; rejected input or a disk conflict prevents startup. These tests do not validate a PTY terminal or individual pytest/Django result parsing.

## Build and checks

Extract variable is now tested with name suggestions, real modification, version checking, and CRLF preservation. The other global menu operations remain unavailable. Closing with Escape preserves the selection in the second editor. Fix tests reject a proposal prepared on an earlier version of the shared buffer.

Packages tests verify the structured form, Poetry, preserved sections and comments, external conflicts, and autosave after 100 ms. They do not install or update real dependencies. Terminal tests launch local shells, with two independent histories, directories, and variables; ↑/↓/Enter selection returns to the command field.

| Local check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed. |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed, no warnings. |
| `cargo build --locked --offline -p pie_crust-desktop` | Passed with the default wgpu renderer; development executable in `target/debug/pie_crust.exe`. |
| `cargo check -p pie_crust-desktop --no-default-features --features renderer-glow --locked --offline` | Passed for the OpenGL variant. This check does not produce a second executable. |
| Launch `pie_crust.exe --help` | Passed with a PATH limited to Windows folders, without Rust or GCC tools. The executable's direct imports are system DLLs. |

The local build is a development build without release optimization. Launching help verifies program loading; it does not validate window creation or GPU initialization.

## Reproduce locally

With a normal Rust installation:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --locked -p pie_crust-desktop
cargo check -p pie_crust-desktop --no-default-features --features renderer-glow --locked
```

With this folder's portable Windows tools, replace `cargo` with `.\scripts\cargo-local.ps1` and write the `'--'` separator in quotes. Details are in [BUILD.md](BUILD.md).

## Remaining checks on target platforms

The GitHub Actions workflow prepares builds, tests, and artifacts on Windows MSVC, Apple Silicon Mac, and Intel Mac. It has not yet run: this Windows machine does not validate native macOS compilation or execution. The `.app` bundle still needs to be built on a Mac.

A native-window recipe remains necessary on each system: GPU rendering, IME, clipboard, resizing, external terminal, and closing through the system's different entry points. Performance on large repositories has not yet been measured. Missing features are listed in the [README](../README.md) and [roadmap](ROADMAP.md).
