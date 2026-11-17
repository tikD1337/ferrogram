<#
.SYNOPSIS
    Compare Telegram clients while they play the same video: memory, CPU and GPU per phase.

.DESCRIPTION
    Interactive: for each client the script asks you to prepare it, then samples the process
    once a second through three phases:
      chat     the chat with the video is open, nothing plays      (-ChatSec)
      playing  the video plays in the client's media viewer         (-PlaySec)
      closed   the viewer is closed again, the chat stays open      (-ClosedSec)
    Only the measured client should play anything; the others may stay open but idle.

    Metrics per phase:
      PrivateMB   private committed memory (Private Bytes), average and maximum
      WorkingMB   working set, including shared pages
      CpuCore     CPU time per wall time, % of ONE core
      Gpu3D       GPU 3D engine running time of the process, % (rendering, presenting)
      GpuDecode   GPU video decode engine running time of the process, %
      GpuMemMB    GPU memory of the process: dedicated plus shared
    GPU figures come from the locale-independent Win32_PerfRawData_GPUPerformanceCounters_*
    classes. Decoding on the CPU shows up in CpuCore, decoding on the GPU in GpuDecode, so both
    have to be read together.

    Compatible with Windows PowerShell 5.1. ASCII only on purpose: PowerShell 5.1 reads BOM-less
    UTF-8 as the ANSI code page.

.EXAMPLE
    .\scripts\measure-video.ps1
    .\scripts\measure-video.ps1 -Clients ferrogram,Telegram -PlaySec 90
