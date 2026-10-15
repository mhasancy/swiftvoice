#Requires -Version 5.1
<#
.SYNOPSIS
    Build, test and package SwiftVoice for Windows x64.

.DESCRIPTION
    1. Checks the toolchain: Rust >= 1.92 with the MSVC target, and CMake for
       the whisper.cpp fallback (the Visual Studio-bundled CMake is used
       when cmake is not on PATH).
    2. cargo build --release --locked, and cargo test unless -SkipTests.
    3. Downloads Microsoft's official ONNX Runtime 1.31.0 CPU build from
       GitHub releases (or uses -OrtZip), verifies its SHA-256, and extracts
       onnxruntime.dll and its licence files.
    4. Copies the Visual C++ runtime DLLs that onnxruntime.dll and
       swiftvoice.exe need from the Visual Studio redistributable folder
       (app-local deployment), so users need no separate VC++ installer.
    5. Assembles dist\SwiftVoice\, zips it to
       dist\SwiftVoice-<version>-win-x64.zip, smoke-tests the packaged exe,
       and prints sizes and SHA-256.

.PARAMETER NoWhisper
    Build without the whisper.cpp fallback (no CMake needed; Bengali and
    other non-Parakeet languages are then unavailable).

.PARAMETER SkipTests
    Do not run cargo test.

.PARAMETER Portable
    Add swiftvoice.portable to the package: settings, logs and models are
    then kept next to swiftvoice.exe instead of %APPDATA% / %LOCALAPPDATA%.

.PARAMETER OrtZip
    Path to an already downloaded onnxruntime-win-x64-1.31.0.zip (offline
    builds). It is verified against the pinned SHA-256 like a download.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\build-windows.ps1
.EXAMPLE
    pwsh scripts/build-windows.ps1 -NoWhisper -SkipTests
#>
[CmdletBinding()]
param(
    [switch]$NoWhisper,
    [switch]$SkipTests,
    [switch]$Portable,
    [string]$OrtZip = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is far faster without the progress bar

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$OrtVersion = '1.31.0'
$OrtUrl = "https://github.com/microsoft/onnxruntime/releases/download/v$OrtVersion/onnxruntime-win-x64-$OrtVersion.zip"
$OrtZipSha256 = '3958D8A44984160692C1B4FFD0DE3CB48F8C39F9D34BB31E433DB06E30B82195'
$OrtZipBytes = 82887349
$OrtDllSha256 = '0C42DC4E278F0968FDF9EBF1B1E54A74C657A9942E70803E453C91993451CF4C'
$VcRuntimeDlls = @('msvcp140.dll', 'msvcp140_1.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')
$MaxInstallMB = 30

function Step($text) { Write-Host "`n==> $text" -ForegroundColor Cyan }
function Fail($text) { Write-Host "`nERROR: $text" -ForegroundColor Red; exit 1 }

function Invoke-Checked {
    param([string]$Exe, [string[]]$Arguments)
    Write-Host "    $Exe $($Arguments -join ' ')" -ForegroundColor DarkGray
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) { Fail "$Exe $($Arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

# Run a native program and capture stdout+stderr as text. Windows PowerShell
# 5.1 turns stderr lines into terminating errors under 'Stop', so relax the
# preference for the call.
function Invoke-Capture {
    param([string]$Exe, [string[]]$Arguments)
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $text = (& $Exe @Arguments 2>&1 | ForEach-Object { "$_" }) -join "`n"
        return [pscustomobject]@{ Code = $LASTEXITCODE; Text = $text }
    } finally {
        $ErrorActionPreference = $saved
    }
}

function Get-Sha256($path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToUpperInvariant() }

function Find-VisualStudio {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $null }
    $path = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($LASTEXITCODE -ne 0 -or -not $path) { return $null }
    return ($path | Select-Object -First 1)
}

