# Native noFF build: one directly runnable StreamCap.exe; never packages Python or user data.
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$desktop = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$env:NO_COLOR = 'true'
$tools = Join-Path $desktop 'target\tools\bin'
if (-not (Test-Path -LiteralPath (Join-Path $tools 'wasm-bindgen.exe'))) {
    throw 'Install the project-local tool first: cargo install wasm-bindgen-cli --version 0.2.128 --locked --root desktop/target/tools'
}
$env:PATH = "$tools;$env:PATH"
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $desktop ('src-tauri\target\native-noFF-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$allowed = [IO.Path]::GetFullPath((Join-Path $desktop 'src-tauri\target')) + [IO.Path]::DirectorySeparatorChar
if (-not $OutputDirectory.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase)) { throw 'Output must stay inside desktop/src-tauri/target.' }
$exe = Join-Path $OutputDirectory 'StreamCap.exe'
if (Test-Path -LiteralPath $exe) { throw 'Output exists; choose a fresh output directory.' }
Push-Location $desktop
try {
    trunk build --release --offline
    if ($LASTEXITCODE -ne 0) { throw 'WASM build failed' }
    cargo build --manifest-path src-tauri/Cargo.toml --release --locked --offline
    if ($LASTEXITCODE -ne 0) { throw 'Native build failed' }
    [IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
    [IO.File]::Copy((Join-Path $desktop 'src-tauri\target\release\streamcap-tauri.exe'),$exe,$false)
    $hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$exe.sha256", "$hash  StreamCap.exe" + [Environment]::NewLine,[Text.UTF8Encoding]::new($false))
    Write-Output "Native noFF executable: $exe"
} finally { Pop-Location }
