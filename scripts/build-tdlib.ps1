<#
.SYNOPSIS
    Build TDLib (tdjson.dll) from the official sources at a pinned commit.

.DESCRIPTION
    Follows the official Windows build instructions (https://tdlib.github.io/td/build.html):
    vcpkg provides gperf, OpenSSL and zlib; CMake builds TDLib with MSVC.
    Sources and build output live outside the repository. ASCII-only on purpose.

    Sources:  https://github.com/tdlib/td      (official TDLib repository)
              https://github.com/microsoft/vcpkg (official vcpkg repository)

.EXAMPLE
    .\scripts\build-tdlib.ps1
#>
[CmdletBinding()]
param(
    [string]$Root = '',
    [string]$Vcpkg = '',
    # Pinned commit of github.com/tdlib/td (master as of 2026-11-16, TDLib 1.8.70).
    [string]$Commit = '5aef7d4036be9bb18e3c78f6709da81f47bb7a70',
    [int]$Jobs = 6,
    # Leave empty to auto-detect: PATH first, then the CMake bundled with Visual Studio.
    [string]$CMake = ''
)
# Defaults: next to the repository directory. $PSScriptRoot is empty in param defaults on PS 5.1.
$parent = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if ($Root -eq '') { $Root = [IO.Path]::GetFullPath((Join-Path $parent '..\tdlib')) }
if ($Vcpkg -eq '') { $Vcpkg = [IO.Path]::GetFullPath((Join-Path $parent '..\vcpkg')) }

function Step([string]$name) { "=== $(Get-Date -Format HH:mm:ss) $name" }
function Check([string]$what) { if ($LASTEXITCODE -ne 0) { throw "$what failed with exit code $LASTEXITCODE" } }

if ($CMake -eq '') {
    $found = Get-Command cmake -ErrorAction SilentlyContinue
    if ($found) {
        $CMake = $found.Source
    } else {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path $vswhere) {
            $vs = & $vswhere -latest -products * -property installationPath
            $candidate = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
            if (Test-Path $candidate) { $CMake = $candidate }
        }
    }
    if ($CMake -eq '') { throw 'CMake not found: install it or pass -CMake <path>' }
}
"cmake: $CMake"

$src = Join-Path $Root 'td'
$build = Join-Path $Root 'build'
$install = Join-Path $Root 'install'
New-Item -ItemType Directory -Force $Root | Out-Null

Step 'vcpkg: clone'
if (-not (Test-Path $Vcpkg)) {
    git clone --depth 1 https://github.com/microsoft/vcpkg.git $Vcpkg
    Check 'git clone vcpkg'
}
Step 'vcpkg: bootstrap'
if (-not (Test-Path (Join-Path $Vcpkg 'vcpkg.exe'))) {
    & (Join-Path $Vcpkg 'bootstrap-vcpkg.bat') -disableMetrics
    Check 'bootstrap-vcpkg'
}
Step 'vcpkg: install gperf openssl zlib'
& (Join-Path $Vcpkg 'vcpkg.exe') install gperf:x64-windows openssl:x64-windows zlib:x64-windows
Check 'vcpkg install'

Step "tdlib: fetch $Commit"
if (-not (Test-Path (Join-Path $src '.git'))) {
    New-Item -ItemType Directory -Force $src | Out-Null
    git -C $src init -q
    Check 'git init'
    git -C $src remote add origin https://github.com/tdlib/td.git
    Check 'git remote add'
}
git -C $src fetch --depth 1 origin $Commit
Check 'git fetch'
git -C $src checkout -q FETCH_HEAD
Check 'git checkout'
"checked out: $(git -C $src rev-parse HEAD)"

Step 'tdlib: configure'
& $CMake -S $src -B $build -A x64 `
    "-DCMAKE_INSTALL_PREFIX:PATH=$install" `
    "-DCMAKE_TOOLCHAIN_FILE:FILEPATH=$(Join-Path $Vcpkg 'scripts\buildsystems\vcpkg.cmake')"
Check 'cmake configure'

Step "tdlib: build (Release, $Jobs jobs)"
& $CMake --build $build --target install --config Release --parallel $Jobs
Check 'cmake build'

Step 'done'
Get-ChildItem (Join-Path $install 'bin') | Select-Object Name, Length | Format-Table -AutoSize | Out-String
Get-FileHash (Join-Path $install 'bin\tdjson.dll') -Algorithm SHA256 | Format-List | Out-String