# --- 1. Toolchain ---------------------------------------------------------
Step 'Checking the toolchain'
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Fail 'cargo was not found. Install Rust from https://rustup.rs (choose the default x86_64-pc-windows-msvc toolchain) and reopen the terminal.'
}
$rustc = (& rustc --version)
if ($rustc -notmatch 'rustc (\d+)\.(\d+)') { Fail "Cannot parse '$rustc'" }
if ([int]$Matches[1] -lt 1 -or ([int]$Matches[1] -eq 1 -and [int]$Matches[2] -lt 92)) {
    Fail "$rustc is too old; SwiftVoice needs Rust 1.92 or newer (run: rustup update stable)."
}
$hostTriple = ((& rustc -vV) | Select-String '^host: (.+)$').Matches[0].Groups[1].Value
if ($hostTriple -ne 'x86_64-pc-windows-msvc') {
    Fail "Rust host is $hostTriple; install the MSVC toolchain: rustup default stable-x86_64-pc-windows-msvc"
}
Write-Host "    $rustc ($hostTriple)"

$vs = Find-VisualStudio
if (-not $vs) {
    Fail 'Visual Studio (or Build Tools) with the "Desktop development with C++" workload was not found. Install it from https://visualstudio.microsoft.com/downloads/ .'
}
Write-Host "    Visual Studio: $vs"

if (-not $NoWhisper) {
    if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
        $vsCmake = Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
        if (Test-Path $vsCmake) {
            $env:CMAKE = $vsCmake
            $env:PATH = (Split-Path $vsCmake) + ';' + $env:PATH
            Write-Host "    CMake: $vsCmake (bundled with Visual Studio)"
        } else {
            Fail 'CMake is needed to build the whisper.cpp fallback. Install "C++ CMake tools for Windows" in the Visual Studio Installer or CMake from https://cmake.org/download/ , or build without it: -NoWhisper'
        }
    } else {
        Write-Host "    CMake: $((Get-Command cmake).Source)"
    }
}

# --- 2. Build and test ----------------------------------------------------
$features = @()
if ($NoWhisper) { $features = @('--no-default-features') }

Step 'Building (release)'
Invoke-Checked cargo (@('build', '--release', '--locked') + $features)

if (-not $SkipTests) {
    Step 'Running tests'
    Invoke-Checked cargo (@('test', '--release', '--locked') + $features)
}

$exe = Join-Path $Root 'target\release\swiftvoice.exe'
$launcher = Join-Path $Root 'target\release\swiftvoice-com.exe'
foreach ($f in $exe, $launcher) { if (-not (Test-Path $f)) { Fail "$f was not produced" } }
$version = ((Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value)

# --- 3. ONNX Runtime ------------------------------------------------------
Step "ONNX Runtime $OrtVersion"
$cache = Join-Path $Root 'target\ort-cache'
New-Item -ItemType Directory -Force -Path $cache | Out-Null
if ($OrtZip) {
    if (-not (Test-Path $OrtZip)) { Fail "-OrtZip $OrtZip does not exist" }
    $zipPath = (Resolve-Path $OrtZip).Path
} else {
    $zipPath = Join-Path $cache "onnxruntime-win-x64-$OrtVersion.zip"
    if (-not (Test-Path $zipPath) -or (Get-Sha256 $zipPath) -ne $OrtZipSha256) {
        Write-Host "    downloading $OrtUrl"
        $partial = "$zipPath.part"
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $OrtUrl -OutFile $partial -UseBasicParsing
        Move-Item -Force $partial $zipPath
    }
}
$actualBytes = (Get-Item $zipPath).Length
$actualSha = Get-Sha256 $zipPath
if ($actualSha -ne $OrtZipSha256) {
    Fail "ONNX Runtime archive $zipPath has SHA-256 $actualSha ($actualBytes bytes); expected $OrtZipSha256 ($OrtZipBytes bytes). Delete it and retry."
}
Write-Host "    verified $zipPath (SHA-256 $actualSha)"

Add-Type -AssemblyName System.IO.Compression.FileSystem
$ortDir = Join-Path $cache "onnxruntime-$OrtVersion"
New-Item -ItemType Directory -Force -Path $ortDir | Out-Null
$wanted = @{
    "onnxruntime-win-x64-$OrtVersion/lib/onnxruntime.dll"     = 'onnxruntime.dll'
    "onnxruntime-win-x64-$OrtVersion/LICENSE"                 = 'LICENSE'
    "onnxruntime-win-x64-$OrtVersion/ThirdPartyNotices.txt"   = 'ThirdPartyNotices.txt'
    "onnxruntime-win-x64-$OrtVersion/Privacy.md"              = 'Privacy.md'
}
$zip = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    foreach ($entry in $zip.Entries) {
        if ($wanted.ContainsKey($entry.FullName)) {
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $ortDir $wanted[$entry.FullName]), $true)
        }
    }
} finally {
    $zip.Dispose()
}
foreach ($name in $wanted.Values) {
    if (-not (Test-Path (Join-Path $ortDir $name))) { Fail "$name is missing from the ONNX Runtime archive" }
}
$dllSha = Get-Sha256 (Join-Path $ortDir 'onnxruntime.dll')
if ($dllSha -ne $OrtDllSha256) { Fail "onnxruntime.dll has SHA-256 $dllSha, expected $OrtDllSha256" }

