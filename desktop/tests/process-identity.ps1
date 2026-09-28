# Shared process identity rules for native smoke and real-media CDP validation.
param(
    [ValidateSet('Library','CheckListener','SelfTest')][string]$Mode='Library',
    [int]$RootPid,
    [string]$CandidateExecutable,
    [datetimeoffset]$StartedAfter,
    [int]$Port
)
$ErrorActionPreference='Stop'
function Test-ProcessInstance($Left, $Right) {
    return $null -ne $Left -and $null -ne $Right -and
        $null -ne $Left.CreationDate -and $null -ne $Right.CreationDate -and
        [int]$Left.ProcessId -eq [int]$Right.ProcessId -and
        $Left.CreationDate.ToUniversalTime() -eq $Right.CreationDate.ToUniversalTime()
}
function Add-OwnedProcessSnapshot([object[]]$Snapshot, $Root, [hashtable]$Owned) {
    $current = @{}
    foreach ($p in $Snapshot) {$current[[int]$p.ProcessId] = $p}
    do {
        $changed = $false
        foreach ($p in $Snapshot) {
            if ([int]$p.ProcessId -eq [int]$Root.ProcessId -or $null -eq $p.CreationDate) {continue}
            $parentId = [int]$p.ParentProcessId
            $parent = if ($parentId -eq [int]$Root.ProcessId) {$Root} else {$Owned[$parentId]}
            # Parent PIDs survive their original process and may refer to a newer, unrelated instance.
            if (-not (Test-ProcessInstance $parent $current[$parentId])) {continue}
            if ($p.CreationDate.ToUniversalTime() -lt $parent.CreationDate.ToUniversalTime()) {continue}
            if (-not (Test-ProcessInstance $Owned[[int]$p.ProcessId] $p)) {
                $Owned[[int]$p.ProcessId] = $p
                $changed = $true
            }
        }
    } while ($changed)
}

