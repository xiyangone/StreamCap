param([switch]$Serve)
$ErrorActionPreference='Stop'
$desktop=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
& (Join-Path $PSScriptRoot 'prepare-ffmpeg.ps1') | Out-Null
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
$manifest=Get-Content -Raw -LiteralPath (Join-Path $desktop 'package.json')|ConvertFrom-Json
foreach($dependency in @('daisyui','tailwindcss','@tailwindcss/cli')){
    $package=Join-Path $desktop ("node_modules/$dependency/package.json")
    if(-not(Test-Path -LiteralPath $package)){throw "Run npm ci in desktop: missing local $dependency."}
    $installed=Get-Content -Raw -LiteralPath $package|ConvertFrom-Json
    if($installed.version -ne $manifest.devDependencies.$dependency){throw "Run npm ci in desktop: $dependency must match the pinned version."}
}
$previousPath=$env:PATH
$previousColor=$env:NO_COLOR
$env:NO_COLOR='true'
$env:PATH="$tools;$previousPath"
Push-Location $desktop
try {
    if($Serve){& trunk serve --port 1420 --locked --offline}else{& trunk build --release --locked --offline}
    if($LASTEXITCODE -ne 0){throw 'Frontend build failed.'}
    if(-not $Serve){
        $required=@('tauri-api/core.js','tauri-api/event.js','tauri-api/external/tslib/tslib.es6.js','tauri-api/LICENSE-MIT','tauri-api/LICENSE-APACHE-2.0','media/mpegts.js','media/mpegts.js.LICENSE.txt','media/LICENSE','ui/daisyui/LICENSE','ui/tailwindcss/LICENSE')
        $dist=Join-Path $desktop 'dist'
        foreach($asset in @('logo-mark.png','media-player.js','index.html')){if(-not(Test-Path -LiteralPath (Join-Path $dist $asset) -PathType Leaf)){throw "Missing application asset: $asset"}}
        foreach($asset in $required){if(-not(Test-Path -LiteralPath (Join-Path $dist $asset) -PathType Leaf)){throw "Missing runtime asset: $asset"}}
        $actual=@(foreach($directory in @('tauri-api','media','ui')){Get-ChildItem -LiteralPath (Join-Path $dist $directory) -File -Recurse | ForEach-Object {[IO.Path]::GetRelativePath($dist,$_.FullName).Replace('\','/')}})
        if(Compare-Object ($required|Sort-Object) ($actual|Sort-Object)){throw 'Unexpected runtime assets: keep only the declared modules and licenses.'}
        Write-Output "Verified $($actual.Count) third-party runtime and license assets."
    }
} finally {Pop-Location;$env:PATH=$previousPath;$env:NO_COLOR=$previousColor}
