<#
.SYNOPSIS
    Prove that PlayoutTranscode really works as a Windows service (T2-1, F-11).

.DESCRIPTION
    F-11 was that the documented production deployment mode did not work: the
    installer registered `PlayoutTranscode run`, a plain console program that
    never talks to the Service Control Manager, so `sc start` failed with
    error 1053 after a 30 s timeout. This cannot be unit-tested -- it needs a
    real SCM -- so it is verified by this script, run manually before a release.

    What it does, in a throwaway directory, under a throwaway service name:

      1. copies the built exe and SPA into a throwaway install directory under
         %ProgramData% -- NOT %TEMP%, which lives in the calling user's profile
         and is unreachable by the service account
      2. writes a config.toml on a free port with throwaway watch/target folders
      3. grants NT AUTHORITY\LocalService read+execute on the install directory
         and Modify on the data, watch and target directories
      4. registers the service against `service-run` as that account
      5. starts it and polls /api/health until it answers or 30 s elapse
      6. checks the service reports RUNNING
      7. stops it and checks it reaches STOPPED within 30 s, with no orphaned
         PlayoutTranscode or ffmpeg process left behind
      8. deletes the service and removes the temp tree, even on failure

    Nothing touches a real installation: the service name, port and directories
    are all distinct from the ones the installer uses.

.PARAMETER ExePath
    The PlayoutTranscode.exe to test. Defaults to target\release, falling back
    to target\debug.

.PARAMETER ServiceName
    Overrides the throwaway service name. Must not be "PlayoutTranscode".

.PARAMETER KeepOnFailure
    Leave the temp tree and the service in place when a check fails, for
    debugging. The service is still stopped.

.EXAMPLE
    # From an elevated PowerShell prompt:
    cargo build --release
    .\scripts\verify-service.ps1
#>
[CmdletBinding()]
param(
    [string]$ExePath,
    [string]$ServiceName = "PlayoutTranscodeVerify",
    [switch]$KeepOnFailure
)

$ErrorActionPreference = "Stop"
$script:Failures = @()

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Pass($msg) { Write-Host "    PASS  $msg" -ForegroundColor Green }
function Fail($msg) {
    Write-Host "    FAIL  $msg" -ForegroundColor Red
    $script:Failures += $msg
}

if ($ServiceName -eq "PlayoutTranscode") {
    throw "Refusing to run against the production service name. Pick another -ServiceName."
}

# Registering a service needs administrator rights; fail now rather than
# half-way through with a confusing access-denied.
$identity  = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw "This script must be run from an elevated PowerShell prompt."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
if (-not $ExePath) {
    foreach ($candidate in @(
        (Join-Path $repoRoot "target\release\PlayoutTranscode.exe"),
        (Join-Path $repoRoot "target\debug\PlayoutTranscode.exe")
    )) {
        if (Test-Path -LiteralPath $candidate) { $ExePath = $candidate; break }
    }
}
if (-not $ExePath -or -not (Test-Path -LiteralPath $ExePath)) {
    throw "PlayoutTranscode.exe not found. Run 'cargo build --release' first, or pass -ExePath."
}
$ExePath = (Resolve-Path -LiteralPath $ExePath).Path

# NOT %TEMP%: that is under the calling user's profile, which
# NT AUTHORITY\LocalService cannot traverse, so `sc start` fails with error 5
# (ACCESS_DENIED) before the service binary ever runs. %ProgramData% is where a
# real install puts its data and is reachable by the service account.
$root       = Join-Path $env:ProgramData ("PlayoutTranscodeVerify-" + [Guid]::NewGuid().ToString("N").Substring(0, 8))
$installDir = Join-Path $root "install"
$dataDir    = Join-Path $root "data"
$watchDir   = Join-Path $root "watch"
$targetDir  = Join-Path $root "target"
$svcAccount = "NT AUTHORITY\LocalService"

function Get-FreePort {
    $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $l.Start()
    $p = $l.LocalEndpoint.Port
    $l.Stop()
    return $p
}

function Get-ServiceState($name) {
    $out = & sc.exe query $name 2>&1 | Out-String
    if ($out -match 'STATE\s+:\s+\d+\s+(\w+)') { return $Matches[1] }
    return "ABSENT"
}