#>
[CmdletBinding()]
param(
    [string[]]$Clients = @('ferrogram', 'Telegram', 'AyuGram'),
    [int]$ChatSec = 30,
    [int]$PlaySec = 60,
    [int]$ClosedSec = 30,
    [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Out) {
    $Out = Join-Path $root ('target\bench\video-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
}
New-Item -ItemType Directory -Force -Path $Out | Out-Null

# One sample of every process with this name: memory, threads, CPU time, GPU counters.
function Get-Sample([string]$name) {
    $procs = @(Get-Process -Name $name -ErrorAction SilentlyContinue)
    if ($procs.Count -eq 0) { return $null }
    $prefixes = $procs | ForEach-Object { "pid_$($_.Id)_" }
    $cpuMs = 0.0
    foreach ($p in $procs) {
        try { $cpuMs += $p.TotalProcessorTime.TotalMilliseconds } catch { }
    }
    $engines = @{}
    foreach ($e in @(Get-CimInstance -ClassName Win32_PerfRawData_GPUPerformanceCounters_GPUEngine -ErrorAction SilentlyContinue)) {
        foreach ($prefix in $prefixes) {
            if ($e.Name.StartsWith($prefix)) {
                $engines[$e.Name] = @([double]$e.RunningTime, [double]$e.Timestamp_Sys100NS)
            }
        }
    }
    $gpuMem = 0.0
    foreach ($m in @(Get-CimInstance -ClassName Win32_PerfRawData_GPUPerformanceCounters_GPUProcessMemory -ErrorAction SilentlyContinue)) {
        foreach ($prefix in $prefixes) {
            if ($m.Name.StartsWith($prefix)) { $gpuMem += [double]$m.DedicatedUsage + [double]$m.SharedUsage }
        }
    }
    [pscustomobject]@{
        Time         = Get-Date
        Processes    = $procs.Count
        PrivateBytes = ($procs | Measure-Object -Property PrivateMemorySize64 -Sum).Sum
        WorkingSet   = ($procs | Measure-Object -Property WorkingSet64 -Sum).Sum
        Threads      = ($procs | ForEach-Object { $_.Threads.Count } | Measure-Object -Sum).Sum
        CpuMs        = $cpuMs
        Engines      = $engines
        GpuMem       = $gpuMem
    }
}

# Share of the interval an engine type was busy, from two samples of the raw running time.
function Get-EngineShare($previous, $current, [string]$type) {
    $share = 0.0
    foreach ($key in $current.Engines.Keys) {
        if (-not $key.EndsWith("engtype_$type")) { continue }
        if (-not $previous.Engines.ContainsKey($key)) { continue }
        $now = $current.Engines[$key]
        $before = $previous.Engines[$key]
        $span = $now[1] - $before[1]
        if ($span -gt 0) { $share += 100.0 * ($now[0] - $before[0]) / $span }
    }
    return $share
}

function Measure-Phase([string]$client, [string]$phase, [int]$seconds) {
    $rows = New-Object System.Collections.Generic.List[object]
    $previous = Get-Sample $client
    if ($null -eq $previous) { throw "$client is not running" }
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $next = $previous.Time.AddSeconds(1)
        $wait = ($next - (Get-Date)).TotalMilliseconds
        if ($wait -gt 0) { Start-Sleep -Milliseconds ([int]$wait) }
        $current = Get-Sample $client
        if ($null -eq $current) { throw "$client exited during the '$phase' phase" }
        $wall = ($current.Time - $previous.Time).TotalMilliseconds
        $rows.Add([pscustomobject]@{
            Client    = $client
            Phase     = $phase
            Time      = $current.Time.ToString('HH:mm:ss.fff')
            PrivateMB = [math]::Round($current.PrivateBytes / 1MB, 1)
            WorkingMB = [math]::Round($current.WorkingSet / 1MB, 1)
            Threads   = $current.Threads
            CpuCore   = [math]::Round(100.0 * ($current.CpuMs - $previous.CpuMs) / $wall, 2)
            Gpu3D     = [math]::Round((Get-EngineShare $previous $current '3D'), 2)
            GpuDecode = [math]::Round((Get-EngineShare $previous $current 'VideoDecode'), 2)
            GpuMemMB  = [math]::Round($current.GpuMem / 1MB, 1)
        })
        $previous = $current
    }
    return $rows
}

function Get-Summary($rows) {
    $avg = { param($p) [math]::Round(($rows | Measure-Object -Property $p -Average).Average, 1) }
    $max = { param($p) [math]::Round(($rows | Measure-Object -Property $p -Maximum).Maximum, 1) }
    [pscustomobject]@{
        Client      = $rows[0].Client
        Phase       = $rows[0].Phase
        Samples     = $rows.Count
        PrivateMB   = & $avg 'PrivateMB'
        PrivateMax  = & $max 'PrivateMB'
        WorkingMB   = & $avg 'WorkingMB'
        Threads     = & $avg 'Threads'
        CpuCore     = & $avg 'CpuCore'
        CpuCoreMax  = & $max 'CpuCore'
        Gpu3D       = & $avg 'Gpu3D'
        GpuDecode   = & $avg 'GpuDecode'
        GpuMemMB    = & $avg 'GpuMemMB'
    }
}

$all = New-Object System.Collections.Generic.List[object]
$summary = New-Object System.Collections.Generic.List[object]
$phases = @(
    @('chat', $ChatSec, 'Open the chat with the video in {0}. Do not play it yet. Press Enter'),
    @('playing', $PlaySec, 'Open the video in {0} and start it, then press Enter at once'),
    @('closed', $ClosedSec, 'Close the video viewer in {0} (keep the chat open), then press Enter')
)
foreach ($client in $Clients) {
    Write-Host ''
    Write-Host "=== $client ===" -ForegroundColor Cyan
    while (-not (Get-Process -Name $client -ErrorAction SilentlyContinue)) {
        $null = Read-Host "Start $client, then press Enter"
    }
    foreach ($phase in $phases) {
        $null = Read-Host ($phase[2] -f $client)
        Write-Host "  measuring '$($phase[0])' for $($phase[1]) s..."
        $rows = Measure-Phase $client $phase[0] $phase[1]
        foreach ($row in $rows) { $all.Add($row) }
        $s = Get-Summary $rows
        $summary.Add($s)
        Write-Host ('  {0,-8} private {1} MB (max {2}), CPU {3} % of a core, GPU 3D {4} %, decode {5} %, GPU memory {6} MB' -f `
            $s.Phase, $s.PrivateMB, $s.PrivateMax, $s.CpuCore, $s.Gpu3D, $s.GpuDecode, $s.GpuMemMB)
    }
}
$all | Export-Csv -NoTypeInformation -Path (Join-Path $Out 'samples.csv')
$summary | Export-Csv -NoTypeInformation -Path (Join-Path $Out 'summary.csv')
Write-Host ''
$summary | Format-Table -AutoSize | Out-String -Width 220 | Write-Host
Write-Host "Samples and summary: $Out"
