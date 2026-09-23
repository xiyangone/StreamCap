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
    if(-not $Serve){
        $required=@('tauri-api/core.js','tauri-api/event.js','tauri-api/external/tslib/tslib.es6.js','tauri-api/LICENSE_MIT','tauri-api/LICENSE_APACHE-2.0','media/mpegts.js','media/mpegts.js.LICENSE.txt','media/LICENSE')
        $dist=Join-Path $desktop 'dist'
        foreach($asset in $required){if(-not(Test-Path -LiteralPath (Join-Path $dist $asset) -PathType Leaf)){throw "Missing runtime asset: $asset"}}
        $actual=@(foreach($directory in @('tauri-api','media')){Get-ChildItem -LiteralPath (Join-Path $dist $directory) -File -Recurse | ForEach-Object {[IO.Path]::GetRelativePath($dist,$_.FullName).Replace('\','/')}})
        if(Compare-Object ($required|Sort-Object) ($actual|Sort-Object)){throw 'Unexpected runtime assets: keep only the declared modules and licenses.'}
        Write-Output "Verified $($actual.Count) third-party runtime and license assets."
    }
} finally {Pop-Location;$env:PATH=$previousPath;$env:NO_COLOR=$previousColor}
