# One reproducible native validation pipeline. Never launches a deployed application.
param([string]$OutputDirectory)
$ErrorActionPreference='Stop'
$desktop=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$repo=[IO.Path]::GetFullPath((Join-Path $desktop '..'))
$stamp=(Get-Date -Format 'yyyyMMdd-HHmmss')+'-'+[Guid]::NewGuid().ToString('N').Substring(0,6)
$run=Join-Path $desktop ('tests/artifacts/verify-'+$stamp)
$tmp=Join-Path $run 'tmp'
[IO.Directory]::CreateDirectory($tmp)|Out-Null
$publish=if($OutputDirectory){[IO.Path]::GetFullPath($OutputDirectory)}else{Join-Path $desktop ('src-tauri/target/native-noFF-'+$stamp)}
$steps=[Collections.Generic.List[object]]::new()
$names=@('TEMP','TMP','STREAMCAP_TEST_ARTIFACTS','npm_config_cache','NO_COLOR','STREAMCAP_TEST_FFMPEG','STREAMCAP_TEST_PWSH','PLAYWRIGHT_BROWSERS_PATH')
$saved=@{};foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$env:TEMP=$tmp;$env:TMP=$tmp;$env:STREAMCAP_TEST_ARTIFACTS=$run;$env:npm_config_cache=Join-Path $desktop 'build/npm-cache';$env:NO_COLOR='true'
if(-not $env:PLAYWRIGHT_BROWSERS_PATH){$env:PLAYWRIGHT_BROWSERS_PATH=Join-Path $desktop 'build/playwright'}
function Step([string]$name,[scriptblock]$body){
    $global:LASTEXITCODE=0;$watch=[Diagnostics.Stopwatch]::StartNew()
    try {& $body *>&1 | Tee-Object -LiteralPath (Join-Path $run ($name+'.log'));if($LASTEXITCODE -ne 0){throw "$name failed: exit $LASTEXITCODE"};$steps.Add(@{name=$name;passed=$true;seconds=$watch.Elapsed.TotalSeconds})}
    catch{$steps.Add(@{name=$name;passed=$false;error=$_.Exception.Message});throw}
}
$passed=$false;$failure=$null
Push-Location $desktop
try {
    Step 'legacy-boundary' {
        $legacy=@(
            'app','scripts','tests','locales','docs/superpowers',
            'desktop/pyserver','desktop/dist-resolver','desktop/build/resolver','desktop/src-tauri/binaries',
            'main.py','pyproject.toml','poetry.toml','requirements.txt','requirements-web.txt',
            '.ruff.toml','.env.example','.dockerignore','Dockerfile','docker-compose.yml','version_info.txt',
            '.github/workflows/python-lint.yml','.github/workflows/docker-build.yml','.github/workflows/sync.yml',
            '.github/workflows/build-desktop.yml','desktop/build/streamcap-resolver.spec',
            'assets/fonts','assets/icons','assets/favicon.png','assets/icon.ico',
            'assets/images/dmg.jpg','assets/images/example01.png','assets/images/example02.jpg',
            'desktop/assets/logo-mark-64.png','config/language.json','config/version.json'
        )
        foreach($entry in $legacy){if(Test-Path -LiteralPath (Join-Path $repo $entry)){throw "Retired entry remains: $entry"}}
    }
    foreach($entry in @(@('frontend','Cargo.toml'),@('core','core/Cargo.toml'),@('tauri','src-tauri/Cargo.toml'))){
        $label=$entry[0];$manifest=$entry[1]
        Step ($label+'-fmt') {& cargo fmt --manifest-path $manifest --check}
    }
    $audit=Join-Path $desktop 'target/tools/bin/cargo-audit.exe'
    $auditDb=Join-Path $desktop 'target/advisory-db'
    Step 'audit-prerequisite' {
        if(-not(Test-Path -LiteralPath $audit)){throw 'Install project-local cargo-audit: cargo install cargo-audit --version 0.22.2 --locked --root target/tools'}
        $actual=(& $audit --version).Trim()
        if($LASTEXITCODE -ne 0 -or $actual -ne 'cargo-audit 0.22.2'){throw "Expected cargo-audit 0.22.2, found: $actual"}
    }
    $auditFetched=$false
    foreach($entry in @(@('frontend','Cargo.lock'),@('core','core/Cargo.lock'),@('tauri','src-tauri/Cargo.lock'))){
        $label=$entry[0];$lock=$entry[1]
        Step ($label+'-audit') {
            $arguments=@('audit','--file',$lock,'--db',$auditDb,'--json')
            if($auditFetched){$arguments+='--no-fetch'}
            $raw=& $audit @arguments 2> (Join-Path $run ($label+'-audit.stderr.log'))
            $code=$LASTEXITCODE
            $raw|Set-Content -LiteralPath (Join-Path $run ($label+'-audit.json')) -Encoding utf8
            $report=($raw -join [Environment]::NewLine)|ConvertFrom-Json
            Write-Output "$label audit: $($report.vulnerabilities.count) vulnerabilities; database $($report.database.'last-commit')"
            foreach($kind in $report.warnings.PSObject.Properties){Write-Output "Advisory warnings: $($kind.Name)=$(@($kind.Value).Count)"}
            if($code -ne 0){throw "$label dependency audit failed: exit $code; see $label-audit.json"}
        }
        $auditFetched=$true
    }
    Step 'npm-audit' {
        $raw=& npm audit --json 2> (Join-Path $run 'npm-audit.stderr.log')
        $code=$LASTEXITCODE
        $raw|Set-Content -LiteralPath (Join-Path $run 'npm-audit.json') -Encoding utf8
        $report=($raw -join [Environment]::NewLine)|ConvertFrom-Json
        Write-Output "npm audit: $($report.metadata.vulnerabilities.total) vulnerabilities"
        if($code -ne 0){throw "npm dependency audit failed: exit $code; see npm-audit.json"}
    }
    Step 'browser-prerequisite' {& node --input-type=module -e 'import { chromium } from "playwright"; const browser = await chromium.launch({headless:true}); await browser.close();'}
    Step 'ffmpeg-prerequisite' {
        $env:STREAMCAP_TEST_PWSH=(Get-Command pwsh -ErrorAction Stop).Source
        if(-not $env:STREAMCAP_TEST_FFMPEG){$env:STREAMCAP_TEST_FFMPEG=(Get-Command ffmpeg -ErrorAction Stop).Source}
        if(-not(Test-Path -LiteralPath $env:STREAMCAP_TEST_FFMPEG)){throw 'FFmpeg validation tool missing'}
        if(-not(Test-Path -LiteralPath (Join-Path (Split-Path -Parent $env:STREAMCAP_TEST_FFMPEG) 'ffprobe.exe'))){throw 'Adjacent ffprobe is required for recording validation'}
    }
    Step 'core-test' {& cargo test --manifest-path core/Cargo.toml --locked --offline}
    Step 'frontend-test' {& cargo test --manifest-path Cargo.toml --target x86_64-pc-windows-msvc --lib --locked --offline}
    Step 'ffmpeg-recording' {& cargo test --manifest-path core/Cargo.toml --locked --offline --test shutdown ffmpeg_ -- --ignored}
    Step 'media-pipeline' {& cargo test --manifest-path core/Cargo.toml --locked --offline --test media_pipeline -- --ignored}
    Step 'owned-script-tree' {& cargo test --manifest-path core/Cargo.toml --locked --offline --lib owned_process::tests::owned_script_tree_exits_with_its_job -- --ignored}
    Step 'tauri-test' {& cargo test --manifest-path src-tauri/Cargo.toml --locked --offline}
    Step 'core-clippy' {& cargo clippy --manifest-path core/Cargo.toml --all-targets --locked --offline -- -D warnings}
    Step 'tauri-clippy' {& cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --locked --offline -- -D warnings}
    Step 'frontend-clippy' {& cargo clippy --manifest-path Cargo.toml --target wasm32-unknown-unknown --locked --offline -- -D warnings}
    Step 'build' {& (Join-Path $PSScriptRoot 'build-release.ps1') -OutputDirectory $publish}
    Step 'ui' {& npm test}
    $exe=Join-Path $publish 'StreamCap.exe'
    Step 'native-close' {& (Join-Path $desktop 'tests/native-smoke.ps1') -Executable $exe -ExitMode Choice}
    Step 'native-tray' {& (Join-Path $desktop 'tests/native-smoke.ps1') -Executable $exe -ExitMode Tray}
    Step 'diff-check' {& git diff --check}
    $passed=$true
    if($env:GITHUB_ACTIONS -eq 'true' -and $env:GITHUB_OUTPUT){[IO.File]::AppendAllText($env:GITHUB_OUTPUT,"release-directory=$publish"+[Environment]::NewLine)}
} catch {$failure=$_.Exception.Message;throw}
finally {
    $report=@{passed=$passed;failure=$failure;steps=$steps.ToArray();releaseDirectory=$publish;artifacts=$run}
    $report|ConvertTo-Json -Depth 8|Set-Content -LiteralPath (Join-Path $run 'verification.json') -Encoding utf8
    Pop-Location;foreach($name in $names){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
    Write-Host "Verification report: $run/verification.json"
}
if($passed){Write-Output "Verified noFF executable: $exe"}
