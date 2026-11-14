# Interoperability run: the real client (the game runtime's --probe-lan-peer mode) against kalamar-relay on 127.0.0.1.
# Every file the run writes stays under crates\kalamar-relay\interop-run (ignored by git). -Exe is the game runtime
# executable the player built.
#   .\scripts\interop.ps1 -Exe <runtime.exe> -Peers 3                three peers exchanging through the relay
#   .\scripts\interop.ps1 -Exe <runtime.exe> -Peers 3 -RestartRelay  the relay is killed 6 s in and started again
#                                                                    2 s later: the peers must rejoin at the same
#                                                                    virtual addresses
# Exit code 0 when every peer passed.
param([int]$Peers = 3, [int]$Seconds = 20, [switch]$RestartRelay,
      [Parameter(Mandatory = $true)][string]$Exe,
      [string]$RelayExe = (Join-Path $PSScriptRoot '..\..\..\target\release\kalamar-relay.exe'),
      [string]$Room = 'PEERTEST')
$ErrorActionPreference = 'Stop'
$Exe = (Resolve-Path $Exe).Path
$RelayExe = (Resolve-Path $RelayExe).Path
$run = Join-Path (Split-Path $PSScriptRoot) 'interop-run'
New-Item -ItemType Directory -Force $run | Out-Null

function Start-Relay([string]$tag) {
    $log = Join-Path $run "relay-$tag"
    $p = Start-Process -FilePath $RelayExe -ArgumentList @('--open', '--bind', '127.0.0.1', '--port', '47313', '--log-level', 'info', '--stats-interval', '5') `
        -NoNewWindow -PassThru -RedirectStandardOutput "$log.out" -RedirectStandardError "$log.err"
    $null = $p.Handle
    Start-Sleep -Milliseconds 500
    [pscustomobject]@{ p = $p; log = $log }
}

$relay = $null
$procs = @()
$fail = 0
try {
    $relay = Start-Relay 'first'
    for ($i = 1; $i -le $Peers; $i++) {
        $u = Join-Path $run "peer$i"
        New-Item -ItemType Directory -Force $u | Out-Null
        $env:KLM_LAN = 'relay:127.0.0.1:47313'
        $env:KLM_LAN_ROOM = $Room
        $env:KLM_LAN_NAME = "peer $i"
        $env:KLM_PEER_SECONDS = "$Seconds"
        $env:KLM_PEER_HOLD = if ($RestartRelay) { '1' } else { '0' }
        $log = Join-Path $u 'peer'
        $p = Start-Process -FilePath $Exe -ArgumentList '--probe-lan-peer' -WorkingDirectory $u -NoNewWindow -PassThru `
            -RedirectStandardOutput "$log.out" -RedirectStandardError "$log.err"
        $null = $p.Handle
        $procs += [pscustomobject]@{ i = $i; p = $p; log = $log }
    }
    Remove-Item Env:KLM_LAN, Env:KLM_LAN_ROOM, Env:KLM_LAN_NAME, Env:KLM_PEER_SECONDS, Env:KLM_PEER_HOLD
    $second = $null
    if ($RestartRelay) {
        Start-Sleep -Seconds 6
        Stop-Process -Id $relay.p.Id -Force
        $relay.p.WaitForExit()
        'relay killed at 6 s; started again 2 s later'
        Start-Sleep -Seconds 2
        $second = Start-Relay 'restarted'
    }
    foreach ($x in $procs) {
        if (-not $x.p.WaitForExit(($Seconds + 20) * 1000)) { Stop-Process -Id $x.p.Id -Force; $x.p.WaitForExit() }
        "--- peer $($x.i): exit $($x.p.ExitCode)"
        Get-Content "$($x.log).out"
        Get-Content "$($x.log).err" | Select-String 'lan:' | ForEach-Object { "    $($_.Line)" }
        if ($x.p.ExitCode -ne 0) { $fail++ }
    }
} finally {
    foreach ($r in @($relay, $second)) {
        if ($r -and -not $r.p.HasExited) { Stop-Process -Id $r.p.Id -Force; $r.p.WaitForExit() }
    }
    foreach ($x in $procs) { if (-not $x.p.HasExited) { Stop-Process -Id $x.p.Id -Force } }
}
foreach ($r in @($relay, $second)) {
    if ($r) { "--- relay ($($r.log))"; Get-Content "$($r.log).out"; Get-Content "$($r.log).err" }
}
"interop ($Peers peers$(if ($RestartRelay) { ', relay restart' })): $(if ($fail) { "FAILED ($fail)" } else { 'passed' })"
exit $fail
