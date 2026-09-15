# Uses the optional toolchain installed under .tools without changing the system.
$CargoArguments = $args
$ErrorActionPreference = 'Stop'
$projectDirectory = Split-Path -Parent $PSScriptRoot
$localCargo = Join-Path $projectDirectory '.tools/cargo/bin/cargo.exe'
$localCompiler = Join-Path $projectDirectory '.tools/mingw64/bin'
if (-not (Test-Path -LiteralPath $localCargo)) {
    throw 'Local Rust toolchain not found. Install Rust normally and use cargo, or see docs/BUILD.md.'
}
if (-not (Test-Path -LiteralPath (Join-Path $localCompiler 'gcc.exe'))) {
    throw 'Local GCC-MinGW toolchain not found. See docs/BUILD.md.'
}
$pie_crustEnvironmentKeys = @(
    'CARGO_HOME', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN', 'PATH',
    'CC_x86_64_pc_windows_gnu', 'AR_x86_64_pc_windows_gnu',
    'CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER'
)
$pie_crustPreviousEnvironment = @{}
foreach ($pie_crustKey in $pie_crustEnvironmentKeys) {
    $pie_crustPreviousEnvironment[$pie_crustKey] = [Environment]::GetEnvironmentVariable($pie_crustKey, 'Process')
}
$env:CARGO_HOME = Join-Path $projectDirectory '.tools/cargo'
$env:RUSTUP_HOME = Join-Path $projectDirectory '.tools/rustup'
$env:RUSTUP_TOOLCHAIN = '1.98.1-x86_64-pc-windows-gnu'
$env:PATH = $localCompiler + [IO.Path]::PathSeparator + (Join-Path $projectDirectory '.tools/cargo/bin') + [IO.Path]::PathSeparator + $env:PATH
$env:CC_x86_64_pc_windows_gnu = Join-Path $localCompiler 'gcc.exe'
$env:AR_x86_64_pc_windows_gnu = Join-Path $localCompiler 'ar.exe'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = Join-Path $localCompiler 'gcc.exe'
Push-Location -LiteralPath $projectDirectory
try {
    & $localCargo @CargoArguments
    exit $LASTEXITCODE
} finally {
    Pop-Location
    foreach ($pie_crustKey in $pie_crustEnvironmentKeys) {
        [Environment]::SetEnvironmentVariable($pie_crustKey, $pie_crustPreviousEnvironment[$pie_crustKey], 'Process')
    }
}
