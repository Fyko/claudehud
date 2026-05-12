# claudehud installer (Windows)
# usage: irm https://raw.githubusercontent.com/fyko/claudehud/main/install.ps1 | iex
#
# Env opt-outs (all optional):
#   $env:CLAUDEHUD_VERSION         Pin a specific release tag (default: latest)
#   $env:CLAUDEHUD_INSTALL_DIR     Override install directory (default: %LOCALAPPDATA%\Programs\claudehud)
#   $env:CLAUDEHUD_FORCE_INSTALL=1 Reinstall even if the target version is already present
#   $env:CLAUDEHUD_SKIP_CONFIG=1   Skip configuration of Claude Code statusLine
#   $env:CLAUDEHUD_FORCE_CONFIG=1  Override existing statusLine configuration
#   $env:CLAUDEHUD_SKIP_PATH=1     Don't modify user PATH
#   $env:CLAUDEHUD_SKIP_DAEMON=1   Don't register the Task Scheduler entry
#   $env:CLAUDEHUD_SKIP_CHECKSUM=1 Skip .sha256 sidecar verification (debug only)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'fyko/claudehud'
$DefaultInstallDir = Join-Path $env:LOCALAPPDATA 'Programs\claudehud'
$InstallDir = if ($env:CLAUDEHUD_INSTALL_DIR) { $env:CLAUDEHUD_INSTALL_DIR } else { $DefaultInstallDir }

function Say($msg) { Write-Host "==> $msg" -ForegroundColor White }
function Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }
function Die($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

# ---------------------------------------------------------------------------
# preflight
# ---------------------------------------------------------------------------

if (-not [Environment]::Is64BitOperatingSystem) {
    Die 'claudehud requires 64-bit Windows.'
}

# ---------------------------------------------------------------------------
# detect arch
# ---------------------------------------------------------------------------

$Target = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    'aarch64-pc-windows-msvc'
} else {
    'x86_64-pc-windows-msvc'
}
Say "detected target: $Target"

# ---------------------------------------------------------------------------
# resolve version
# ---------------------------------------------------------------------------

function Get-LatestTag {
    $url = "https://api.github.com/repos/$Repo/releases/latest"
    $headers = @{ 'User-Agent' = 'claudehud-installer' }
    try {
        $rel = Invoke-RestMethod -Uri $url -Headers $headers -ErrorAction Stop
        return $rel.tag_name
    } catch {
        Die "failed to fetch latest release tag: $_"
    }
}

$Tag = if ($env:CLAUDEHUD_VERSION) {
    Say "using pinned version $env:CLAUDEHUD_VERSION"
    $env:CLAUDEHUD_VERSION
} else {
    Say 'fetching latest release tag...'
    Get-LatestTag
}
$TagVer = $Tag -replace '^v', ''

# ---------------------------------------------------------------------------
# up-to-date short-circuit
# ---------------------------------------------------------------------------

$ClientExe = Join-Path $InstallDir 'claudehud.exe'
$DaemonExe = Join-Path $InstallDir 'claudehud-daemon.exe'
$SkipDownload = $false

if (-not $env:CLAUDEHUD_FORCE_INSTALL -and (Test-Path $ClientExe)) {
    try {
        $verLine = & $ClientExe --version 2>$null
        $installedVer = ($verLine -split '\s+')[1]
        if ($installedVer -eq $TagVer) {
            Say "claudehud $installedVer is already up to date"
            Say '(set $env:CLAUDEHUD_FORCE_INSTALL=1 to reinstall)'
            $SkipDownload = $true
        } else {
            Say "upgrading claudehud $installedVer -> $TagVer"
        }
    } catch {
        Say "installing claudehud $Tag"
    }
} else {
    Say "installing claudehud $Tag"
}

# ---------------------------------------------------------------------------
# download + verify
# ---------------------------------------------------------------------------

