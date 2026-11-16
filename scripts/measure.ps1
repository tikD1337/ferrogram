<#
.SYNOPSIS
    Sample resource usage of a process (or all processes sharing a name).

.DESCRIPTION
    Takes a sample every -IntervalSec seconds and prints a summary at the end.
    Compatible with Windows PowerShell 5.1. This file is ASCII-only on purpose:
    PowerShell 5.1 reads BOM-less UTF-8 as the ANSI code page.

    Metrics (do not mix them up):
      WorkingSet     physical memory including shared pages (DLLs etc).
      PrivateWS      private part of the working set ("Memory" in Task Manager).
      PrivateBytes   private committed memory; the main figure for comparisons.
      CpuPct         share of ALL logical cores (100 = every core busy).

.EXAMPLE
    .\scripts\measure.ps1 -Name Telegram -DurationSec 60
    .\scripts\measure.ps1 -Name Telegram -DurationSec 300 -Csv .\bench\results\tdesktop-idle.csv
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Name,
    [int]$DurationSec = 60,
    [int]$IntervalSec = 1,
    [string]$Csv = ''
)

$ErrorActionPreference = 'Stop'
$cores = [Environment]::ProcessorCount
$mb = 1MB

function Get-Snapshot {
    $procs = @(Get-Process -Name $Name -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) { return $null }

    $ids = $procs | ForEach-Object { $_.Id }
    $privateWs = 0
    $perf = @(Get-CimInstance -ClassName Win32_PerfRawData_PerfProc_Process -ErrorAction SilentlyContinue |
        Where-Object { $ids -contains $_.IDProcess })
    foreach ($p in $perf) { $privateWs += $p.WorkingSetPrivate }

    $cpuMs = 0.0
    foreach ($p in $procs) {
        try { $cpuMs += $p.TotalProcessorTime.TotalMilliseconds } catch { }
    }

    [pscustomobject]@{
        Time         = Get-Date
        Processes    = $procs.Count
        WorkingSet   = ($procs | Measure-Object -Property WorkingSet64 -Sum).Sum
        PrivateWS    = $privateWs
        PrivateBytes = ($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum
        Threads      = ($procs | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum
        Handles      = ($procs | Measure-Object -Property HandleCount -Sum).Sum
        CpuMs        = $cpuMs
    }
}

$first = Get-Snapshot
if ($null -eq $first) {
    Write-Error "Process '$Name' not found."
    exit 1
}

$rows = New-Object System.Collections.Generic.List[object]
$prev = $first
$deadline = (Get-Date).AddSeconds($DurationSec)

while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds $IntervalSec
    $cur = Get-Snapshot
    if ($null -eq $cur) { Write-Warning 'Process exited during the measurement.'; break }

    $wallMs = ($cur.Time - $prev.Time).TotalMilliseconds
    $cpuPct = 0.0
    if ($wallMs -gt 0) { $cpuPct = ($cur.CpuMs - $prev.CpuMs) / $wallMs / $cores * 100.0 }

    $rows.Add([pscustomobject]@{
            Time           = $cur.Time.ToString('HH:mm:ss')
            Processes      = $cur.Processes
            WorkingSetMB   = [math]::Round($cur.WorkingSet / $mb, 1)
            PrivateWSMB    = [math]::Round($cur.PrivateWS / $mb, 1)
            PrivateBytesMB = [math]::Round($cur.PrivateBytes / $mb, 1)
            Threads        = $cur.Threads
            Handles        = $cur.Handles
            CpuPct         = [math]::Round($cpuPct, 3)
        })
    $prev = $cur
}

if ($rows.Count -eq 0) { Write-Error 'No samples collected.'; exit 1 }

if ($Csv -ne '') {
    $dir = Split-Path -Parent $Csv
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
    $rows | Export-Csv -Path $Csv -NoTypeInformation -Encoding UTF8
}

function Get-Stat([string]$prop) {
    $m = $rows | Measure-Object -Property $prop -Average -Minimum -Maximum
    [pscustomobject]@{
        Metric = $prop
        Avg    = [math]::Round($m.Average, 2)
        Min    = [math]::Round($m.Minimum, 2)
        Max    = [math]::Round($m.Maximum, 2)
    }
}

$totalCpuMs = $prev.CpuMs - $first.CpuMs
$totalWallMs = ($prev.Time - $first.Time).TotalMilliseconds
$allCoresPct = $totalCpuMs / $totalWallMs / $cores * 100
$oneCorePct = $totalCpuMs / $totalWallMs * 100

"Process: $Name | instances: $($prev.Processes) | samples: $($rows.Count) | logical cores: $cores"
@('WorkingSetMB', 'PrivateWSMB', 'PrivateBytesMB', 'Threads', 'Handles', 'CpuPct') |
    ForEach-Object { Get-Stat $_ } | Format-Table -AutoSize | Out-String
'CPU over the whole interval: {0:N1} ms of CPU time in {1:N1} s' -f $totalCpuMs, ($totalWallMs / 1000)
'  = {0:N3} percent of all cores, {1:N3} percent of one core' -f $allCoresPct, $oneCorePct
