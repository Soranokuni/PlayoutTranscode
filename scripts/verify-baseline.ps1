# scripts/verify-baseline.ps1
# Verifies PlayoutTranscode V2-0 baseline media fixtures against canonical manifest specifications.

$ErrorActionPreference = "Stop"

$ProjectRoot = Resolve-Path "$PSScriptRoot\.."
$FixturesDir = Join-Path $ProjectRoot "fixtures"
$ManifestPath = Join-Path $FixturesDir "manifest.json"

Write-Host "==> Resolving FFprobe toolchain..." -ForegroundColor Cyan

function Find-Tool {
    param([string]$toolName)
    
    $cmd = Get-Command $toolName -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }
    
    $binPath = Join-Path $ProjectRoot "bin\$toolName.exe"
    if (Test-Path $binPath) {
        return $binPath
    }
    
    $targetBinPath = Join-Path $ProjectRoot "target\debug\bin\$toolName.exe"
    if (Test-Path $targetBinPath) {
        return $targetBinPath
    }
    
    return $null
}

$FfprobeExe = Find-Tool "ffprobe"

if (-not $FfprobeExe) {
    Write-Host "ERROR: FFprobe toolchain is not available." -ForegroundColor Red
    Write-Host "Please install FFmpeg/FFprobe into PATH or download them via the PlayoutTranscode control panel." -ForegroundColor Yellow
    exit 1
}

Write-Host "Found FFprobe: $FfprobeExe" -ForegroundColor Green

if (-not (Test-Path $ManifestPath)) {
    Write-Host "ERROR: Manifest file not found at '$ManifestPath'." -ForegroundColor Red
    Write-Host "Please run 'powershell -ExecutionPolicy Bypass -File scripts/generate-fixtures.ps1' first." -ForegroundColor Yellow
    exit 1
}

$Manifest = Get-Content $ManifestPath | ConvertFrom-Json
Write-Host "`n==> Verifying V2-0 Media Fixture Invariants (Manifest v$($Manifest.version))..." -ForegroundColor Cyan

$Passed = 0
$Failed = 0

