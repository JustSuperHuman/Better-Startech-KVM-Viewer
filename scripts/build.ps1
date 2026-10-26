param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Release',
    [switch]$Test
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$manifest = Join-Path $root 'Cargo.toml'
$installed = & rustup target list --installed
if ($LASTEXITCODE -ne 0) { throw 'Could not inspect Rust targets.' }
foreach ($target in @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc')) {
    if ($target -notin $installed) { throw "Missing Rust target. Run: rustup target add $target" }
}
$profile = @()
if ($Configuration -eq 'Release') { $profile = @('--release') }
& cargo build --locked --manifest-path $manifest -p cart-device-helper --target i686-pc-windows-msvc @profile
if ($LASTEXITCODE -ne 0) { throw '32-bit device helper build failed.' }
& cargo build --locked --manifest-path $manifest -p cart-viewer --target x86_64-pc-windows-msvc @profile
if ($LASTEXITCODE -ne 0) { throw '64-bit viewer build failed.' }
if ($Test) {
    & cargo test --locked --manifest-path $manifest -p cart-core --all-features --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Core tests failed.' }
    & cargo test --locked --manifest-path $manifest -p cart-device-helper --target i686-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Helper tests failed.' }
    & cargo test --locked --manifest-path $manifest -p cart-viewer --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Viewer tests failed.' }
}
$directory = $Configuration.ToLowerInvariant()
Write-Output "Viewer: $root\target\x86_64-pc-windows-msvc\$directory\cart-viewer.exe"
Write-Output "Helper: $root\target\i686-pc-windows-msvc\$directory\cart-device-helper.exe"
