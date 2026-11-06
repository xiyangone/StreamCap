# Prepare a pinned, self-contained runtime payload. Downloads happen only while building.
param([switch]$Offline)
$ErrorActionPreference = 'Stop'
$desktop = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$manifest = Get-Content -LiteralPath (Join-Path $desktop 'ffmpeg-bundle.json') -Raw | ConvertFrom-Json
$names = @('ffmpeg.exe','ffprobe.exe','LICENSE','README.txt')
if ($manifest.schemaVersion -ne 1 -or $manifest.archiveSha256 -cnotmatch '^[a-f0-9]{64}$' -or
    $manifest.files.Count -ne $names.Count -or (Compare-Object ($names | Sort-Object) ($manifest.files.name | Sort-Object))) {
    throw 'Invalid FFmpeg bundle manifest.'
}
foreach ($file in $manifest.files) {
    if ($file.sha256 -cnotmatch '^[a-f0-9]{64}$' -or $file.size -le 0 -or $file.size -gt 268435456) { throw 'Invalid bundled file checksum or size.' }
}
$archiveUri = [Uri]$manifest.archiveUrl
if ($archiveUri.Scheme -ne 'https' -or $archiveUri.Host -ne 'www.gyan.dev' -or
    $archiveUri.AbsolutePath -notmatch '^/ffmpeg/builds/packages/ffmpeg-[0-9.]+-essentials_build\.zip$') {
    throw 'FFmpeg must use a fixed official release URL.'
}
$cache = Join-Path $desktop 'build/ffmpeg-bundle'
[IO.Directory]::CreateDirectory($cache) | Out-Null
function Assert-RegularFile([string]$FilePath) {
    $item = Get-Item -LiteralPath $FilePath -Force
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Expected an ordinary file: $FilePath" }
}
function Assert-FileHash([string]$FilePath, $Spec) {
    Assert-RegularFile $FilePath
    if ((Get-Item -LiteralPath $FilePath).Length -ne $Spec.size -or
        (Get-FileHash -LiteralPath $FilePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Spec.sha256) {
        throw "Bundled file verification failed: $FilePath"
    }
}
function Assert-Bundle([string]$BundlePath) {
    Assert-RegularFile $BundlePath
    $zip = [IO.Compression.ZipFile]::OpenRead($BundlePath)
    try {
        if ($zip.Entries.Count -ne $manifest.files.Count) { throw 'Unexpected runtime package entries.' }
        foreach ($file in $manifest.files) {
            $entries = @($zip.Entries | Where-Object { $_.FullName -ceq $file.name })
            if ($entries.Count -ne 1 -or $entries[0].Length -ne $file.size) { throw "Invalid runtime entry: $($file.name)" }
            $stream = $entries[0].Open()
            try { $hash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($stream)).ToLowerInvariant() }
            finally { $stream.Dispose() }
            if ($hash -ne $file.sha256) { throw "Runtime entry checksum mismatch: $($file.name)" }
        }
    } finally { $zip.Dispose() }
}
$lockPath = Join-Path $cache 'prepare.lock'
if (Test-Path -LiteralPath $lockPath) { Assert-RegularFile $lockPath }
$lock = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
try {
    $archivePath = Join-Path $cache ([IO.Path]::GetFileName($archiveUri.AbsolutePath))
    if (-not (Test-Path -LiteralPath $archivePath)) {
        if ($Offline) { throw 'Pinned FFmpeg archive is missing; run prepare-ffmpeg.ps1 once with network access.' }
        $download = Join-Path $cache ('download-' + [Guid]::NewGuid().ToString('N') + '.zip')
        Invoke-WebRequest -Uri $archiveUri.AbsoluteUri -OutFile $download -TimeoutSec 600 -MaximumRetryCount 3 -RetryIntervalSec 5
        if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest.archiveSha256) {
            throw 'FFmpeg archive checksum mismatch; the rejected download has been retained for inspection.'
        }
        [IO.File]::Move($download,$archivePath,$false)
    }
    Assert-RegularFile $archivePath
    if ((Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $manifest.archiveSha256) { throw 'Cached FFmpeg archive checksum mismatch.' }
    $tools = Join-Path $cache $manifest.archiveSha256
    if (-not (Test-Path -LiteralPath $tools)) {
        $staging = Join-Path $cache ('prepare-' + [Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($staging) | Out-Null
        $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
        try {
            foreach ($file in $manifest.files) {
                $entries = @($archive.Entries | Where-Object { $_.FullName -ceq $file.archivePath })
                if ($entries.Count -ne 1 -or $entries[0].Length -ne $file.size) { throw "Invalid official archive entry: $($file.name)" }
                $destination = Join-Path $staging $file.name
                $inputStream = $entries[0].Open()
                $outputStream = [IO.File]::Open($destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
                try { $inputStream.CopyTo($outputStream); $outputStream.Flush($true) }
                finally { $outputStream.Dispose(); $inputStream.Dispose() }
                Assert-FileHash $destination $file
            }
        } finally { $archive.Dispose() }
        [IO.Directory]::Move($staging,$tools)
    }
    $toolDirectory = Get-Item -LiteralPath $tools -Force
    if (-not $toolDirectory.PSIsContainer -or ($toolDirectory.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Invalid prepared tools directory.' }
    foreach ($file in $manifest.files) { Assert-FileHash (Join-Path $tools $file.name) $file }
    $bundleName = $manifest.archiveSha256 + '.bundle.zip'
    $bundlePath = Join-Path $cache $bundleName
    if (-not (Test-Path -LiteralPath $bundlePath)) {
        $pendingBundle = Join-Path $cache ('bundle-' + [Guid]::NewGuid().ToString('N') + '.zip')
        $zip = [IO.Compression.ZipFile]::Open($pendingBundle,[IO.Compression.ZipArchiveMode]::Create)
        try {
            foreach ($file in $manifest.files) {
                $entry = $zip.CreateEntry($file.name,[IO.Compression.CompressionLevel]::Optimal)
                $entry.LastWriteTime = [DateTimeOffset]::new(1980,1,1,0,0,0,[TimeSpan]::Zero)
                $inputStream = [IO.File]::OpenRead((Join-Path $tools $file.name))
                $outputStream = $entry.Open()
                try { $inputStream.CopyTo($outputStream) }
                finally { $outputStream.Dispose(); $inputStream.Dispose() }
            }
        } finally { $zip.Dispose() }
        Assert-Bundle $pendingBundle
        [IO.File]::Move($pendingBundle,$bundlePath,$false)
    }
    Assert-Bundle $bundlePath
    $includePath = Join-Path $cache 'embedded.rs'
    $include = '// Generated by prepare-ffmpeg.ps1 from the pinned manifest.' + "`n" +
        'const FFMPEG_BUNDLE: &[u8] = include_bytes!("' + $bundleName + '");' + "`n"
    if (Test-Path -LiteralPath $includePath) { Assert-RegularFile $includePath }
    if (-not (Test-Path -LiteralPath $includePath) -or [IO.File]::ReadAllText($includePath) -cne $include) {
        [IO.File]::WriteAllText($includePath,$include,[Text.UTF8Encoding]::new($false))
    }
    Write-Host "Verified bundled FFmpeg $($manifest.version) and ffprobe with their license and source notice."
    Write-Output $tools
} finally { $lock.Dispose() }
