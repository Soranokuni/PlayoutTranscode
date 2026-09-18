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

# Debug symbols, kept OUT of the installer payload (T3-6).
#
# The release profile no longer strips, so a panic backtrace names functions
# instead of hex addresses -- but only if the matching .pdb is available when
# someone reads it, months later. It goes beside the installer rather than
# inside it: the shipped payload stays the same size, and the symbols are
# archived with the build so a crash report from it can still be symbolised.
$symbolDir = Join-Path (Split-Path -Parent $OutputDir) "symbols"
New-Item -ItemType Directory -Path $symbolDir -Force | Out-Null
$pdb = "target\release\PlayoutTranscode.pdb"
if (Test-Path -LiteralPath $pdb) {
    Copy-Item -LiteralPath $pdb -Destination $symbolDir -Force
    Write-Host "Symbols archived to $symbolDir (not shipped in the installer)" -ForegroundColor Green
} else {
    Write-Warning "No PDB at $pdb - a crash report from this build will not symbolise."
}

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
#
# T2-1: the service is registered against `service-run`, the Service Control
# Manager entry point, not `run`. `run` is a plain console program: the SCM
# waits for a status report that never arrives and fails the start with
# error 1053 after 30 s, which is why "Install as Windows Service" never
# produced a working service (F-11).
#
# It runs as NT AUTHORITY\LocalService, not LocalSystem: the service needs
# filesystem access to the media folders and nothing else, and LocalSystem
# hands a compromised FFmpeg invocation the whole machine.
#
# Because LocalService cannot write under Program Files, all mutable state
# lives in %ProgramData%\PlayoutTranscode (T2-2), created and ACL'd here.
$installScript = @'
$ErrorActionPreference = "Stop"
Write-Host "=== PlayoutTranscode Post-Install ===" -ForegroundColor Cyan

$exeDir  = $PSScriptRoot
$exePath = Join-Path $exeDir "PlayoutTranscode.exe"
if (-not (Test-Path -LiteralPath $exePath)) {
    Write-Error "PlayoutTranscode.exe not found at $exePath"
    exit 1
}

$svcName    = "PlayoutTranscode"
$svcAccount = "NT AUTHORITY\LocalService"
$dataDir    = Join-Path $env:ProgramData "PlayoutTranscode"
$logDir     = Join-Path $dataDir "logs"
$configPath = Join-Path $dataDir "config.toml"

New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
New-Item -ItemType Directory -Path $logDir  -Force | Out-Null

if (-not (Test-Path -LiteralPath $configPath)) {
    $example = Join-Path $exeDir "config.toml.example"
    if (Test-Path -LiteralPath $example) {
        Copy-Item -LiteralPath $example -Destination $configPath
        Write-Host "Created default config at $configPath" -ForegroundColor Green
        Write-Host "Edit it to set the watch and target folders." -ForegroundColor Yellow
    }
}

# The service account needs Modify on the data directory: config.toml, the
# asset registry, the rotated logs and any downloaded toolchain all live there.
Write-Host "Granting $svcAccount Modify on $dataDir" -ForegroundColor Yellow
& icacls.exe $dataDir /grant "${svcAccount}:(OI)(CI)M" /T /C | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Warning "icacls returned $LASTEXITCODE. The service may not be able to write to $dataDir."
}

# Re-registering is the supported upgrade path; sc.exe cannot edit obj= and
# binPath= atomically, and a stale binPath is how a half-upgraded install
# keeps launching the old entry point.
$existing = Get-Service -Name $svcName -ErrorAction SilentlyContinue
if ($existing) {
    Write-Host "Service $svcName exists; stopping and removing it..." -ForegroundColor Yellow
    Stop-Service -Name $svcName -Force -ErrorAction SilentlyContinue
    & sc.exe delete $svcName | Out-Null
    # sc.exe delete is asynchronous; wait for the name to be released.
    for ($i = 0; $i -lt 30; $i++) {
        if (-not (Get-Service -Name $svcName -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 500
    }
}

$binPath = '"{0}" service-run --data-dir "{1}" --config "{2}"' -f $exePath, $dataDir, $configPath
& sc.exe create $svcName binPath= $binPath start= auto obj= $svcAccount DisplayName= "PlayoutTranscode Media Service" | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Error "sc.exe create failed with $LASTEXITCODE"; exit 1 }

& sc.exe description $svcName "Automated broadcast media transcoding service" | Out-Null

# Restart after a crash: 5 s, then 30 s, then every 60 s; the failure count
# resets daily so a long-lived service is not permanently in "failed" state.
& sc.exe failure $svcName reset= 86400 actions= restart/5000/restart/30000/restart/60000 | Out-Null

& sc.exe start $svcName | Out-Null
if ($LASTEXITCODE -ne 0) {
    Write-Warning "sc.exe start returned $LASTEXITCODE. Check $logDir and 'sc query $svcName'."
}

# Desktop shortcut
$desktop      = [Environment]::GetFolderPath("CommonDesktopDirectory")
$shortcutPath = Join-Path $desktop "PlayoutTranscode.url"
$webUrl       = "http://127.0.0.1:4353"
"[InternetShortcut]`r`nURL=$webUrl" | Out-File -FilePath $shortcutPath -Encoding ASCII

# Start Menu folder
$startMenu = Join-Path ([Environment]::GetFolderPath("CommonPrograms")) "PlayoutTranscode"
New-Item -ItemType Directory -Path $startMenu -Force | Out-Null
Copy-Item -LiteralPath $shortcutPath -Destination (Join-Path $startMenu "PlayoutTranscode Web UI.url") -Force

Write-Host ""
Write-Host "Installation complete." -ForegroundColor Green
Write-Host "  Service account : $svcAccount"
Write-Host "  Data directory  : $dataDir"
Write-Host "  Config          : $configPath"
Write-Host "  Web UI          : $webUrl"
Write-Host ""
Write-Host "Grant $svcAccount Modify on the watch and target folders before starting ingest:" -ForegroundColor Yellow
Write-Host "  icacls ""<watch folder>"" /grant ""$svcAccount`:(OI)(CI)M"" /T" -ForegroundColor Yellow
Write-Host "  icacls ""<target folder>"" /grant ""$svcAccount`:(OI)(CI)M"" /T" -ForegroundColor Yellow
'@

Set-Content -Path "$OutputDir\install.ps1" -Value $installScript -Encoding UTF8

Write-Host "[5/5] Complete!" -ForegroundColor Green
Write-Host "Installer files at: $OutputDir"
Get-ChildItem -Path $OutputDir -Recurse | ForEach-Object {
    $size = if ($_.PSIsContainer) { "DIR" } else { "{0:N0} KB" -f ($_.Length / 1KB) }
    Write-Host "  $size  $_"
}
