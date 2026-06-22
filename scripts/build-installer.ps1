param(
    [string]$Version = "1.0.0",
    [string]$OutputDir = "$PSScriptRoot\..\dist\installer"
)

$ErrorActionPreference = "Stop"
Set-Location -LiteralPath (Join-Path $PSScriptRoot "..")

Write-Host "=== PlayoutTranscode Installer Build ===" -ForegroundColor Cyan
Write-Host "Version: $Version"
Write-Host "Output:  $OutputDir"

# Clean output
if (Test-Path -LiteralPath $OutputDir) { Remove-Item -LiteralPath $OutputDir -Recurse -Force }
New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
New-Item -ItemType Directory -Path "$OutputDir\web-ui\dist" -Force | Out-Null
New-Item -ItemType Directory -Path "$OutputDir\Requirements\ffmpeg\bin" -Force | Out-Null

# Download FFmpeg
$ffmpegUrl = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
$ffmpegZip = "$env:TEMP\ffmpeg-essentials.zip"
Write-Host "[1/5] Downloading FFmpeg essentials..." -ForegroundColor Yellow
Invoke-WebRequest -Uri $ffmpegUrl -OutFile $ffmpegZip -UseBasicParsing

Write-Host "[2/5] Extracting FFmpeg..." -ForegroundColor Yellow
$extractDir = "$env:TEMP\ffmpeg-extract"
if (Test-Path -LiteralPath $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
Expand-Archive -LiteralPath $ffmpegZip -DestinationPath $extractDir -Force
$ffmpegBin = Get-ChildItem -Path $extractDir -Recurse -Filter "ffmpeg.exe" -File | Select-Object -First 1
if (-not $ffmpegBin) { throw "FFmpeg binary not found in extracted archive" }
$ffmpegBinDir = $ffmpegBin.Directory
Copy-Item -Path "$ffmpegBinDir\ffmpeg.exe" -Destination "$OutputDir\Requirements\ffmpeg\bin\" -Force
Copy-Item -Path "$ffmpegBinDir\ffprobe.exe" -Destination "$OutputDir\Requirements\ffmpeg\bin\" -Force
Copy-Item -Path "$ffmpegBinDir\ffplay.exe" -Destination "$OutputDir\Requirements\ffmpeg\bin\" -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $ffmpegZip -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $extractDir -Recurse -Force -ErrorAction SilentlyContinue

# Build Rust
Write-Host "[3/5] Building Rust release binary..." -ForegroundColor Yellow
cargo build --release
Copy-Item -Path "target\release\PlayoutTranscode.exe" -Destination "$OutputDir\" -Force

# Build Vue SPA
Write-Host "[4/5] Building Vue SPA..." -ForegroundColor Yellow
Push-Location "web-ui"
try {
    npm install --silent
    npm run build
    Copy-Item -Path "dist\*" -Destination "$OutputDir\web-ui\dist\" -Recurse -Force
} finally {
    Pop-Location
}

# Copy example config
Copy-Item -Path "config.toml" -Destination "$OutputDir\config.toml.example" -Force -ErrorAction SilentlyContinue

# Copy post-install script
@'
@'
Write-Host "=== PlayoutTranscode Post-Install ===" -ForegroundColor Cyan

$exeDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$exePath = Join-Path $exeDir "PlayoutTranscode.exe"

if (-not (Test-Path $exePath)) {
    Write-Error "PlayoutTranscode.exe not found at $exePath"
    exit 1
}

$configPath = Join-Path $exeDir "config.toml"
if (-not (Test-Path $configPath)) {
    Copy-Item "$exeDir\config.toml.example" $configPath
    Write-Host "Created default config at $configPath" -ForegroundColor Green
    Write-Host "Edit $configPath to set watch/target folders" -ForegroundColor Yellow
}

# Register Windows Service
$svcName = "PlayoutTranscode"
$existing = Get-Service -Name $svcName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "Service $svcName already exists, stopping..." -ForegroundColor Yellow
    Stop-Service -Name $svcName -Force -ErrorAction SilentlyContinue
    sc.exe delete $svcName | Out-Null
    Start-Sleep -Seconds 2
}

$binPath = "`"$exePath`" run --config `"$configPath`""
sc.exe create $svcName binPath= $binPath start= auto DisplayName= "PlayoutTranscode Media Service" | Out-Null
sc.exe description $svcName "Automated broadcast media transcoding service" | Out-Null
sc.exe start $svcName | Out-Null

# Desktop shortcut
$desktop = [Environment]::GetFolderPath("Desktop")
$shortcutPath = Join-Path $desktop "PlayoutTranscode.url"
$webUrl = "http://127.0.0.1:4353"
@"
[InternetShortcut]
URL=$webUrl
"@ | Out-File -FilePath $shortcutPath -Encoding ASCII

# Start Menu folder
$startMenu = Join-Path ([Environment]::GetFolderPath("Programs")) "PlayoutTranscode"
New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
$smShortcut = Join-Path $startMenu "PlayoutTranscode Web UI.url"
Copy-Item $shortcutPath $smShortcut -Force

Write-Host "Installation complete. Open http://127.0.0.1:4353 in your browser." -ForegroundColor Green
'@ | Set-Content -Path "$OutputDir\install.ps1" -Encoding UTF8

Write-Host "[5/5] Complete!" -ForegroundColor Green
Write-Host "Installer files at: $OutputDir"
Get-ChildItem -Path $OutputDir -Recurse | ForEach-Object {
    $size = if ($_.PSIsContainer) { "DIR" } else { "{0:N0} KB" -f ($_.Length / 1KB) }
    Write-Host "  $size  $_"
}
