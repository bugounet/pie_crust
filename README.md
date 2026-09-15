# pie_crust

The first foundation of a native Rust IDE for Python. The egui interface shares the same code on Windows and macOS, with a dark theme by default; wgpu provides the default renderer, with OpenGL available as an option. The core and MCP server are independent of the interface.

## Getting started

The application is called **pie_crust** and its executable is **crusty**:

```sh
crusty
crusty /chemin/vers/projet
crusty --help
```

On Windows, `crusty.exe` opens only the IDE window, without an empty console, in both development and release builds. Help and launch errors use the calling terminal or redirected output; without a terminal, a dialog displays them. To install the command in the user PATH after compiling, run `./scripts/install-windows.ps1`, then open a new terminal. On macOS, `cargo install --path crates/pie_crust-desktop --locked` installs the command.

Local storage is now named `.pie_crust`, and the MCP token uses `PIE_CRUST_MCP_TOKEN`. There is no backward compatibility or automatic migration from the old name.

With Rust and the platform build tools installed:

```sh
cargo run -- examples/demo-project
```

On Windows, with the portable tools already prepared in this folder:

```powershell
.\scripts\cargo-local.ps1 run '--' examples/demo-project
```

Pass another project path instead of `examples/demo-project`, or use the open field in the application. Without an argument, pie_crust displays a welcome screen for choosing the project folder. [Windows, Intel Mac, and Apple Silicon build instructions](docs/BUILD.md).

## Available in this first version

- Opening a project or Git worktree; selecting other worktrees in the repository. Preparation runs in the background with an animated loader, the current step, and elapsed time. Indexing remains visible in the empty editor and status bar, which also reports Python analysis.
- Left toolbar, collapsible panes, central editor, and bottom output areas. Tab actions split the editor horizontally or vertically; tabs can be dragged between the two groups. The Layout button has been removed.
- Tree explorer that selects, expands, and reveals the active editor file. This tracking can be disabled in settings.
- UTF-8 editing, line numbers, and Python syntax highlighting, including `.pyi` files. Detected indentation (tabs, two or four spaces) and insertions respect LF/CRLF line endings. Automatic saving after 100 ms of inactivity; Ctrl+S/Cmd+S remains available. A disk conflict preserves the input and interrupts saving.
- Case-sensitive literal search with a SQLite FTS5 trigram index, regex filter on the file name, and relative-folder filter. Filters apply before the result limit. Results open the file at the matching line and column; modified buffers take precedence over disk.
- A distinct `.pie_crust/indexes/<worktree_id>.sqlite` file for each worktree. The `.pie_crust/worktrees.json` registry preserves identities independently of branch names. Reindexing reconciles additions, modifications, and deletions.
- Independent search and indexing exclusions. A file excluded from the index can remain searchable by on-demand scan. `.pie_crust` and `.git` are excluded from sources.
- Persistent drafts in `.pie_crust/scratches/`, specific to the project and shared between its worktrees. These free-form files, especially Python files, open in the editor and remain outside search and indexing.
- Markdown preview and textual Git log. The Packages page occupies the central area: project name, version, required Python, dependencies, scripts, and complete TOML. Preserved sections and comments remain editable. Valid forms are saved after 100 ms of inactivity; invalid or conflicting drafts remain visible.
- Python name completion, highlighting occurrences of the selected symbol, suggested fixes, typed call hierarchy, and pytest fixtures for the targeted function, based on a Python parser integrated in Rust.
- Local view of Django and Alembic migrations: relationships between migrations, merges, and heads not yet merged; clicking a migration or its parent opens its file. Multiple heads are not automatically presented as a conflict. Dynamic expressions and unresolved references are reported as a partial graph.
- Running commands and tests, streaming output, standard-input entry, closing that input (EOF), and stopping processes. An external system terminal remains available from the Terminal pane.
- Authenticated local HTTP MCP server: listing worktrees, requesting focus, reading buffers, searching, and reindexing.

## Navigation and execution

Clicking an icon in the left toolbar or using its shortcut opens its pane; repeating the action collapses it.

| Shortcut | Pane |
| --- | --- |
| Ctrl+1 | Files |
| Ctrl+2 | Search |
| Ctrl+3 | Packages |
| Ctrl+4 | Migrations |
| Ctrl+5 | Tests |
| Ctrl+6 | Git |
| Ctrl+7 or Ctrl+, | Settings (central page) |
| Ctrl+8 | Drafts |
| Ctrl+9 | Terminals |

