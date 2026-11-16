<#
.SYNOPSIS
    Measure the Slint feed spike per renderer: idle and autoscroll.

.DESCRIPTION
    For every backend: start the spike, let it settle, sample memory/CPU while idle,
    then restart it with autoscroll and sample again. Frame statistics printed by
    Slint (SLINT_DEBUG_PERFORMANCE) go to a stderr log next to the CSV files.
    ASCII-only on purpose (PowerShell 5.1 reads BOM-less UTF-8 as ANSI).

.EXAMPLE
    .\bench.ps1
    .\bench.ps1 -Backends winit-software -IdleSec 60
#>
[CmdletBinding()]
param(
    [string[]]$Backends = @('winit-software', 'winit-femtovg'),
    [int]$IdleSec = 30,
    [int]$ScrollSec = 20,
    [int]$Count = 10000,
    [string[]]$ExtraArgs = @(),
    [string]$Tag = '',
    [string]$TargetDir = 'target'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root "$TargetDir\release\slint-feed-spike.exe"
$measure = Join-Path $root '..\..\scripts\measure.ps1'
$out = Join-Path $root '..\..\bench\results'
$date = Get-Date -Format 'yyyy-MM-dd'
$name = 'slint-feed-spike'

if (-not (Test-Path $exe)) { throw "Build first: cargo build --release ($exe not found)" }
if (-not (Test-Path $out)) { New-Item -ItemType Directory -Force $out | Out-Null }
Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force

foreach ($backend in $Backends) {
    $prefix = Join-Path $out "$date-slint-$backend$Tag"
    $env:SLINT_BACKEND = $backend

    Remove-Item Env:SLINT_DEBUG_PERFORMANCE -ErrorAction SilentlyContinue
    $idleArgs = @('--count', $Count, '--start-bottom') + $ExtraArgs
    $p = Start-Process -FilePath $exe -ArgumentList $idleArgs -PassThru
    Start-Sleep -Seconds 6
    "=== $backend$Tag | idle | $Count messages"
    & $measure -Name $name -DurationSec $IdleSec -Csv "$prefix-idle.csv"
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Start-Sleep -Seconds 1

    $env:SLINT_DEBUG_PERFORMANCE = 'refresh_lazy,console'
    $log = "$prefix-scroll.stderr.txt"
    $scrollArgs = @('--count', $Count, '--autoscroll', ($ScrollSec + 8), '--exit-after', ($ScrollSec + 10)) + $ExtraArgs
    $p = Start-Process -FilePath $exe -ArgumentList $scrollArgs -PassThru -RedirectStandardError $log
    Start-Sleep -Seconds 4
    "=== $backend$Tag | autoscroll | $Count messages"
    & $measure -Name $name -DurationSec $ScrollSec -Csv "$prefix-scroll.csv"
    if (-not $p.WaitForExit(20000)) { Stop-Process -Id $p.Id -Force }
    "--- last lines of $log"
    Get-Content $log -Tail 6
    Remove-Item Env:SLINT_DEBUG_PERFORMANCE -ErrorAction SilentlyContinue
}
Remove-Item Env:SLINT_BACKEND -ErrorAction SilentlyContinue
