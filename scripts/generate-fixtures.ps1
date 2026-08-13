# scripts/generate-fixtures.ps1
# Generates synthetic media fixtures for PlayoutTranscode V2-0 baseline verification.

$ErrorActionPreference = "Stop"

$ProjectRoot = Resolve-Path "$PSScriptRoot\.."
$FixturesDir = Join-Path $ProjectRoot "fixtures"

Write-Host "==> Resolving FFmpeg and FFprobe toolchain..." -ForegroundColor Cyan

function Find-Tool {
    param([string]$toolName)
    
    # 1. Check PATH
    $cmd = Get-Command $toolName -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }
    
    # 2. Check project bin/
    $binPath = Join-Path $ProjectRoot "bin\$toolName.exe"
    if (Test-Path $binPath) {
        return $binPath
    }
    
    # 3. Check target/debug/bin/
    $targetBinPath = Join-Path $ProjectRoot "target\debug\bin\$toolName.exe"
    if (Test-Path $targetBinPath) {
        return $targetBinPath
    }
    
    return $null
}

$FfmpegExe = Find-Tool "ffmpeg"
$FfprobeExe = Find-Tool "ffprobe"

if (-not $FfmpegExe -or -not $FfprobeExe) {
    Write-Host "ERROR: FFmpeg or FFprobe toolchain is not available." -ForegroundColor Red
    Write-Host "  FFmpeg: $(if ($FfmpegExe) { $FfmpegExe } else { 'NOT FOUND' })" -ForegroundColor Yellow
    Write-Host "  FFprobe: $(if ($FfprobeExe) { $FfprobeExe } else { 'NOT FOUND' })" -ForegroundColor Yellow
    Write-Host "Please install FFmpeg/FFprobe into PATH or download them via the PlayoutTranscode control panel / 'PlayoutTranscode setup'." -ForegroundColor Yellow
    exit 1
}

Write-Host "Found FFmpeg: $FfmpegExe" -ForegroundColor Green
Write-Host "Found FFprobe: $FfprobeExe" -ForegroundColor Green

if (-not (Test-Path $FixturesDir)) {
    New-Item -ItemType Directory -Path $FixturesDir | Out-Null
}

Write-Host "`n==> Generating V2-0 Synthetic Media Fixtures in '$FixturesDir'..." -ForegroundColor Cyan

# Helper to run FFmpeg command safely
function Invoke-FFmpeg {
    param([string[]]$Arguments, [string]$OutputFile)
    
    Write-Host "  [+] Generating $(Split-Path $OutputFile -Leaf)..." -NoNewline
    $process = Start-Process -FilePath $FfmpegExe -ArgumentList $Arguments -NoNewWindow -Wait -PassThru -RedirectStandardError "$FixturesDir\ffmpeg_last.log"
    if ($process.ExitCode -ne 0) {
        Write-Host " FAILED!" -ForegroundColor Red
        Get-Content "$FixturesDir\ffmpeg_last.log" | Select-Object -Last 15
        Write-Error "FFmpeg failed while generating $OutputFile"
        exit 1
    }
    Write-Host " OK ($((Get-Item $OutputFile).Length) bytes)" -ForegroundColor Green
}

# 1. Video Only (5s, 1080p25, no audio)
$vOnly = Join-Path $FixturesDir "video_only.mp4"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25",
    "-t", "5.0",
    "-c:v", "libx264", "-pix_fmt", "yuv420p",
    $vOnly
) -OutputFile $vOnly

# 2. Audio Only (5s, 44.1kHz MP3)
$aOnly = Join-Path $FixturesDir "audio_only.mp3"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100",
    "-t", "5.0",
    "-c:a", "libmp3lame", "-b:a", "192k",
    $aOnly
) -OutputFile $aOnly

# 3. Video + Stereo Audio (5s, 1080p25, 48kHz AAC)
$vStereo = Join-Path $FixturesDir "video_stereo.mp4"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25",
    "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000",
    "-t", "5.0",
    "-c:v", "libx264", "-pix_fmt", "yuv420p",
    "-c:a", "aac", "-b:a", "320k",
    $vStereo
) -OutputFile $vStereo

