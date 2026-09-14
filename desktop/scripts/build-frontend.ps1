param([switch]$Serve)
$ErrorActionPreference='Stop'
$desktop=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$tools=Join-Path $desktop 'target\tools\bin'
$tool=Join-Path $tools 'wasm-bindgen.exe'
$lock=[IO.File]::ReadAllText((Join-Path $desktop 'Cargo.lock'))
$version=[regex]::Match($lock,'(?m)^name = "wasm-bindgen"\r?\nversion = "([^\"]+)"').Groups[1].Value
if(-not $version){throw 'wasm-bindgen is missing from the frontend lockfile.'}
if(-not(Test-Path -LiteralPath $tool)){throw "Missing project-local wasm-bindgen $version. Install with cargo install wasm-bindgen-cli --version $version --locked --root $desktop\target\tools"}
$actual=(& $tool --version).Trim()
if($LASTEXITCODE -ne 0 -or $actual -ne "wasm-bindgen $version"){throw "wasm-bindgen version mismatch. Lockfile: $version; CLI: $actual"}
if(-not(Test-Path -LiteralPath (Join-Path $desktop 'node_modules\@tauri-apps\api\core.js'))){throw 'Run npm ci in desktop before building.'}
if(-not(Test-Path -LiteralPath (Join-Path $desktop 'node_modules\mpegts.js\dist\mpegts.js'))){throw 'Run npm ci in desktop: local TS preview assets are required.'}
$previousPath=$env:PATH
$previousColor=$env:NO_COLOR
$env:NO_COLOR='true'
$env:PATH="$tools;$previousPath"
Push-Location $desktop
try {
    if($Serve){& trunk serve --port 1420 --locked --offline}else{& trunk build --release --locked --offline}
    if($LASTEXITCODE -ne 0){throw 'Frontend build failed.'}
} finally {Pop-Location;$env:PATH=$previousPath;$env:NO_COLOR=$previousColor}
