<#
.SYNOPSIS
    Builds the portable Veygram release: one zip with veygram.exe, TDLib, its libraries,
    the Visual C++ runtime and the license texts.

.DESCRIPTION
    Telegram clients that are distributed carry their own application credentials
    (api_id and api_hash from my.telegram.org). This script reads them from .env in the
    repository root, passes them to the build as VEYGRAM_BUILD_API_ID and
    VEYGRAM_BUILD_API_HASH and never prints them. The build uses its own target
    directory, so development builds never contain them.

    The zip unpacks into one folder. veygram.exe loads tdjson.dll from its own folder;
    tdjson.dll finds OpenSSL and zlib there too. The Visual C++ runtime DLLs are copied
    app-locally, as Microsoft allows, so a clean Windows 10 or 11 x64 needs nothing else.

.EXAMPLE
    .\scripts\package-release.ps1 -TdlibBin E:\tdlib\install\bin -CrtDir E:\vsst\VC\Redist\MSVC\14.51.36231\x64\Microsoft.VC145.CRT -LicenseDir E:\vcpkg\installed\x64-windows\share
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$TdlibBin,
    [Parameter(Mandatory = $true)][string]$CrtDir,
    # vcpkg share directory with openssl\copyright and zlib\copyright.
    [Parameter(Mandatory = $true)][string]$LicenseDir,
    [string]$Out = ''
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Out) { $Out = Join-Path $root 'target-release\dist' }

$manifest = Get-Content (Join-Path $root 'crates\app\Cargo.toml') -Raw
$versionLine = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"')
if ($versionLine.Success) {
    $version = $versionLine.Groups[1].Value
} else {
    $workspace = Get-Content (Join-Path $root 'Cargo.toml') -Raw
    $version = [regex]::Match($workspace, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
}
if (-not $version) { throw 'Could not read the version from Cargo.toml.' }

$tdlibFiles = 'tdjson.dll', 'libcrypto-3-x64.dll', 'libssl-3-x64.dll', 'libz.dll'
$crtFiles = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll'
foreach ($name in $tdlibFiles) {
    if (-not (Test-Path (Join-Path $TdlibBin $name))) { throw "Missing $name in $TdlibBin" }
}
foreach ($name in $crtFiles) {
    if (-not (Test-Path (Join-Path $CrtDir $name))) { throw "Missing $name in $CrtDir" }
}
$licenses = @{
    'Veygram-GPL-3.0.txt' = Join-Path $root 'LICENSE'
    'TDLib-BSL-1.0.txt' = Join-Path (Split-Path -Parent (Split-Path -Parent $TdlibBin)) 'td\LICENSE_1_0.txt'
    'OpenSSL.txt' = Join-Path $LicenseDir 'openssl\copyright'
    'zlib.txt' = Join-Path $LicenseDir 'zlib\copyright'
}
foreach ($source in $licenses.Values) {
    if (-not (Test-Path $source)) { throw "Missing license text $source" }
}

# Read only the two credential values; nothing from .env is printed or written elsewhere.
$envFile = Join-Path $root '.env'
if (-not (Test-Path $envFile)) { throw 'Put TG_API_ID and TG_API_HASH into .env first.' }
$id = $null
$hash = $null
foreach ($line in Get-Content $envFile) {
    $text = $line.Trim()
    if ($text.StartsWith('export ')) { $text = $text.Substring(7).Trim() }
    $parts = $text.Split('=', 2)
    if ($parts.Count -ne 2) { continue }
    $value = $parts[1].Trim().Trim('"').Trim("'")
    switch ($parts[0].Trim()) {
        'TG_API_ID' { $id = $value }
        'TG_API_HASH' { $hash = $value }
    }
}
if (-not $id -or -not $hash) { throw '.env must contain TG_API_ID and TG_API_HASH.' }

$target = Join-Path $root 'target-release'
$env:VEYGRAM_BUILD_API_ID = $id
$env:VEYGRAM_BUILD_API_HASH = $hash
$previousTarget = $env:CARGO_TARGET_DIR
$env:CARGO_TARGET_DIR = $target
try {
    Push-Location $root
    # PowerShell 5.1 turns cargo's progress on stderr into errors under 'Stop'; the exit
    # code decides instead.
    $ErrorActionPreference = 'Continue'
    & cargo build --release -p tg-app --locked 2>&1 | ForEach-Object { "$_" }
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw "cargo build failed with exit code $code" }
} finally {
    Pop-Location
    Remove-Item Env:VEYGRAM_BUILD_API_ID -ErrorAction SilentlyContinue
    Remove-Item Env:VEYGRAM_BUILD_API_HASH -ErrorAction SilentlyContinue
    $env:CARGO_TARGET_DIR = $previousTarget
    $id = $null
    $hash = $null
}

$name = "Veygram-$version-windows-x64"
$stage = Join-Path $Out $name
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $stage 'licenses') | Out-Null
Copy-Item (Join-Path $target 'release\veygram.exe') $stage
foreach ($file in $tdlibFiles) { Copy-Item (Join-Path $TdlibBin $file) $stage }
foreach ($file in $crtFiles) { Copy-Item (Join-Path $CrtDir $file) $stage }
foreach ($entry in $licenses.GetEnumerator()) {
    Copy-Item $entry.Value (Join-Path $stage "licenses\$($entry.Key)")
}
$commit = (& git -C $root rev-parse HEAD).Trim()
$readme = @"
Veygram $version for Windows 10 and 11 (x64)

Unpack the whole folder anywhere and start veygram.exe. Keep the DLL files next to it.
Your session and cache are stored in %LOCALAPPDATA%\tg-rust.

Veygram is an unofficial Telegram client, licensed under GPL-3.0-or-later.
Source code of this build: https://github.com/tikD1337/veygram/tree/$commit

Included third-party components:
- TDLib (Boost Software License 1.0): tdjson.dll
- OpenSSL (Apache License 2.0): libcrypto-3-x64.dll, libssl-3-x64.dll
- zlib (zlib license): libz.dll
- Microsoft Visual C++ runtime (redistributable): vcruntime140.dll, vcruntime140_1.dll, msvcp140.dll
License texts are in the licenses folder.
"@
Set-Content -Path (Join-Path $stage 'README.txt') -Value $readme -Encoding ASCII

$zip = Join-Path $Out "$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $stage -DestinationPath $zip
$sha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -Path "$zip.sha256" -Value "$sha  $name.zip" -Encoding ASCII
Write-Host "Release: $zip"
Write-Host "SHA-256: $sha"