# 4. Multichannel Audio (5s, 1080p25, 6-ch PCM)
$mChan = Join-Path $FixturesDir "multichannel.mkv"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
    "-t", "5.0",
    "-filter_complex", "[1:a]pan=5.1|c0=c0|c1=c0|c2=c0|c3=c0|c4=c0|c5=c0[aout]",
    "-map", "0:v", "-map", "[aout]",
    "-c:v", "libx264", "-pix_fmt", "yuv420p",
    "-c:a", "pcm_s16le",
    $mChan
) -OutputFile $mChan

# 5. VFR Source (5s variable timestamp stream)
$vfrSrc = Join-Path $FixturesDir "vfr_source.mp4"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
    "-t", "5.0",
    "-vf", "select='not(mod(n\,3))',setpts=N/(24*TB)",
    "-c:v", "libx264", "-pix_fmt", "yuv420p",
    "-c:a", "aac",
    $vfrSrc
) -OutputFile $vfrSrc

# 6. Interlaced TFF (5s 1080i50)
$intlTff = Join-Path $FixturesDir "interlaced_tff.mp4"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
    "-t", "5.0",
    "-c:v", "libx264", "-flags", "+ilme+ildct", "-top", "1", "-pix_fmt", "yuv420p",
    "-c:a", "aac",
    $intlTff
) -OutputFile $intlTff

# 7. Corrupt / Truncated Source
$corruptFile = Join-Path $FixturesDir "corrupt_truncated.mp4"
Write-Host "  [+] Generating corrupt_truncated.mp4..." -NoNewline
$tempMp4 = Join-Path $FixturesDir "temp_for_corrupt.mp4"
Start-Process -FilePath $FfmpegExe -ArgumentList @("-y", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25", "-t", "1.0", "-c:v", "libx264", $tempMp4) -NoNewWindow -Wait
if (Test-Path $tempMp4) {
    $bytes = [System.IO.File]::ReadAllBytes($tempMp4)
    $truncatedLength = [Math]::Min(32768, $bytes.Length)
    $truncated = New-Object byte[] $truncatedLength
    [Array]::Copy($bytes, $truncated, $truncatedLength)
    [System.IO.File]::WriteAllBytes($corruptFile, $truncated)
    Remove-Item $tempMp4 -Force -ErrorAction SilentlyContinue
    Write-Host " OK ($truncatedLength bytes truncated)" -ForegroundColor Green
} else {
    Write-Host " FAILED!" -ForegroundColor Red
    exit 1
}

# 8. Compliant Mezzanine (5s 1080p25, closed GOP 50, faststart)
$mezzFile = Join-Path $FixturesDir "mezzanine_compliant.mp4"
Invoke-FFmpeg -Arguments @(
    "-y", "-hide_banner", "-loglevel", "error",
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25",
    "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000",
    "-t", "5.0",
    "-c:v", "libx264", "-preset", "medium", "-crf", "24", "-profile:v", "high", "-level", "4.2", "-pix_fmt", "yuv420p",
    "-g", "50", "-keyint_min", "50", "-sc_threshold", "0",
    "-x264-params", "open-gop=0:keyint=50:min-keyint=50:scenecut=0",
    "-movflags", "+faststart",
    "-c:a", "aac", "-b:a", "320k", "-ar", "48000", "-ac", "2",
    $mezzFile
) -OutputFile $mezzFile

# Remove temporary log file
Remove-Item "$FixturesDir\ffmpeg_last.log" -Force -ErrorAction SilentlyContinue

Write-Host "`n==> All 8 fixture classes generated successfully in '$FixturesDir'." -ForegroundColor Green
Write-Host "To verify baseline probe properties, run: powershell -ExecutionPolicy Bypass -File scripts/verify-baseline.ps1" -ForegroundColor Gray
