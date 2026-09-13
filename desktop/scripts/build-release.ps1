# Native noFF build: one directly runnable StreamCap.exe; never packages Python or user data.
param([string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$desktop = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$env:NO_COLOR = 'true'
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
    & (Join-Path $PSScriptRoot 'build-frontend.ps1')
    cargo build --manifest-path src-tauri/Cargo.toml --release --locked --offline
    if ($LASTEXITCODE -ne 0) { throw 'Native build failed' }
    [IO.Directory]::CreateDirectory($OutputDirectory) | Out-Null
    [IO.File]::Copy((Join-Path $desktop 'src-tauri\target\release\streamcap-tauri.exe'),$exe,$false)
    $hash = (Get-FileHash -LiteralPath $exe -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$exe.sha256", "$hash  StreamCap.exe" + [Environment]::NewLine,[Text.UTF8Encoding]::new($false))
    Write-Host "Native noFF executable: $exe"
    Write-Output $exe
} finally { Pop-Location }
