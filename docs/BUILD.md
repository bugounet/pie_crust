# Build pie_crust

pie_crust uses the same egui interface code on macOS and Windows. The core and MCP server do not depend on the windowing system. The default renderer is wgpu; an OpenGL variant is available with the `renderer-glow` feature.

## Windows

Install Rust with rustup and the Visual Studio Build Tools C++ tools with the Windows SDK. From the project root:

```powershell
cargo build --release --locked
.\target\release\crusty.exe E:\chemin\vers\projet
```

To make `crusty` available from the shell:

```powershell
.\scripts\install-windows.ps1
crusty E:\chemin\vers\projet
```

The script copies the executable to `%LOCALAPPDATA%\pie_crust\bin` and adds this folder to the user PATH. Open a new terminal, or restart Windows Terminal if it retains the old environment. For a development build, pass `-Executable .\target\debug\crusty.exe` to the script.

The binary uses the Windows GUI subsystem, including in debug mode: launching it directly opens no console. `cargo run` naturally keeps the terminal where Cargo was launched. `crusty --help` and argument errors remain available in the calling terminal and redirected output; without available output, a dialog displays them.

The `rust-toolchain.toml` file fixes the Rust version. Application dependencies are fixed by `Cargo.lock`. SQLite is compiled with the project; no separate SQLite installation is required.

### Portable tools in this development environment

On the machine where this project was created, build tools are installed in `.tools/`. The following script configures only its own process and does not change the system PATH:

```powershell
.\scripts\cargo-local.ps1 build --release --locked
.\scripts\cargo-local.ps1 run '--' examples/demo-project
```

It uses Rust `1.98.1-x86_64-pc-windows-gnu` and GCC-MinGW 16.2.0 / mingw-w64 14.0.0, the WinLibs MSVCRT variant. `.tools/` is local and ignored by Git; another Windows machine can use the standard MSVC installation above directly. The local tool sources are [rustup](https://rust-lang.github.io/rustup/installation/index.html) and [WinLibs](https://github.com/brechtsanders/winlibs_mingw/releases/tag/16.2.0posix-14.0.0-msvcrt-r1).

With this PowerShell script, keep the quotes around `'--'` to pass the separator to Cargo. This also applies to `fmt --all '--' --check` and `clippy --workspace --all-targets --locked '--' -D warnings`.

## macOS

Install the Xcode Command Line Tools and Rust with rustup. From the project root:

```sh
xcode-select --install
cargo build --release --locked
./target/release/crusty /chemin/vers/projet
```

The build uses the Mac's architecture: Apple Silicon or Intel. To produce an application that can be opened in Finder:

```sh
bash scripts/package-macos.sh
open dist/pie_crust.app
```

The resulting bundle is intended for local development; distribution signing and notarization are not configured. The project-opening field in the application lets you select a project when launching from Finder does not pass a path.

Building a macOS binary requires the Apple SDK and tools. The provided workflow builds separately on Windows, Apple Silicon Mac, and Intel Mac; it does not claim to produce a native macOS binary from this Windows machine.

## Verification and renderer variant

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked --no-default-features --features renderer-glow
```

Both renderers use the same views and interactions. Shortcuts use Cmd on macOS and Ctrl on Windows. The [CI](../.github/workflows/ci.yml) workflow contains the three-platform matrix and keeps executables/bundles as artifacts. It will run when this project is hosted in a GitHub repository with Actions enabled; its presence in the folder does not constitute macOS validation already being obtained.