function Verify-Sha256 {
    param([string]$File, [string]$Url)
    if ($env:CLAUDEHUD_SKIP_CHECKSUM) {
        Warn "CLAUDEHUD_SKIP_CHECKSUM set, skipping verification for $(Split-Path -Leaf $File)"
        return
    }
    $sidecar = "$File.sha256"
    try {
        Invoke-WebRequest -Uri "$Url.sha256" -OutFile $sidecar -UseBasicParsing -ErrorAction Stop
    } catch {
        Remove-Item -Force -ErrorAction SilentlyContinue $File
        Die "failed to download checksum sidecar for $(Split-Path -Leaf $File): $_"
    }
    $expected = ((Get-Content $sidecar -Raw) -split '\s+')[0].ToLower()
    Remove-Item -Force $sidecar
    $actual = (Get-FileHash $File -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        Remove-Item -Force $File
        Die "checksum mismatch for $(Split-Path -Leaf $File) -- expected $expected, got $actual"
    }
}

if (-not $SkipDownload) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $tmpDir = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "claudehud-install-$([guid]::NewGuid())")
    try {
        $baseUrl = "https://github.com/$Repo/releases/download/$Tag"
        foreach ($name in 'claudehud', 'claudehud-daemon') {
            $artifact = "$name-$Target.exe"
            $url = "$baseUrl/$artifact"
            $dest = Join-Path $tmpDir.FullName "$name.exe"
            Say "downloading $name..."
            try {
                Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing -ErrorAction Stop
            } catch {
                Die "failed to download $name : $_"
            }
            Verify-Sha256 -File $dest -Url $url
            Move-Item -Force $dest (Join-Path $InstallDir "$name.exe")
        }
        Say "installed to $InstallDir"
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmpDir
    }
}

# ---------------------------------------------------------------------------
# PATH
# ---------------------------------------------------------------------------

$pathWasUpdated = $false
if (-not $env:CLAUDEHUD_SKIP_PATH) {
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $segments = if ($userPath) { $userPath -split ';' } else { @() }
    if ($segments -notcontains $InstallDir) {
        $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
        $env:PATH = "$env:PATH;$InstallDir"
        Say "added $InstallDir to user PATH"
        $pathWasUpdated = $true
    }
}

# ---------------------------------------------------------------------------
# statusLine config
# ---------------------------------------------------------------------------

if (-not $env:CLAUDEHUD_SKIP_CONFIG) {
    $forceArg = if ($env:CLAUDEHUD_FORCE_CONFIG) { '--force' } else { $null }
    Say 'configuring Claude Code statusLine...'
    if ($forceArg) {
        & $ClientExe install $forceArg
    } else {
        & $ClientExe install
    }
    if ($LASTEXITCODE -ne 0) {
        Warn 'claudehud install returned non-zero; statusLine may not be configured'
    }
} else {
    Say 'skipping Claude Code configuration (CLAUDEHUD_SKIP_CONFIG is set)'
}

# ---------------------------------------------------------------------------
# Task Scheduler registration
# ---------------------------------------------------------------------------

function Register-ClaudehudDaemon {
    # Use the user's SID throughout. $env:USERNAME is only the short name on
    # domain-joined machines (e.g. "carter" not "CORP\carter"), which can fail
    # or match the wrong principal. SIDs are unambiguous in both environments.
    $sid = ([System.Security.Principal.WindowsIdentity]::GetCurrent()).User.Value
    $xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$sid</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$sid</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Hidden>true</Hidden>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>$DaemonExe</Command>
    </Exec>
  </Actions>
</Task>
"@
    try {
        Register-ScheduledTask -Xml $xml -TaskName 'claudehud-daemon' -User $sid -Force | Out-Null
        Start-ScheduledTask -TaskName 'claudehud-daemon' -ErrorAction SilentlyContinue
        Say 'daemon registered + started via Task Scheduler (claudehud-daemon)'
    } catch {
        Warn "failed to register Task Scheduler entry: $_"
        Warn 'you can start the daemon manually with: & "$DaemonExe"'
    }
}

if (-not $env:CLAUDEHUD_SKIP_DAEMON) {
    Register-ClaudehudDaemon
}

# ---------------------------------------------------------------------------
# done
# ---------------------------------------------------------------------------

if ($pathWasUpdated) {
    Write-Host ''
    Write-Host 'hint: ' -ForegroundColor Yellow -NoNewline
    Write-Host 'restart your terminal so the updated PATH takes effect.'
}

Write-Host ''
Say 'done. claudehud is ready.'
