param(
    [string]$Executable = (Join-Path $PSScriptRoot '../target/release/crusty.exe'),
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA 'pie_crust/bin')
)

$ErrorActionPreference = 'Stop'
$source = (Resolve-Path -LiteralPath $Executable).Path
$destination = [IO.Path]::GetFullPath($InstallDirectory)
New-Item -ItemType Directory -Path $destination -Force | Out-Null
$installed = Join-Path $destination 'crusty.exe'
if ($source -ne $installed) {
    Copy-Item -LiteralPath $source -Destination $installed -Force
}

# Register the native GUI executable itself, so launchers never start a shell.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
$alreadyRegistered = $entries | Where-Object {
    [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\', '/') -ieq $destination.TrimEnd('\', '/')
}
if (-not $alreadyRegistered) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $destination) -join ';'), 'User')
}
if (($env:Path -split ';') -notcontains $destination) {
    $env:Path += [IO.Path]::PathSeparator + $destination
}

# Let Explorer and newly opened terminals refresh their environment.
if (-not ('PieCrust.EnvironmentNotification' -as [type])) {
    Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace PieCrust {
    public static class EnvironmentNotification {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr window, uint message, UIntPtr wParam, string lParam,
            uint flags, uint timeout, out UIntPtr result);
    }
}
'@
}
$notificationResult = [UIntPtr]::Zero
[void][PieCrust.EnvironmentNotification]::SendMessageTimeout(
    [IntPtr]0xffff, 0x001a, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$notificationResult
)
Write-Output "Installed: $installed"
Write-Output 'Run crusty [PROJECT] from a new terminal (restart an existing terminal app if needed).'