The same actions also accept Cmd on Mac. **Ctrl+R** opens the launch window: choose a configuration, edit its command, or enter a new command directly. The buttons in the upper-right launch the selected configuration and stop executions. Saved configurations are kept in `.pie_crust/run-configs.json`.

**Ctrl+Shift+R** (or Cmd+Shift+R on Mac) opens the refactoring menu. **Extract variable** transforms a selected expression, with several proposed names based on its call or return type. The name remains editable and the transformation verifies that the source has not changed. The other menu operations (rename, move, signature change with call adaptation, kwargs/sortedargs, function extraction, and inline) remain disabled.

| Shortcut | Action |
| --- | --- |
| Ctrl+F | Search in file, next/previous occurrences |
| Ctrl+Shift+F | Search project |
| Ctrl+O | Quick file; `billing.py:1234` opens line 1234 |
| Ctrl+L | Choose line number |
| Ctrl+W | Close active page after saving |
| Ctrl+Shift+W | Close other tabs and keep this one |
| Ctrl+Enter / Cmd+Enter | Fixes for the symbol under the cursor |
| Ctrl+H | Static typed call hierarchy |
| Alt+← / Alt+→ | Move the selected parameter in the signature |

In the file browser, a click selects; Enter or double-click opens in the active group. Ctrl+click or Ctrl+Enter opens side by side. Double-clicking `pyproject.toml` opens the Packages page; its **Open TOML** button returns to the text. Right-clicking a folder offers search in that folder.

pie_crust discovers `pyproject.toml` files in subfolders as soon as it opens, independently of index loading and search exclusions. The manifest at the root has priority; otherwise, the one closest to the root is selected, then the path breaks ties. The Packages page can choose another manifest. Its folder becomes a Python source root; `src` folders are also recognized for imports. Without a manifest, `manage.py` serves as a marker. For example, opening `frigo-recettes` automatically detects `back-end/frigo-recettes` and keeps file paths relative to the repository.

New Python, Django, and test configurations use the detected source folder. The system-compatible venv closest to the project is selected based on its Python executable; custom names containing `pyvenv.cfg`, such as `.venv.windows`, are recognized. Previously saved custom commands keep their launch folder. In **Packages**, the detected interpreter is displayed and **Check dependencies** compares the TOML's main `[project]` dependencies with the versions actually installed, using the states compatible, missing, incompatible version, or not applicable. This check reads local metadata without installing a package.

The quick file picker keeps input active while browsing results with ↑/↓: you can continue refining the path or add `:line` before confirming.

## Python editing and terminals

When creating an instance method, the opening parenthesis inserts `self, `; ordinary functions and static methods do not receive `self`. Tab moves through parameters to the return annotation; Shift+Tab returns to the previous parameter or the name. Enter from a definition places the cursor in its body, using the file's indentation. Alt+←/→ changes the local signature; it does not yet rewrite calls.

Completion proposes names visible in the scope and project class members on annotated receivers. Ctrl+Enter proposes, depending on context, an import, a similar name, or creation of a skeleton to complete. Occurrences distinguish variables shadowed in other scopes and ignore comments and strings.

Ctrl+H displays statically resolved callers and calls. Both functions must be explicitly annotated; ambiguous, dynamic, or untyped resolutions are excluded. The fixture view includes parameters, `usefixtures`, `autouse`, fixture dependencies, and ancestor `conftest.py` files. Clicking a known definition opens its source.

Ctrl+9 lists all terminals. ↑/↓ then Enter chooses a session or **New session**, collapses the pane, and places the cursor in its input. Each session keeps its shell, current folder, variables, and history. Its name follows the last command and indicates a running execution. Sessions keep the worktree where they were created.

Common commands are proposed for `unittest`, `pytest`, and Django when `manage.py` is present. Discovery adds up to 64 Python scripts located directly at the project root or in `scripts/`. Their presence in the list does not execute them.

Each launch keeps the selected worktree's folder, even if the worktree is changed afterward. Before starting, pie_crust saves modified documents from that worktree and shared drafts; a save conflict interrupts the launch. The **Terminal**, **Tests**, and **Execution** tabs at the bottom display each command's output, duration, and exit code.

## Connecting an MCP client

In **Settings → MCP Connection**, copy the URL and token. Example configuration to adapt to the client's format:

```json
{
  "url": "http://127.0.0.1:43127/mcp",
  "headers": { "Authorization": "Bearer <jeton copié dans pie_crust>" }
}
```

The transport is MCP Streamable HTTP via the official rmcp SDK. Available tools are `workspace_list`, `workspace_focus`, `editor_focus`, `document_read`, `code_search`, and `index_rebuild`. All tools targeting sources require the worktree identifier. Positions start at 1; columns count Unicode characters.