function Assert-OwnedWebViewSnapshot([object[]]$Snapshot, $Root, [int]$OwnerId) {
    $current=@{};foreach($p in $Snapshot){$current[[int]$p.ProcessId]=$p}
    if(-not(Test-ProcessInstance $Root $current[[int]$Root.ProcessId])) {throw 'Candidate identity no longer matches'}
    $owned=@{};Add-OwnedProcessSnapshot $Snapshot $Root $owned
    $owner=$current[$OwnerId]
    if($OwnerId -eq [int]$Root.ProcessId -or -not(Test-ProcessInstance $owner $owned[$OwnerId]) -or $owner.Name -ne 'msedgewebview2.exe') {
        throw 'Debug listener does not belong to the candidate WebView instance'
    }
    return $owner
}
function Get-OwnedWebViewListener($Root,[int]$DebugPort) {
    if($DebugPort -lt 1 -or $DebugPort -gt 65535){throw 'Invalid debug port'}
    $listeners=@(Get-NetTCPConnection -State Listen -LocalPort $DebugPort -ErrorAction Stop)
    if(-not $listeners.Count -or @($listeners|Where-Object {$_.LocalAddress -notin @('127.0.0.1','::1')}).Count){throw 'Debug listener must be loopback-only'}
    $owners=@($listeners.OwningProcess|Sort-Object -Unique)
    if($owners.Count -ne 1){throw 'Ambiguous debug listener owners'}
    $all=@(Get-CimInstance Win32_Process|Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CreationDate)
    $owner=Assert-OwnedWebViewSnapshot $all $Root ([int]$owners[0])
    return [ordered]@{confirmed=$true;rootPid=[int]$Root.ProcessId;rootCreatedAt=$Root.CreationDate.ToUniversalTime().ToString('o');ownerPid=[int]$owner.ProcessId;ownerCreatedAt=$owner.CreationDate.ToUniversalTime().ToString('o');port=$DebugPort}
}
if($Mode -eq 'CheckListener'){
    if($RootPid -le 0 -or -not $CandidateExecutable -or -not $PSBoundParameters.ContainsKey('StartedAfter')){throw 'Explicit candidate identity required'}
    $path=(Resolve-Path -LiteralPath $CandidateExecutable).Path
    $root=Get-CimInstance Win32_Process -Filter "ProcessId=$RootPid"|Select-Object ProcessId,ParentProcessId,Name,ExecutablePath,CreationDate
    if($null -eq $root.CreationDate -or -not $root.ExecutablePath -or -not [string]::Equals([IO.Path]::GetFullPath($root.ExecutablePath),$path,[StringComparison]::OrdinalIgnoreCase) -or $root.CreationDate.ToUniversalTime() -lt $StartedAfter.UtcDateTime.AddSeconds(-1)){
        throw 'Candidate path or start time does not match the launched process'
    }
    Get-OwnedWebViewListener $root $Port|ConvertTo-Json -Compress
}
if($Mode -eq 'SelfTest'){
    $checks=[Collections.Generic.List[string]]::new()
    function Fixture([int]$Id,[int]$Parent,[int]$Seconds,[string]$Name='msedgewebview2.exe'){
        [pscustomobject]@{ProcessId=$Id;ParentProcessId=$Parent;CreationDate=([datetime]'2026-09-28T00:00:00Z').AddSeconds($Seconds);Name=$Name;ExecutablePath='fixture'}
    }
    function Check([string]$Name,[bool]$Passed){if(-not $Passed){throw "Identity regression failed: $Name"};$checks.Add($Name)}
    function Reject([string]$Name,[scriptblock]$Call){$rejected=$false;try{& $Call|Out-Null}catch{$rejected=$true};Check $Name $rejected}
    $root=Fixture 100 1 100 'StreamCap.exe';$child=Fixture 200 100 101;$grand=Fixture 300 200 102
    $owned=@{};Add-OwnedProcessSnapshot @($grand,$child,$root) $root $owned
    Check 'out-of-order descendants are recognized' ($owned.Count -eq 2)
    Check 'same process instance accepted' (Test-ProcessInstance $child (Fixture 200 100 101))
    Check 'recycled PID rejected' (-not(Test-ProcessInstance $child (Fixture 200 1 120)))
    Check 'missing instance rejected' (-not(Test-ProcessInstance $child $null))
    $older=Fixture 400 100 90;$oldOwner=Fixture 500 400 95
    $legacy=@(100);do{$before=$legacy.Count;$legacy+=@(@($root,$older,$oldOwner)|Where-Object {$legacy -contains $_.ParentProcessId}|ForEach-Object {[int]$_.ProcessId});$legacy=@($legacy|Sort-Object -Unique)}while($before -ne $legacy.Count)
    Check 'legacy PID-only logic reproduces a false-positive owner' ($legacy -contains 500)
    Reject 'old unrelated ancestors are rejected' {Assert-OwnedWebViewSnapshot @($root,$older,$oldOwner) $root 500}
    $replacement=Fixture 200 1 120;$unrelated=Fixture 600 200 121
    Add-OwnedProcessSnapshot @($root,$replacement,$unrelated) $root $owned
    Check 'recycled tracked parent cannot claim descendants' (-not $owned.ContainsKey(600))
    Reject 'recycled root rejected' {Assert-OwnedWebViewSnapshot @((Fixture 100 1 200 'StreamCap.exe'),(Fixture 700 100 201)) $root 700}
    Check 'owned WebView accepted' ((Assert-OwnedWebViewSnapshot @($grand,$child,$root) $root 300).ProcessId -eq 300)
    Reject 'root cannot masquerade as a WebView' {Assert-OwnedWebViewSnapshot @($root) $root 100}
    Reject 'wrong descendant executable rejected' {Assert-OwnedWebViewSnapshot @($root,(Fixture 800 100 101 'other.exe')) $root 800}
    $missing=Fixture 900 100 101;$missing.CreationDate=$null
    Reject 'missing child creation time rejected' {Assert-OwnedWebViewSnapshot @($root,$missing) $root 900}
    $current=@{200=$replacement;300=$grand};$remaining=@($owned.Keys|Where-Object {Test-ProcessInstance $owned[$_] $current[$_]})
    Check 'only actual surviving process instance is reported' ($remaining.Count -eq 1 -and $remaining[0] -eq 300)
    [ordered]@{passed=$true;cases=$checks.ToArray()}|ConvertTo-Json -Depth 3
}
