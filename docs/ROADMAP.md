# Roadmap and acceptance recipe

This document describes the product trajectory. The first foundation is implemented: shared Windows/macOS interface, documents and worktrees, separate search/indexing, Markdown preview, and HTTP MCP. The interface adds panes, split editors, drafts, launch configurations, and automatic saving. Integrated Python analysis provides initial completion, typed callers, fixtures, and extract variable; the Packages and Settings pages occupy the central area and terminals retain their shell. P1 through P5 are partially covered; session restoration, continuous watching, a complete LSP, a PTY terminal, and the debugger still need to be integrated in particular. Typed navigation and transformations are not yet exposed through MCP. The [README](../README.md) describes the current scope and [VALIDATION.md](VALIDATION.md) the checks actually run.

## P0 — Remove technical risks

Make the same native Rust interface compile and run on Windows, Intel Mac, and Apple Silicon Mac; test a real editable buffer, Unicode/IME, undo, scrolling, large files, and an HTML panel. In parallel, run ty as an LSP and measure the data available for annotations, callers, rename, and unsaved files. Compare the Pyright semantic adapter only if necessary.

Acceptance recipe: interface choice documented by a prototype; real LSP exchanges; explicitly typed, untyped, and ambiguous caller cases differentiated; preview of an HTML file with relative resources. No performance figure declared without measurement.

## P1 — Shared core and first complete loop

Create the Cargo workspace, document model, snapshots, per-worktree sessions, opening/saving, and history. Define the local registry in `.pie_crust`, the durable identity of each worktree, and its `.pie_crust/indexes/<worktree_id>.sqlite` file. Add an interface with tree, tabs, editor, and selection. Add HTTP MCP with authentication, `workspace_list`, `workspace_focus`, `editor_focus`, and document reading.

Acceptance recipe: an independent MCP client negotiates the protocol, lists tools, opens a file, and selects a line in the window; the client also reads an unsaved modified buffer. Two worktrees containing the same relative path never exchange buffers. Switching preserves modifications. A concurrent external write is not overwritten.

## P2 — Fast reading and search

Add a persistent catalog, path and content indexes, literal/regex search, symbol search, and incremental background indexing. Add independent exclusions and a freshness indicator. Integrate syntax highlighting, completion, signature help, diagnostics, and go-to-definition.

Acceptance recipe: creation, deletion, renaming, and external modification update results; a restart recovers a consistent index; all four search/indexing combinations work. Open buffers take precedence over disk. Stale results do not move the cursor to an incorrect range. LSP and search can be canceled without blocking input.

Storage acceptance recipe: two worktrees containing different versions of the same path have two distinct SQLite files under `.pie_crust/indexes/` and independent results. Recreating one leaves the other unchanged. Changing branches preserves worktree identity but invalidates stale data. Two instances do not compete to write the same database without coordination. The IDE never indexes its `.pie_crust` folder. Local artifacts are ignored by Git and recovery buffers are never treated as a disposable index.

## P3 — Typed navigation and first refactoring

Build the annotation graph and resolution adapter. Expose eligible callers in the UI and MCP. Implement rename through a versioned plan, diff, validations, application, and grouped undo.

Acceptance recipe: no false link from merely identical names; import aliases, scopes, methods, stubs, and unsaved code covered; Any/Unknown and dynamic ineligible cases clearly identified. Rename changes only the declaration and eligible uses, without additional search, blocking, or confirmation for untyped uses. One acceptance case deliberately keeps an untyped call that becomes invalid; the corresponding test fails, then the fix and added typing make it eligible for the next refactoring. A rename LSP response proposing edits outside the scope does not apply them. Modifying a file after preparing a plan makes its application be rejected. Two clients attempting concurrent modifications lose no data. Retrying the same application request does not apply it twice.

## P4 — Daily Python loop

Integrate an interactive terminal, paginated Git log, per-worktree interpreter, and launching tests from their name. Add pytest, unittest, Django, and custom runners. Add package management with a preview of changes, synchronization, and solver errors.

Acceptance recipe: function, method, class, and parameterized tests launched from the gutter; custom command using absolute file, relative file, file name, and test name; arguments with spaces or special characters passed unchanged. Subprocess cancellation, progressive output, and exact exit codes. Changing worktrees preserves the cwd of active jobs. Terminal: full-screen program, resize, Unicode, and Ctrl+C. Dependency update consistent across constraints, lockfile, and environment, or an explained failure.

## P5 — Migrations, previews, and debugger

Create the Django/Alembic DAG view, Markdown/HTML preview, and DAP launch/attach configurations. Link test errors, migrations, and stack frames to documents in the correct worktree.

Acceptance recipe: Django with cross-app dependency, two leaves in one application, squash, and dynamic dependency; Alembic with merge, multiple databases, intentional branches, and missing parent. A project with two `version_locations` displays migrations and heads from both directories. Two heads not yet merged are visible in the counter and reachable through “View heads”; after adding a merge, its ancestors are no longer marked as heads. Clicking every node, especially a head, opens the exact file at its declaration in the correct worktree. Distinguish source graph from applied state. Debug a script and module, attach to existing debugpy, try PID, breakpoint, step, variables, detach, Django reloader, and subprocesses. Markdown on a modified buffer, static HTML with assets, and a preview of a real Django URL.

## P6 — Advanced refactorings

Proposed order: positional arguments to kwargs; signature modification and reordering; move; extraction; inline; argument conversions or sorting according to the finalized contract. Each operation states its validity domain and returns a concrete reason when it does not apply.

Acceptance recipe: before/after transformation corpus, preservation of comments and line endings, syntax compilation, type checking, and behavioral tests for eligible transformations. Cases covering side effects, mutable defaults, closures, async, generators, overrides, import cycles, positional-only, keyword-only, `*args`, and `**kwargs`. An unsupported transformation within the typed scope produces a precise rejection. The presence of untyped uses is not a reason for rejection: they remain outside the scope and their breakage is handled in the tests → fix → add typing loop.

## Performance objectives to measure

On a documented reference machine and reproducible corpus, aim for: input without blocking work on the UI thread; warm-index search with first results under 100 ms at p95 for common queries; visual restoration of a warm session under 300 ms; progressive indexing with measured memory limits. These values are initial objectives, not achieved performance.

Compare corpora of approximately 1,000 and 50,000 files, with and without cache, distinguishing time to first result, total time, memory, and invalidation. Include scans without an index and unselective regexes without promising indexed-search latency for them.

## Conditions for replacing PyCharm day to day

All essential functions from the request must be available on representative user projects, with documented limitations judged acceptable. Do not call a product “finished” when a refactoring button, debugger, or MCP only simulates its result.

The first genuinely useful tranche to complete is P1–P3: open a project, read and edit, search, go to typed callers, and rename a function from both the UI and an MCP client. P4–P6 progressively complete day-to-day replacement.