`document_read` also reads unsaved changes. A truncated read returns `next_byte_offset`; resuming with `byte_offset` and `expected_version` also allows long lines to be read without mixing two document versions. `editor_focus` requests opening and placing the cursor in the interface; it does not guarantee that the operating system brings the window in front of all other applications. `code_search` returns up to 200 results per call. Reads are bounded and report the returned portion.

The token is generated for each launch and is not written to the repository. It can be provided through `PIE_CRUST_MCP_TOKEN` (32 to 512 visible ASCII characters) if the client needs a persistent value. Launch options: `--mcp-port 43127`, `--mcp-port 0` for a free port, `--no-mcp` to disable the server. The server listens only on the loopback interface.

## Configuration and current limitations

Copy [the active example](examples/pie_crust.toml) to `.pie_crust/config.toml` at the project's main root, or edit exclusions from **Settings**. The `search` and `index` sections are validated before saving, including their exclusion patterns. Launch configurations use `.pie_crust/run-configs.json` separately. The other options in [the future example](examples/pie_crust-planned.toml) are not interpreted yet.

Python environments are excluded by default from file and text searches, symbols, signatures, callers, and project refactorings in all worktrees. pie_crust stops traversing `.venv` folders, variants such as `.venv.windows`, `venv`/`virtualenv` folders, and any folder containing `pyvenv.cfg`, even with `exclude = []` or without honoring `.gitignore`. Reindexing removes previously indexed venv files. **Include venvs for this search**, in text search or **Go to file**, allows exceptional access to their files, even when Git ignores them, without adding them to the permanent index or project Python analysis. This option is not saved and resets when changing worktrees; the **Go to file** option resets on each opening. The venv remains usable for commands and the Packages page; a library file opened explicitly remains viewable in the editor.

The index is updated on opening, after saving in the interface, and with the reindex button. Search checks dates and sizes to read from disk if an entry is stale; reindexing revalidates hashes. Continuous watching and automatic tree updates remain to be integrated. Binary, non-UTF-8, or larger-than-2 MiB files are not edited/indexed in this version. Search uses files subject to the same limits.

Sessions, tabs, display settings, and unsaved buffers are not yet restored after a restart. Saved drafts and launch configurations remain on disk. The Git log displays the last 100 commits selected by Git; pagination remains to be integrated.

The integrated console keeps one shell per session and communicates through input and output streams; it does not yet have a pseudo-terminal (PTY). Full-screen interactive applications require the external terminal. The Tests pane displays a command's overall result; it does not yet produce a per-test result tree. Clicking a test name in code and command templates with file/test variables remain to be integrated.

The Packages page searches available versions with pip, or with `uv` if the venv does not contain pip; otherwise, an external pip can drive the selected interpreter through `--python`. No package manager is installed automatically in the venv. It can update a package on explicit request, without automatically synchronizing constraints and lockfiles. The **Execution log** button opens output directly from Packages. The migration graph relies on static analysis of local files within the project scope, without loading Django or Alembic or consulting the database; dynamic cases and some complex graphs remain partial.

Current Python analysis covers declarations and direct/relative imports in the project, with conservative resolution; it does not replace a complete LSP server and does not yet resolve inheritance, dynamic extensions, or all pytest plugins. Proposals may be incomplete during initial analysis. Extract variable rejects partial expressions that would change evaluation order. HTML preview, global refactorings, and the debugger remain to be implemented. Available MCP tools are those listed above; new editing and analysis actions are not yet exposed through MCP.

The confirmed contract for future transformations remains: only uses eligible by typing are modified, without additional searching for untyped uses. Tests reveal any resulting breakage, then the fix adds typing.

## Organization and verification

- `pie_crust-core`: versioned documents, worktrees, configuration, and independent indexes.
- `pie_crust-mcp`: HTTP protocol, authentication, and adaptation of core commands.
- `pie_crust-desktop`: common interface for both systems and background tasks.

```sh
cargo test --workspace --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Tests cover indexes, exclusions, document conflicts, drafts, Git worktrees, editor interactions outside the window, process commands, and HTTP MCP exchanges. The CI matrix includes Windows, Apple Silicon Mac, and Intel Mac; macOS compilation and execution still need to be validated on a Mac. The results actually obtained in this environment are described in [VALIDATION.md](docs/VALIDATION.md).

[Target architecture](docs/ARCHITECTURE.md) · [Roadmap](docs/ROADMAP.md) · [Build](docs/BUILD.md)
