<#
.SYNOPSIS
    Measure one owned demo-scroll process by PID, without including the live client.
#>
[CmdletBinding()]
param(
    [string]$Binary = 'target/release/ferrogram.exe',
    [ValidateRange(1, 3600)][int]$DurationSec = 120,
    [string]$Csv = 'target/stage3-m1-scroll-memory.csv'
)
$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$process = Start-Process -FilePath $binaryPath -ArgumentList '--demo-scroll', $DurationSec -WindowStyle Hidden -PassThru
$clock = [System.Diagnostics.Stopwatch]::StartNew()
# At most DurationSec + 31 samples including the exit grace period, capped at 3,631.
$rows = New-Object System.Collections.Generic.List[object]
try {
    while (-not $process.WaitForExit(1000)) {
        $process.Refresh()
        $rows.Add([pscustomobject]@{
            Seconds = [math]::Round($clock.Elapsed.TotalSeconds, 2)
            PrivateBytesMB = [math]::Round($process.PrivateMemorySize64 / 1MB, 3)
            WorkingSetMB = [math]::Round($process.WorkingSet64 / 1MB, 3)
            CpuMs = $process.TotalProcessorTime.TotalMilliseconds
            Threads = $process.Threads.Count
            Handles = $process.HandleCount
        })
        if ($clock.Elapsed.TotalSeconds -gt $DurationSec + 30) {
            throw 'Demo did not exit within its duration plus 30 seconds.'
        }
    }
    $process.WaitForExit()
    $rows | Export-Csv -LiteralPath $Csv -NoTypeInformation -Encoding UTF8
    "Binary: $binaryPath"
    "SHA256: $((Get-FileHash -LiteralPath $binaryPath -Algorithm SHA256).Hash)"
    "Exit: $($process.ExitCode); duration: $([math]::Round($clock.Elapsed.TotalSeconds, 2)) s; samples: $($rows.Count)"
    foreach ($range in @(@(0, 30), @(30, 60), @(60, 90), @(90, 120))) {
        $samples = @($rows | Where-Object { $_.Seconds -gt $range[0] -and $_.Seconds -le $range[1] })
        if ($samples.Count -gt 0) {
            $stat = $samples | Measure-Object PrivateBytesMB -Average -Minimum -Maximum
            '{0}-{1}s Private Bytes MB: avg {2:N3}, min {3:N3}, max {4:N3}' -f $range[0], $range[1], $stat.Average, $stat.Minimum, $stat.Maximum
        }
    }
    if ($process.ExitCode -ne 0) { throw 'Demo exited with a nonzero status.' }
} finally {
    # Stop only this script's child if it failed to exit; never stop the live client.
    if (-not $process.HasExited) { Stop-Process -Id $process.Id }
    $process.Dispose()
}
