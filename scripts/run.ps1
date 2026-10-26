param([switch]$Demo)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$profile = 'release'
$viewer = Join-Path $root "target\x86_64-pc-windows-msvc\$profile\cart-viewer.exe"
if (-not (Test-Path -LiteralPath $viewer -PathType Leaf)) {
    $profile = 'debug'
    $viewer = Join-Path $root "target\x86_64-pc-windows-msvc\$profile\cart-viewer.exe"
}
if (-not (Test-Path -LiteralPath $viewer -PathType Leaf)) {
    throw 'Build the viewer first: .\scripts\build.ps1'
}
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = $viewer
$start.WorkingDirectory = $root
$start.UseShellExecute = $false
if ($Demo) {
    $start.ArgumentList.Add('--demo')
} else {
    $helper = Join-Path $root "target\i686-pc-windows-msvc\$profile\cart-device-helper.exe"
    if (-not (Test-Path -LiteralPath $helper -PathType Leaf)) { throw 'Matching device helper is missing. Run .\scripts\build.ps1.' }
    $start.ArgumentList.Add('--helper')
    $start.ArgumentList.Add($helper)
    $start.ArgumentList.Add('--connect')
}
$process = [System.Diagnostics.Process]::Start($start)
Write-Output "Viewer started (PID $($process.Id)). Input is automatic over focused video. F12 switches Control / Navigate."