# --- 4. Visual C++ runtime (app-local) ------------------------------------
Step 'Visual C++ runtime DLLs'
$crtDir = $null
$redistRoot = Join-Path $vs 'VC\Redist\MSVC'
if (Test-Path $redistRoot) {
    $crtDir = Get-ChildItem $redistRoot -Directory |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Get-ChildItem (Join-Path $_.FullName 'x64') -Directory -Filter 'Microsoft.VC*.CRT' -ErrorAction SilentlyContinue } |
        Where-Object { Test-Path (Join-Path $_.FullName 'vcruntime140.dll') } |
        Select-Object -First 1
}
if (-not $crtDir) {
    Fail "The Visual C++ redistributable files were not found under $redistRoot. Install the 'MSVC v14x - VS C++ x64/x86 build tools' component, which includes VC\Redist."
}
Write-Host "    from $($crtDir.FullName)"

# --- 5. Package -----------------------------------------------------------
Step 'Assembling dist\SwiftVoice'
$dist = Join-Path $Root 'dist'
$pkg = Join-Path $dist 'SwiftVoice'
if (Test-Path $pkg) { Remove-Item -Recurse -Force $pkg }
New-Item -ItemType Directory -Force -Path $pkg, (Join-Path $pkg 'licenses'), (Join-Path $pkg 'docs') | Out-Null

Copy-Item $exe $pkg
# Console front end: `swiftvoice ...` in a terminal runs swiftvoice.com first
# (PATHEXT), which waits for swiftvoice.exe and relays its exit code.
Copy-Item $launcher (Join-Path $pkg 'swiftvoice.com')
Copy-Item (Join-Path $ortDir 'onnxruntime.dll') $pkg
foreach ($dll in $VcRuntimeDlls) {
    $src = Join-Path $crtDir.FullName $dll
    if (-not (Test-Path $src)) { Fail "$dll is missing from $($crtDir.FullName)" }
    Copy-Item $src $pkg
}
Copy-Item (Join-Path $Root 'README.md'), (Join-Path $Root 'LICENSE') $pkg
Copy-Item (Join-Path $ortDir 'LICENSE') (Join-Path $pkg 'licenses\ONNXRuntime-LICENSE.txt')
Copy-Item (Join-Path $ortDir 'ThirdPartyNotices.txt') (Join-Path $pkg 'licenses\ONNXRuntime-ThirdPartyNotices.txt')
Copy-Item (Join-Path $ortDir 'Privacy.md') (Join-Path $pkg 'licenses\ONNXRuntime-Privacy.md')
foreach ($doc in 'CONFIGURATION.md', 'TROUBLESHOOTING.md', 'BENCHMARKS.md') {
    Copy-Item (Join-Path $Root "docs\$doc") (Join-Path $pkg 'docs')
}
Copy-Item (Join-Path $Root 'models\README.md') (Join-Path $pkg 'docs\MODELS.md')