function Wait-ServiceState($name, $state, $timeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    while ((Get-Date) -lt $deadline) {
        if ((Get-ServiceState $name) -eq $state) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Remove-VerifyService {
    if ((Get-ServiceState $ServiceName) -ne "ABSENT") {
        & sc.exe stop $ServiceName | Out-Null
        [void](Wait-ServiceState $ServiceName "STOPPED" 20)
        & sc.exe delete $ServiceName | Out-Null
        for ($i = 0; $i -lt 40; $i++) {
            if ((Get-ServiceState $ServiceName) -eq "ABSENT") { break }
            Start-Sleep -Milliseconds 500
        }
    }
}

try {
    Step "Preparing $root"
    Remove-VerifyService
    foreach ($d in @($installDir, $dataDir, $watchDir, $targetDir)) {
        New-Item -ItemType Directory -Path $d -Force | Out-Null
    }
    Copy-Item -LiteralPath $ExePath -Destination $installDir -Force
    $testExe = Join-Path $installDir "PlayoutTranscode.exe"

    # The SPA is optional for this check -- /api/health does not need it -- but
    # copying it exercises the same layout the installer ships.
    $spa = Join-Path $repoRoot "web-ui\dist"
    if (Test-Path -LiteralPath $spa) {
        $dest = Join-Path $installDir "web-ui\dist"
        New-Item -ItemType Directory -Path $dest -Force | Out-Null
        Copy-Item -Path (Join-Path $spa "*") -Destination $dest -Recurse -Force
    }

    $port = Get-FreePort
    $configPath = Join-Path $dataDir "config.toml"
    $watchToml  = $watchDir  -replace '\\', '/'
    $targetToml = $targetDir -replace '\\', '/'
    @"
initialized = true

[server]
web_port = $port
bind_address = "127.0.0.1"

[paths]
watch_folder = "$watchToml"
target_folder = "$targetToml"

[logging]
level = "info"
"@ | Set-Content -LiteralPath $configPath -Encoding UTF8

    # The service account has to read and execute the binary, and write to the
    # three data/media folders. Missing the first is error 5 at start time.
    Step "Granting $svcAccount read+execute on the install folder"
    & icacls.exe $installDir /grant "${svcAccount}:(OI)(CI)RX" /T /C | Out-Null
    if ($LASTEXITCODE -ne 0) { Write-Warning "icacls on $installDir returned $LASTEXITCODE" }

    Step "Granting $svcAccount Modify on the data, watch and target folders"
    foreach ($d in @($dataDir, $watchDir, $targetDir)) {
        & icacls.exe $d /grant "${svcAccount}:(OI)(CI)M" /T /C | Out-Null
        if ($LASTEXITCODE -ne 0) { Write-Warning "icacls on $d returned $LASTEXITCODE" }
    }

    Step "Registering $ServiceName against 'service-run' as $svcAccount"
    $binPath = '"{0}" service-run --data-dir "{1}" --config "{2}"' -f $testExe, $dataDir, $configPath
    & sc.exe create $ServiceName binPath= $binPath start= demand obj= $svcAccount DisplayName= "PlayoutTranscode (verify)" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "sc.exe create failed with $LASTEXITCODE" }
    & sc.exe failure $ServiceName reset= 86400 actions= restart/5000/restart/30000/restart/60000 | Out-Null

    Step "Starting the service"
    & sc.exe start $ServiceName | Out-Null
    if ($LASTEXITCODE -ne 0) {
        $meaning = switch ($LASTEXITCODE) {
            5    { "ACCESS_DENIED - the service account cannot reach the exe or its folders. This is a harness/ACL problem, not an SCM entry point problem." }
            2    { "FILE_NOT_FOUND - the binPath is wrong." }
            1053 { "the service did not report to the SCM in time: the entry point really is missing or wedged." }
            1069 { "LOGON_FAILURE - the service account could not log on." }
            default { "see 'net helpmsg $LASTEXITCODE'." }
        }
        Fail "sc.exe start returned $LASTEXITCODE - $meaning"
        Write-Host "--- sc qc ---" -ForegroundColor DarkGray
        & sc.exe qc $ServiceName | Write-Host
        Write-Host "--- recent System event log for this service ---" -ForegroundColor DarkGray
        Get-WinEvent -FilterHashtable @{LogName='System'; StartTime=(Get-Date).AddMinutes(-5)} -ErrorAction SilentlyContinue |
            Where-Object { $_.Message -like "*$ServiceName*" } |
            Select-Object -First 5 |
            ForEach-Object { Write-Host "  [$($_.TimeCreated)] $($_.Message)" }
    }

    if (Wait-ServiceState $ServiceName "RUNNING" 30) {
        Pass "the SCM reports RUNNING"
    } else {
        Fail "the service did not reach RUNNING within 30 s (state: $(Get-ServiceState $ServiceName))"
    }

    Step "Polling http://127.0.0.1:$port/api/health"
    $healthy  = $false
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$port/api/health" -UseBasicParsing -TimeoutSec 3
            if ($r.StatusCode -eq 200) { $healthy = $true; break }
        } catch {
            Start-Sleep -Milliseconds 500
        }
    }
    if ($healthy) {
        Pass "/api/health answered 200 while running under the SCM"
    } else {
        Fail "/api/health never answered within 30 s"
    }

    Step "Checking the single-instance lock (T2-5)"
    $lockPath = Join-Path $dataDir "playout-transcode.lock"
    if (Test-Path -LiteralPath $lockPath) {
        Pass "the running service holds $lockPath"
    } else {
        Fail "no instance lock at $lockPath while the service is running"
    }

    # A second process on the same data directory would give two watchers on one
    # folder and two writers on one registry. It must refuse and say why.
    #
    # The refusal is written to stderr, and this script runs under
    # `$ErrorActionPreference = "Stop"`. Under that preference PowerShell turns
    # a native command's stderr into a *terminating* NativeCommandError, so
    # `2>&1` on a command we are deliberately making fail aborts the script --
    # at the one moment the failure is the expected result. Redirect to a file
    # instead of merging into the pipeline, which never produces an error
    # record at all.
    $secondOutFile = Join-Path $root "second-instance.txt"
    $proc = Start-Process -FilePath $testExe `
        -ArgumentList @("run", "--data-dir", $dataDir) `
        -NoNewWindow -Wait -PassThru `
        -RedirectStandardOutput "$secondOutFile.out" `
        -RedirectStandardError  "$secondOutFile.err"

    $secondOut = @(
        (Get-Content "$secondOutFile.out" -Raw -ErrorAction SilentlyContinue),
        (Get-Content "$secondOutFile.err" -Raw -ErrorAction SilentlyContinue)
    ) -join ""

    if ($proc.ExitCode -ne 0 -and $secondOut -match "already using this data directory") {
        Pass "a second instance on the same data directory was refused"
    } else {
        Fail "a second instance was not refused (exit $($proc.ExitCode)): $secondOut"
    }

    Step "Stopping the service"
    & sc.exe stop $ServiceName | Out-Null
    if (Wait-ServiceState $ServiceName "STOPPED" 30) {
        Pass "the SCM reports STOPPED"
    } else {
        Fail "the service did not reach STOPPED within 30 s (state: $(Get-ServiceState $ServiceName))"
    }

    Step "Checking for orphaned processes"
    Start-Sleep -Seconds 2
    $orphans = Get-CimInstance Win32_Process -Filter "Name='PlayoutTranscode.exe' OR Name='ffmpeg.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine -like "*$root*" }
    if ($orphans) {
        Fail "$($orphans.Count) process(es) survived the stop: $($orphans.ProcessId -join ', ')"
        $orphans | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    } else {
        Pass "no PlayoutTranscode or ffmpeg process survived the stop"
    }

    Step "Checking the instance lock was released"
    if (Test-Path -LiteralPath $lockPath) {
        Fail "the instance lock survived a clean stop; the next start will report a stale takeover"
    } else {
        Pass "the instance lock was released on stop"
    }

    Step "Checking the service wrote into its data directory"
    if (Test-Path -LiteralPath (Join-Path $dataDir "media_assets.db")) {
        Pass "media_assets.db was created under the data directory as LocalService"
    } else {
        Fail "media_assets.db is missing -- LocalService could not write to $dataDir"
    }
} finally {
    $keep = $KeepOnFailure -and $script:Failures.Count -gt 0
    if ($keep) {
        Write-Host ""
        Write-Host "Leaving $root and service $ServiceName in place (-KeepOnFailure)." -ForegroundColor Yellow
        if ((Get-ServiceState $ServiceName) -ne "STOPPED") { & sc.exe stop $ServiceName | Out-Null }
    } else {
        Step "Cleaning up"
        Remove-VerifyService
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host ""
if ($script:Failures.Count -eq 0) {
    Write-Host "verify-service: all checks passed" -ForegroundColor Green
    exit 0
} else {
    Write-Host "verify-service: $($script:Failures.Count) check(s) failed" -ForegroundColor Red
    $script:Failures | ForEach-Object { Write-Host "  - $_" -ForegroundColor Red }
    exit 1
}