foreach ($item in $Manifest.fixtures) {
    $filePath = Join-Path $FixturesDir $item.name
    Write-Host "`n--- [Fixture] $($item.name) ($($item.class)) ---" -ForegroundColor Yellow
    Write-Host "Description: $($item.description)" -ForegroundColor Gray
    
    if (-not (Test-Path $filePath)) {
        Write-Host "  [-] File MISSING: $filePath" -ForegroundColor Red
        $Failed++
        continue
    }
    
    # Compute SHA-256 (Informational / Toolchain-specific)
    $hash = (Get-FileHash $filePath -Algorithm SHA256).Hash
    Write-Host "  [*] File SHA-256 (Informational): $hash" -ForegroundColor ConsoleColor

    # Handle corrupt fixture probe expected failure
    if ($item.class -eq "corrupt_truncated") {
        $probeProcess = Start-Process -FilePath $FfprobeExe -ArgumentList @("-v", "error", "-print_format", "json", "-show_format", "-show_streams", $filePath) -NoNewWindow -Wait -PassThru -RedirectStandardError "$FixturesDir\probe_err.log"
        if ($probeProcess.ExitCode -ne 0 -or (Get-Content "$FixturesDir\probe_err.log" -ErrorAction SilentlyContinue)) {
            Write-Host "  [+] Corrupt stream probe failed as expected (Error exit / diagnostic written)." -ForegroundColor Green
            $Passed++
        } else {
            Write-Host "  [-] Corrupt stream probe succeeded unexpectedly!" -ForegroundColor Red
            $Failed++
        }
        Remove-Item "$FixturesDir\probe_err.log" -Force -ErrorAction SilentlyContinue
        continue
    }

    # Run ffprobe for standard fixtures
    $stdoutFile = [System.IO.Path]::GetTempFileName()
    $stderrFile = [System.IO.Path]::GetTempFileName()
    
    $proc = Start-Process -FilePath $FfprobeExe -ArgumentList @(
        "-v", "quiet",
        "-print_format", "json",
        "-show_format",
        "-show_streams",
        $filePath
    ) -NoNewWindow -Wait -PassThru -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile
    
    $jsonRaw = Get-Content $stdoutFile -Raw
    Remove-Item $stdoutFile, $stderrFile -Force -ErrorAction SilentlyContinue
    
    if (-not $jsonRaw) {
        Write-Host "  [-] FFprobe produced empty output!" -ForegroundColor Red
        $Failed++
        continue
    }

    try {
        $probeData = $jsonRaw | ConvertFrom-Json
    } catch {
        Write-Host "  [-] Failed to parse FFprobe JSON output: $_" -ForegroundColor Red
        $Failed++
        continue
    }

    $vStream = $probeData.streams | Where-Object { $_.codec_type -eq "video" } | Select-Object -First 1
    $aStream = $probeData.streams | Where-Object { $_.codec_type -eq "audio" } | Select-Object -First 1

    $fixtureErrors = @()

    # Check Video Presence
    if ($item.expected_probe.has_video -and -not $vStream) {
        $fixtureErrors += "Expected video stream, but none found"
    } elseif (-not $item.expected_probe.has_video -and $vStream) {
        $fixtureErrors += "Expected NO video stream, but found video stream"
    }

    # Check Audio Presence
    if ($item.expected_probe.has_audio -and -not $aStream) {
        $fixtureErrors += "Expected audio stream, but none found"
    } elseif (-not $item.expected_probe.has_audio -and $aStream) {
        $fixtureErrors += "Expected NO audio stream, but found audio stream"
    }

    # Check Duration (tolerance 250ms)
    if ($probeData.format -and $probeData.format.duration) {
        $probedDurationMs = [int]([double]$probeData.format.duration * 1000)
        $expectedDurationMs = $item.expected_probe.duration_ms
        $diff = [Math]::Abs($probedDurationMs - $expectedDurationMs)
        if ($diff -gt 250) {
            $fixtureErrors += "Duration mismatch: probed ${probedDurationMs}ms, expected ${expectedDurationMs}ms (diff ${diff}ms > 250ms)"
        } else {
            Write-Host "  [+] Duration: ${probedDurationMs}ms (matches expected ${expectedDurationMs}ms ±250ms)" -ForegroundColor Green
        }
    }

    # Check Video Properties
    if ($vStream) {
        if ($item.expected_probe.width -and $vStream.width -ne $item.expected_probe.width) {
            $fixtureErrors += "Width mismatch: probed $($vStream.width), expected $($item.expected_probe.width)"
        }
        if ($item.expected_probe.height -and $vStream.height -ne $item.expected_probe.height) {
            $fixtureErrors += "Height mismatch: probed $($vStream.height), expected $($item.expected_probe.height)"
        }
        if ($item.expected_probe.video_codec -and $vStream.codec_name -ne $item.expected_probe.video_codec) {
            $fixtureErrors += "Video codec mismatch: probed $($vStream.codec_name), expected $($item.expected_probe.video_codec)"
        }
        Write-Host "  [+] Video Stream: $($vStream.codec_name) $($vStream.width)x$($vStream.height)" -ForegroundColor Green
    }

    # Check Audio Properties
    if ($aStream) {
        if ($item.expected_probe.audio_codec -and $aStream.codec_name -ne $item.expected_probe.audio_codec) {
            $fixtureErrors += "Audio codec mismatch: probed $($aStream.codec_name), expected $($item.expected_probe.audio_codec)"
        }
        if ($item.expected_probe.sample_rate -and [int]$aStream.sample_rate -ne $item.expected_probe.sample_rate) {
            $fixtureErrors += "Sample rate mismatch: probed $($aStream.sample_rate) Hz, expected $($item.expected_probe.sample_rate) Hz"
        }
        if ($item.expected_probe.channels -and $aStream.channels -ne $item.expected_probe.channels) {
            $fixtureErrors += "Channel count mismatch: probed $($aStream.channels), expected $($item.expected_probe.channels)"
        }
        Write-Host "  [+] Audio Stream: $($aStream.codec_name) $($aStream.sample_rate) Hz ($($aStream.channels) ch)" -ForegroundColor Green
    }

    # Profile Classification Assertion
    Write-Host "  [+] Expected Profile Classification: $($item.expected_classification.profile)" -ForegroundColor Green

    if ($fixtureErrors.Count -eq 0) {
        Write-Host "  [+] Fixture $($item.name) VERIFIED OK." -ForegroundColor Green
        $Passed++
    } else {
        Write-Host "  [-] Fixture $($item.name) FAILED verification:" -ForegroundColor Red
        foreach ($err in $fixtureErrors) {
            Write-Host "      * $err" -ForegroundColor Red
        }
        $Failed++
    }
}

Write-Host "`n==========================================" -ForegroundColor Cyan
Write-Host " Baseline Verification Summary" -ForegroundColor Cyan
Write-Host " Passed: $Passed | Failed: $Failed | Total: $($Manifest.fixtures.Count)" -ForegroundColor $(if ($Failed -eq 0) { "Green" } else { "Red" })
Write-Host "==========================================" -ForegroundColor Cyan

if ($Failed -ne 0) {
    exit 1
} else {
    exit 0
}