# Licences of the Rust crates compiled into swiftvoice.exe (normal
# dependencies for this target and feature set; build tools are not shipped).
$tree = Invoke-Capture cargo (@('tree', '--locked', '-e', 'normal', '--target', 'x86_64-pc-windows-msvc', '--prefix', 'none', '--format', '{p}  [{l}]  {r}') + $features)
if ($tree.Code -ne 0) { Fail "cargo tree failed:`n$($tree.Text)" }
$crates = $tree.Text -split "`n" |
    ForEach-Object { ($_ -replace ' \(\*\)$', '').Trim() } |
    Where-Object { $_ -and $_ -notmatch '^swiftvoice v' } |
    Sort-Object -Unique
$header = @('Rust crates compiled into swiftvoice.exe: name, version, [licence], repository.', 'Full licence texts are in each crate (https://crates.io/crates/<name>) and repository.', '')
($header + $crates) | Set-Content -Encoding UTF8 (Join-Path $pkg 'licenses\RUST-CRATES.txt')

if ($Portable) {
    New-Item -ItemType File -Force -Path (Join-Path $pkg 'swiftvoice.portable') | Out-Null
    Write-Host '    portable mode: settings, logs and models stay next to swiftvoice.exe'
}

Step 'Smoke test of the packaged executable'
$ver = Invoke-Capture (Join-Path $pkg 'swiftvoice.exe') @('--version')
if ($ver.Code -ne 0) { Fail "swiftvoice.exe --version failed: $($ver.Text)" }
Write-Host "    $($ver.Text)"
$verCom = Invoke-Capture (Join-Path $pkg 'swiftvoice.com') @('--version')
if ($verCom.Code -ne 0 -or $verCom.Text -ne $ver.Text) { Fail "swiftvoice.com --version gave '$($verCom.Text)' (exit $($verCom.Code))" }
$bad = Invoke-Capture (Join-Path $pkg 'swiftvoice.com') @('--no-such-option')
if ($bad.Code -ne 2) { Fail "swiftvoice.com did not relay the exit code (got $($bad.Code), expected 2)" }
Write-Host '    swiftvoice.com relays output and exit codes'
# --doctor exits non-zero here (no models installed); only its ONNX Runtime
# lines matter for the smoke test.
$env:SWIFTVOICE_HOME = Join-Path $dist 'smoke-home'
$doctor = (Invoke-Capture (Join-Path $pkg 'swiftvoice.com') @('--doctor')).Text
Remove-Item Env:\SWIFTVOICE_HOME
if (Test-Path (Join-Path $dist 'smoke-home')) { Remove-Item -Recurse -Force (Join-Path $dist 'smoke-home') }
if ($doctor -notmatch '\[ok\]   ONNX Runtime:') { Fail "the packaged onnxruntime.dll did not load:`n$doctor" }
if ($doctor -notmatch '\[ok\]   ONNX Runtime telemetry: off') { Fail "ONNX Runtime telemetry is not off:`n$doctor" }
Write-Host '    ONNX Runtime loads from the package; telemetry off'

$zipOut = Join-Path $dist "SwiftVoice-$version-win-x64.zip"
if (Test-Path $zipOut) { Remove-Item -Force $zipOut }
Compress-Archive -Path $pkg -DestinationPath $zipOut -CompressionLevel Optimal

$installBytes = (Get-ChildItem -Recurse -File $pkg | Measure-Object -Sum Length).Sum
$installMB = [math]::Round($installBytes / 1MB, 1)
$zipMB = [math]::Round((Get-Item $zipOut).Length / 1MB, 1)
Step 'Done'
Write-Host "    folder:   $pkg  ($installMB MB installed, models not included)"
Write-Host "    archive:  $zipOut  ($zipMB MB)"
Write-Host "    SHA-256:  $(Get-Sha256 $zipOut)"
Get-ChildItem $pkg -File | Sort-Object Length -Descending | ForEach-Object {
    Write-Host ("    {0,10:N0}  {1}" -f $_.Length, $_.Name)
}
if ($installMB -gt $MaxInstallMB) {
    Write-Warning "Installed size $installMB MB exceeds the $MaxInstallMB MB target (docs/BENCHMARKS.md)."
}
Write-Host "`nFirst run: $pkg\swiftvoice.exe (tray icon), or: swiftvoice models download parakeet-tdt-0.6b-v3 silero-vad"
