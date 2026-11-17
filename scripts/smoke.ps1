<#
.SYNOPSIS
    Fast end-to-end checks of the real client: it starts, renders, plays media, protects
    its process and recovers from a refused key. No account or network needed.

.DESCRIPTION
    Every scenario starts ferrogram.exe with --check, which writes a JSON account of what
    really happened (chats loaded, how far the video played, errors), and asserts on it.

    Default (local, about half a minute after the build): demo, video, voice, recovery.
    -Full adds round, stickers, scroll and oled (used by CI).
    -Scenario runs only the named scenarios (CI runs each one as a separate check).

    ASCII only: Windows PowerShell 5.1 reads scripts without a BOM as the ANSI code page.

.EXAMPLE
    .\scripts\smoke.ps1
.EXAMPLE
    .\scripts\smoke.ps1 -Full
.EXAMPLE
    .\scripts\smoke.ps1 -Exe target\release\ferrogram.exe -Scenario video -Out smoke
#>
[CmdletBinding()]
param(
    [string]$Exe = '',
    [string[]]$Scenario = @(),
    [switch]$Full,
    [string]$Out = '',
    [switch]$NoBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$quick = @('demo', 'video', 'voice', 'recovery')
$extra = @('round', 'stickers', 'scroll', 'oled')
if ($Scenario.Count -gt 0) {
    $selected = @($Scenario | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
} elseif ($Full) {
    $selected = $quick + $extra
} else {
    $selected = $quick
}
foreach ($name in $selected) {
    if (($quick + $extra) -notcontains $name) { throw "Unknown scenario '$name'" }
}

if (-not $Exe) {
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }
    $Exe = Join-Path $target 'debug\ferrogram.exe'
    if (-not $NoBuild) {
        Write-Host 'Building the debug client...'
        Push-Location $root
        try {
            cargo build -p tg-app --quiet
            if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
        } finally { Pop-Location }
    }
}
$Exe = (Resolve-Path -LiteralPath $Exe).Path
if (-not $Out) { $Out = Join-Path ([IO.Path]::GetTempPath()) ('ferrogram-smoke-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)) }
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Out = (Resolve-Path -LiteralPath $Out).Path

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class FerrogramSmoke {
    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll")]
    public static extern bool CloseHandle(IntPtr handle);
    // True when a handle with this access could be opened (and closes it again).
    public static bool CanOpen(uint pid, uint access) {
        IntPtr h = OpenProcess(access, false, pid);
        if (h == IntPtr.Zero) { return false; }
        CloseHandle(h);
        return true;
    }
}
'@

$elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$audio = @(Get-PnpDevice -Class AudioEndpoint -Status OK -ErrorAction SilentlyContinue).Count -gt 0

# Starts the client, optionally probes it while it runs, waits with a hard limit, returns the report.
function Invoke-Client([string]$name, [string[]]$arguments, [int]$limitSec, [scriptblock]$whileRunning) {
    $check = Join-Path $Out "$name.json"
    Remove-Item -LiteralPath $check -ErrorAction SilentlyContinue
    $all = @($arguments) + @('--check', $check)
    $started = Get-Date
    $process = Start-Process -FilePath $Exe -ArgumentList $all -PassThru
    if ($whileRunning) { & $whileRunning $process }
    if (-not $process.WaitForExit($limitSec * 1000)) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        throw "$name did not finish within $limitSec s"
    }
    $process.WaitForExit()
    if ($process.ExitCode -ne 0) { throw "$name exited with code $($process.ExitCode)" }
    if (-not (Test-Path -LiteralPath $check)) { throw "$name wrote no check report" }
    $report = Get-Content -LiteralPath $check -Raw | ConvertFrom-Json
    $report | Add-Member -NotePropertyName seconds -NotePropertyValue ([math]::Round(((Get-Date) - $started).TotalSeconds, 1))
    return $report
}

function Assert-True([bool]$condition, [string]$message) {
    if (-not $condition) { throw $message }
}

function Assert-Snapshot([string]$path) {
    Assert-True (Test-Path -LiteralPath $path) "no snapshot at $path"
    # A blank or failed frame compresses to a few kilobytes; a rendered window does not.
    $size = (Get-Item -LiteralPath $path).Length
    Assert-True ($size -gt 20000) "snapshot looks empty ($size bytes)"
}

function Assert-Feed($report) {
    Assert-True ($report.ready) 'the main window did not become ready'
    Assert-True ($report.chats -ge 4) "only $($report.chats) chats loaded"
    Assert-True ($report.messages -gt 0) 'no messages loaded in the open chat'
}

$scenarios = @{
    # The demo starts, loads the chat list and the history, and renders a frame.
    demo = {
        $png = Join-Path $Out 'demo.png'
        $r = Invoke-Client 'demo' @('--demo', '--snapshot', $png) 60
        Assert-Feed $r
        Assert-Snapshot $png
        "ready, $($r.chats) chats, $($r.messages) messages"
    }
    # The viewer plays the demo video on Media Foundation, and the hardened process refuses
    # memory reads from this (same-user) script.
    video = {
        $probe = {
            param($process)
            Start-Sleep -Seconds 3
            $script:memoryRead = [FerrogramSmoke]::CanOpen([uint32]$process.Id, 0x0010)
            $script:query = [FerrogramSmoke]::CanOpen([uint32]$process.Id, 0x1000)
        }
        $r = Invoke-Client 'video' @('--demo-video', '7') 60 $probe
        Assert-True (-not $r.video.failed) 'video playback reported a failure'
        Assert-True ($r.video.max_position_ms -ge 1500) "video reached only $($r.video.max_position_ms) ms"
        Assert-True $script:query 'the process cannot even be queried'
        if ($elevated) {
            $access = 'not checked (elevated: administrators bypass the process DACL)'
        } else {
            Assert-True (-not $script:memoryRead) 'another process could open the client for memory reads'
            $access = 'memory reads denied'
        }
        "played to $($r.video.max_position_ms) ms; $access"
    }
    # A voice message plays through the audio-only Media Engine.
    voice = {
        if (-not $audio) { return 'SKIPPED: no audio output device on this machine' }
        $r = Invoke-Client 'voice' @('--demo-voice', '7') 60
        Assert-True (-not $r.audio.failed) 'voice playback reported a failure'
        Assert-True ($r.audio.max_position_ms -ge 1500) "voice reached only $($r.audio.max_position_ms) ms"
        "played to $($r.audio.max_position_ms) ms"
    }
    # A round video plays inline in its circle.
    round = {
        if (-not $audio) { return 'SKIPPED: no audio output device on this machine' }
        $r = Invoke-Client 'round' @('--demo-round', '7') 60
        Assert-True (-not $r.audio.failed) 'round playback reported a failure'
        Assert-True ($r.audio.max_position_ms -ge 1500) "round reached only $($r.audio.max_position_ms) ms"
        "played to $($r.audio.max_position_ms) ms"
    }
    # The sticker chat renders.
    stickers = {
        $png = Join-Path $Out 'stickers.png'
        $r = Invoke-Client 'stickers' @('--demo-stickers', '--snapshot', $png) 60
        Assert-Feed $r
        Assert-Snapshot $png
        "$($r.messages) messages"
    }
    # Fast scrolling through the long history keeps memory bounded (pages are dropped).
    scroll = {
        $probe = {
            param($process)
            $script:samples = @()
            while (-not $process.HasExited) {
                Start-Sleep -Milliseconds 500
                $process.Refresh()
                if (-not $process.HasExited) { $script:samples += $process.PrivateMemorySize64 }
            }
        }
        $r = Invoke-Client 'scroll' @('--demo-scroll', '12') 60 $probe
        Assert-Feed $r
        $early = $script:samples[[math]::Min(7, $script:samples.Count - 1)]
        $peak = ($script:samples | Measure-Object -Maximum).Maximum
        Assert-True ($peak -lt 400MB) ('peak private memory {0:N0} MB' -f ($peak / 1MB))
        Assert-True ($peak -lt 1.6 * $early) ('memory grew from {0:N0} MB to {1:N0} MB while scrolling' -f ($early / 1MB), ($peak / 1MB))
        '{0:N0} MB after 4 s, peak {1:N0} MB' -f ($early / 1MB), ($peak / 1MB)
    }
    # The OLED theme renders.
    oled = {
        $png = Join-Path $Out 'oled.png'
        $r = Invoke-Client 'oled' @('--demo', '--theme', 'oled', '--snapshot', $png) 60
        Assert-Feed $r
        Assert-Snapshot $png
        'rendered'
    }
    # A key file that cannot be unlocked shows the recovery screen instead of failing. Runs the
    # real (non-demo) start path with throwaway credentials; TDLib is never reached.
    recovery = {
        $data = Join-Path $Out 'locked-data'
        New-Item -ItemType Directory -Force -Path $data | Out-Null
        [IO.File]::WriteAllBytes((Join-Path $data 'database-key.bin'), [Text.Encoding]::ASCII.GetBytes('not a key file'))
        $saved = @{ TG_DATA_DIR = $env:TG_DATA_DIR; TG_API_ID = $env:TG_API_ID; TG_API_HASH = $env:TG_API_HASH }
        try {
            $env:TG_DATA_DIR = $data
            $env:TG_API_ID = '1'
            $env:TG_API_HASH = '0123456789abcdef0123456789abcdef'
            $png = Join-Path $Out 'recovery.png'
            $r = Invoke-Client 'recovery' @('--snapshot', $png) 60
        } finally {
            foreach ($key in $saved.Keys) { Set-Item -Path "Env:$key" -Value $saved[$key] -ErrorAction SilentlyContinue; if ($null -eq $saved[$key]) { Remove-Item "Env:$key" -ErrorAction SilentlyContinue } }
        }
        Assert-True ($r.auth_step -eq 5) "expected the recovery screen, got auth step $($r.auth_step)"
        Assert-True (Test-Path -LiteralPath (Join-Path $data 'database-key.bin')) 'the key file was touched'
        Assert-Snapshot $png
        'recovery screen shown, key file untouched'
    }
}

$failed = 0
$total = Get-Date
Write-Host ("Smoke test of {0}" -f $Exe)
foreach ($name in $selected) {
    $start = Get-Date
    try {
        $detail = & $scenarios[$name]
        $seconds = [math]::Round(((Get-Date) - $start).TotalSeconds, 1)
        if ("$detail" -like 'SKIPPED*') {
            Write-Host ("  SKIP  {0,-9} {1,5} s  {2}" -f $name, $seconds, $detail) -ForegroundColor Yellow
            # Shown as a warning on the GitHub check, so a skip is never mistaken for a pass.
            if ($env:GITHUB_ACTIONS) { Write-Host "::warning title=Smoke ($name) skipped::$detail" }
        } else {
            Write-Host ("  PASS  {0,-9} {1,5} s  {2}" -f $name, $seconds, $detail) -ForegroundColor Green
        }
    } catch {
        $failed++
        $seconds = [math]::Round(((Get-Date) - $start).TotalSeconds, 1)
        Write-Host ("  FAIL  {0,-9} {1,5} s  {2}" -f $name, $seconds, $_.Exception.Message) -ForegroundColor Red
    }
}
Write-Host ("{0} of {1} passed in {2:N0} s. Reports and snapshots: {3}" -f ($selected.Count - $failed), $selected.Count, ((Get-Date) - $total).TotalSeconds, $Out)
if ($failed -gt 0) { exit 1 }
