<#
.SYNOPSIS
    Run the TDLib probe with the TDLib DLLs on PATH.

.DESCRIPTION
    Run this from your own terminal: on first start it asks for the phone number, the login
    code and, if enabled, the two-step password. Later starts reuse the encrypted session.
    ASCII-only on purpose (PowerShell 5.1 reads BOM-less UTF-8 as ANSI).

.EXAMPLE
    .\spikes\tdlib-probe\run.ps1
    .\spikes\tdlib-probe\run.ps1 -RunSec 700
#>
[CmdletBinding()]
param(
    [int]$RunSec = 900,
    [switch]$Qr,
    [string]$TdlibDir = ''
)
if ($TdlibDir -eq '') { $TdlibDir = [IO.Path]::GetFullPath((Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..\..\..\tdlib\install')) }

$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root 'target\release\tdlib-probe.exe'
if (-not (Test-Path $exe)) { throw "Build first: cargo build --release ($exe not found)" }
$env:Path = (Join-Path $TdlibDir 'bin') + ';' + $env:Path
Push-Location (Join-Path $root '..\..')
$probeArgs = @('--run-sec', $RunSec)
if ($Qr) { $probeArgs += '--qr' }
try { & $exe @probeArgs } finally { Pop-Location }
