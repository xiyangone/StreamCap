# Explicit real-target validation. No production process, credentials copy, or implicit login.
param(
 [Parameter(Mandatory)][string]$Executable,
 [Parameter(Mandatory)][string]$ProfileDirectory,
 [Parameter(Mandatory)][string]$MediaRoot,
 [Parameter(Mandatory)][string]$MpvPath,
 [string]$ArtifactsDirectory,
 [string]$RoomIndices,
 [string]$ProxyConfig
)
$ErrorActionPreference='Stop'
$desktop=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$Executable=(Resolve-Path -LiteralPath $Executable).Path
if(-not $Executable.StartsWith((Join-Path $desktop 'src-tauri/target')+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Only a project candidate may run'}
$ProfileDirectory=(Resolve-Path -LiteralPath $ProfileDirectory).Path
$MediaRoot=(Resolve-Path -LiteralPath $MediaRoot).Path
$MpvPath=(Resolve-Path -LiteralPath $MpvPath).Path
if(-not $MpvPath.StartsWith((Join-Path $desktop 'build/mpv-benchmark')+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)){throw 'Use the isolated benchmark component'}
$run=if($ArtifactsDirectory){[IO.Path]::GetFullPath($ArtifactsDirectory)}else{Join-Path $desktop ('tests/artifacts/real-'+(Get-Date -Format 'yyyyMMdd-HHmmss')+'-'+[Guid]::NewGuid().ToString('N').Substring(0,6))}
$allowed=[IO.Path]::GetFullPath((Join-Path $desktop 'tests/artifacts'))+[IO.Path]::DirectorySeparatorChar
if(-not $run.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase) -or (Test-Path -LiteralPath $run)){throw 'Use a fresh project evidence directory'}
[IO.Directory]::CreateDirectory((Join-Path $run 'tmp'))|Out-Null
$protected=@('recordings.json','cookies.json','user_settings.json','accounts.json','web_auth.json','media_jobs.json')
$before=@{};foreach($name in $protected){$p=Join-Path $ProfileDirectory ('config/'+$name);if(Test-Path -LiteralPath $p){$before[$name]=(Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash}}
$records=Get-Content -LiteralPath (Join-Path $ProfileDirectory 'config/recordings.json') -Raw|ConvertFrom-Json
if(-not $RoomIndices){$RoomIndices=(@(for($i=0;$i -lt $records.Count;$i++){if($records[$i].monitor_status -and $records[$i].platform_key -in @('douyin','kuaishou')){$i}})|Select-Object -First 7)-join ','}
if(-not $RoomIndices){throw 'No monitored supported real room; provide explicit room indices'}
$environment=@('TEMP','TMP','STREAMCAP_TEST_ARTIFACTS','STREAMCAP_REAL_PROFILE','STREAMCAP_REAL_ROOM_INDICES','STREAMCAP_PROXY_CONFIG','STREAMCAP_TEST_FFMPEG','STREAMCAP_TEST_PWSH','PLAYWRIGHT_BROWSERS_PATH')
$saved=@{};foreach($name in $environment){$saved[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$env:TEMP=Join-Path $run 'tmp';$env:TMP=$env:TEMP;$env:STREAMCAP_TEST_ARTIFACTS=$run;$env:STREAMCAP_REAL_PROFILE=$ProfileDirectory;$env:STREAMCAP_REAL_ROOM_INDICES=$RoomIndices;$env:STREAMCAP_PROXY_CONFIG=$ProxyConfig;$env:STREAMCAP_TEST_FFMPEG=(Get-Command ffmpeg -ErrorAction Stop).Source;$env:STREAMCAP_TEST_PWSH=(Get-Command pwsh -ErrorAction Stop).Source;$env:PLAYWRIGHT_BROWSERS_PATH=Join-Path $desktop 'build/playwright'
$report=[ordered]@{passed=$false;artifacts=$run;sourceProfileUnchanged=$false;steps=@();gaps=@();platforms=@()}
function Step([string]$name,[scriptblock]$body){$global:LASTEXITCODE=0;& $body *>&1|Tee-Object -LiteralPath (Join-Path $run ($name+'.log'));$code=$LASTEXITCODE;$report.steps+=@{name=$name;exitCode=$code;passed=($code -eq 0)};if($code -ne 0){throw "$name failed: $code"}}
Push-Location $desktop
try {
 Step 'network-route' {
  $rows=@();foreach($hostName in @('live.douyin.com','live.kuaishou.com')){try{$ip=[Net.Dns]::GetHostAddresses($hostName)|Where-Object AddressFamily -eq InterNetwork|Select-Object -First 1;$route=Find-NetRoute -RemoteIPAddress $ip.IPAddressToString -ErrorAction Stop;$rows+=@{host=$hostName;address=$ip.IPAddressToString;route=@($route|Select-Object InterfaceAlias,DestinationPrefix,NextHop)}}catch{$rows+=@{host=$hostName;route='unavailable'}}};$rows|ConvertTo-Json -Depth 5
 }
 Step 'real-platforms' {& cargo test --manifest-path core/Cargo.toml --locked --offline --test lifecycle_regression real_rooms_resolve_and_preview_without_changing_user_data -- --ignored --exact --nocapture}
 $report.platforms=@(Get-Content -LiteralPath (Join-Path $run 'real-platforms.log')|Where-Object {$_ -like 'REAL_ROOM_PROBE *'}|ForEach-Object {($_ -replace '^REAL_ROOM_PROBE ','')|ConvertFrom-Json})
 if(-not ($report.platforms|Where-Object {$_.live -and $_.mpegTsReadable})){$report.gaps+=@('No readable live stream in the explicit target sample; live recovery remains unverified')}
 if(-not ($report.platforms|Where-Object {$_.controlledRecovery.recovered -and $_.controlledRecovery.stopPreventedRestart})){$report.gaps+=@('Controlled real-stream interruption and recovery not verified')}
 foreach($platform in $report.platforms){if(-not $platform.businessReadable){$report.gaps+=@("Platform $($platform.platform): $($platform.accessState) $($platform.error)")}}
 Step 'real-preview' {& node tests/real-preview.mjs --exe $Executable --media-root $MediaRoot --expected-sha256 (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash}
 $media=Get-ChildItem -LiteralPath $MediaRoot -File -Recurse|Where-Object Extension -eq '.mp4'|Sort-Object Length -Descending|Select-Object -First 1
 if(-not $media){throw 'No actual MP4 for the decoder comparison'}
 Step 'mpv-comparison' {& node tests/mpv-comparison.mjs --mpv $MpvPath --media-file $media.FullName --expected-sha256 (Get-FileHash -LiteralPath $MpvPath -Algorithm SHA256).Hash}
 $report['routes']=@(Get-Content -LiteralPath (Join-Path $run 'real-platforms.log')|Where-Object {$_ -like 'REAL_ROUTE_PROBE *'}|ForEach-Object {($_ -replace '^REAL_ROUTE_PROBE ','')|ConvertFrom-Json})
 if(-not $report.routes.Count){$report.gaps+=@('Final proxy outbound node was not observed for the test process; OS route alone is not proof of that node')}
 $report.passed=$report.gaps.Count -eq 0
}catch{$report.failure=$_.Exception.Message;throw}
finally{
 $unchanged=$true;foreach($name in $before.Keys){$p=Join-Path $ProfileDirectory ('config/'+$name);if(-not(Test-Path -LiteralPath $p) -or (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash -ne $before[$name]){$unchanged=$false}}
 $report.sourceProfileUnchanged=$unchanged;if(-not $unchanged){$report.passed=$false;$report.gaps+=@('User profile changed during validation')}
 $report|ConvertTo-Json -Depth 10|Set-Content -LiteralPath (Join-Path $run 'verification.json') -Encoding utf8
 Pop-Location;foreach($name in $environment){[Environment]::SetEnvironmentVariable($name,$saved[$name],'Process')}
 Write-Output ('Real verification report: '+(Join-Path $run 'verification.json'))
}
if(-not $report.passed){Write-Output 'Real validation has explicitly reported gaps; not a full pass.';exit 2}
