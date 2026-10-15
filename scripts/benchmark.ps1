#Requires -Version 5.1
<#
.SYNOPSIS
    Run the SwiftVoice benchmark and keep the results.

.DESCRIPTION
    Wraps `swiftvoice --benchmark`. It reports audio duration, model load
    time, transcription time, real-time factor, peak RAM, word error rate
    (when a reference transcript is given), post-processing time and the
    perceived latency of push-to-talk and toggle dictation.

    With no -Audio, it uses samples\test.wav and samples\test.txt. When
    test.wav is missing, it downloads it (the public-domain JFK clip
    published by whisper.cpp) and verifies its SHA-256.

    Results are printed and saved to bench-results\<time>-<model>.txt/.json.

.PARAMETER Audio
    WAV file to transcribe (any sample rate, mono or stereo).

.PARAMETER Reference
    Reference transcript file for word error rate.

.PARAMETER Model
    Model id (default: asr.model from the config, normally parakeet-tdt-0.6b-v3).

.PARAMETER Runs
    Repetitions of each transcription (median is reported). Default 5.

.PARAMETER Download
    Download the model and the Silero VAD first if they are not installed.

.PARAMETER Exe
    swiftvoice.exe to run (default: dist\SwiftVoice, then target\release).

.PARAMETER Extra
    More arguments passed to swiftvoice --benchmark, for example
    @('--low-memory', 'off', '--commit-after-secs', '8').

.EXAMPLE
    scripts\benchmark.ps1
.EXAMPLE
    scripts\benchmark.ps1 -Model parakeet-tdt-ctc-110m-en -Download
.EXAMPLE
    scripts\benchmark.ps1 -Audio samples\me.wav -Reference samples\me.txt -Runs 10
#>
[CmdletBinding()]
param(
    [string]$Audio = '',
    [string]$Reference = '',
    [string]$Model = '',
    [int]$Runs = 5,
    [switch]$Download,
    [string]$Exe = '',
    [string[]]$Extra = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$ProgressPreference = 'SilentlyContinue'

$Root = Split-Path -Parent $PSScriptRoot
$SampleUrl = 'https://raw.githubusercontent.com/ggml-org/whisper.cpp/master/samples/jfk.wav'
$SampleSha256 = '59DFB9A4ACB36FE2A2AFFC14BACBEE2920FF435CB13CC314A08C13F66BA7860E'

function Fail($text) { Write-Host "ERROR: $text" -ForegroundColor Red; exit 1 }

# Windows PowerShell 5.1 turns native stderr into terminating errors under
# 'Stop'; SwiftVoice logs progress to stderr.
function Invoke-Native {
    param([string]$File, [string[]]$Arguments, [string]$Tee = '')
    $saved = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $lines = & $File @Arguments 2>&1 | ForEach-Object { "$_" }
        $code = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $saved
    }
    $lines | ForEach-Object { Write-Host $_ }
    if ($Tee) { $lines | Set-Content -Encoding UTF8 $Tee }
    return $code
}

if (-not $Exe) {
    foreach ($candidate in @((Join-Path $Root 'dist\SwiftVoice\swiftvoice.exe'), (Join-Path $Root 'target\release\swiftvoice.exe'))) {
        if (Test-Path $candidate) { $Exe = $candidate; break }
    }
}
if (-not $Exe -or -not (Test-Path $Exe)) {
    Fail 'swiftvoice.exe not found. Build it first: scripts\build-windows.ps1 (or pass -Exe).'
}

if (-not $Audio) {
    $Audio = Join-Path $Root 'samples\test.wav'
    if (-not $Reference) { $Reference = Join-Path $Root 'samples\test.txt' }
    if (-not (Test-Path $Audio)) {
        Write-Host "Downloading the benchmark sample from $SampleUrl"
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $SampleUrl -OutFile "$Audio.part" -UseBasicParsing
        $sha = (Get-FileHash -Algorithm SHA256 "$Audio.part").Hash
        if ($sha -ne $SampleSha256) { Remove-Item "$Audio.part"; Fail "sample SHA-256 $sha does not match $SampleSha256" }
        Move-Item -Force "$Audio.part" $Audio
    }
}
if (-not (Test-Path $Audio)) { Fail "$Audio does not exist" }

if ($Download) {
    $ids = @('silero-vad')
    if ($Model) { $ids += $Model } else { $ids += 'parakeet-tdt-0.6b-v3' }
    $code = Invoke-Native $Exe (@('models', 'download') + $ids)
    if ($code -ne 0) { Fail 'model download failed' }
}

$outDir = Join-Path $Root 'bench-results'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$label = if ($Model) { $Model } else { 'default-model' }
$base = Join-Path $outDir "$stamp-$label"

$arguments = @('--benchmark', '--audio', $Audio, '--runs', "$Runs", '--json', "$base.json")
if ($Reference) {
    if (-not (Test-Path $Reference)) { Fail "$Reference does not exist" }
    $arguments += @('--reference', $Reference)
}
if ($Model) { $arguments += @('--model', $Model) }
$arguments += $Extra

Write-Host "$Exe $($arguments -join ' ')`n"
$code = Invoke-Native $Exe $arguments "$base.txt"
if ($code -ne 0) { Fail "benchmark failed (exit code $code); see the messages above" }
Write-Host "`nSaved: $base.txt and $base.json"
Write-Host 'Targets and how to read the numbers: docs\BENCHMARKS.md'

# Explicit success (see the end of build-windows.ps1).
exit 0
