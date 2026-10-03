<#
.SYNOPSIS
Install, update or remove OpenChroma from its GitHub releases.

.DESCRIPTION
Install or update to the latest release:

    irm https://github.com/__REPO__/releases/latest/download/install.ps1 | iex

A specific version, or uninstall:

    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -Version 0.2.0
    & ([scriptblock]::Create((irm https://github.com/__REPO__/releases/latest/download/install.ps1))) -Uninstall

Installing needs administrator rights; the script asks for them (UAC) when
started from a normal shell. Your profiles and settings in
%ProgramData%\OpenChroma are kept on update and uninstall.
#>
param(
    # Release to install, e.g. 0.2.0. Defaults to the latest.
    [string]$Version = 'latest',
    # Remove the service, the app and the SDK DLLs instead of installing.
    [switch]$Uninstall,
    # GitHub repository (owner/name) to install from. Filled in when the
    # script is published with a release.
    [string]$Repo = '__REPO__',
    # Internal: set when the script relaunched itself elevated, so its window
    # stays open long enough to read.
    [switch]$Elevated
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue' # Invoke-WebRequest is much faster without the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }

if ($Repo -like '*__REPO*') {
    throw 'This copy of the installer has no repository set. Download it from a release, or pass -Repo owner/name.'
}

$principal = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    # `irm | iex` leaves no script file to re-run, so fetch the installer again
    # and start it elevated with the same options.
    Write-Step 'Administrator rights are needed; asking Windows for them'
    $self = Join-Path $env:TEMP 'openchroma-install.ps1'
    Invoke-WebRequest "https://github.com/$Repo/releases/latest/download/install.ps1" -OutFile $self -UseBasicParsing
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$self`"", '-Repo', $Repo, '-Version', $Version, '-Elevated')
    if ($Uninstall) { $argList += '-Uninstall' }
    $proc = Start-Process powershell -Verb RunAs -ArgumentList $argList -Wait -PassThru
    Remove-Item $self -ErrorAction SilentlyContinue
    if ($proc.ExitCode -ne 0) { throw "The installer failed (exit code $($proc.ExitCode))." }
    return
}

$installDir = Join-Path $env:ProgramFiles 'OpenChroma'
$work = Join-Path $env:TEMP ("openchroma-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $work | Out-Null

try {
    # The app must be closed before its files can be replaced or removed.
    Get-Process openchroma-app -ErrorAction SilentlyContinue | Stop-Process -Force

    if ($Uninstall) {
        $cli = Join-Path $installDir 'openchroma.exe'
        if (-not (Test-Path $cli)) { throw "OpenChroma is not installed in $installDir." }
        # Run a copy so the uninstaller can delete the installed files.
        Copy-Item $cli (Join-Path $work 'openchroma.exe')
        $cli = Join-Path $work 'openchroma.exe'
        Write-Step 'Removing the SDK DLLs (Razer''s originals are restored if they were backed up)'
        & $cli sdk uninstall
        Write-Step 'Removing the service and the app'
        & $cli service uninstall
        if ($LASTEXITCODE) { throw 'Uninstalling the service failed.' }
        Remove-Item $installDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Step 'OpenChroma was removed. Your settings remain in %ProgramData%\OpenChroma.'
        return
    }

    $api = "https://api.github.com/repos/$Repo/releases/" + $(if ($Version -eq 'latest') { 'latest' } else { "tags/v$($Version.TrimStart('v'))" })
    Write-Step "Looking up the $Version release of $Repo"
    $release = Invoke-RestMethod $api -Headers @{ 'User-Agent' = 'openchroma-installer' }
    $zip = $release.assets | Where-Object name -like 'OpenChroma-*-windows-x64.zip' | Select-Object -First 1
    $sums = $release.assets | Where-Object name -eq 'SHA256SUMS.txt' | Select-Object -First 1
    if (-not $zip -or -not $sums) { throw "Release $($release.tag_name) has no Windows package." }

    Write-Step "Downloading $($zip.name)"
    $zipPath = Join-Path $work $zip.name
    Invoke-WebRequest $zip.browser_download_url -OutFile $zipPath -UseBasicParsing
    $sumsText = (Invoke-WebRequest $sums.browser_download_url -UseBasicParsing).Content
    if ($sumsText -is [byte[]]) { $sumsText = [Text.Encoding]::ASCII.GetString($sumsText) }
    $expected = ($sumsText -split "`n" | Where-Object { $_ -match [regex]::Escape($zip.name) } | ForEach-Object { ($_ -split '\s+')[0] }) | Select-Object -First 1
    $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash
    if (-not $expected -or $actual -ne $expected.Trim().ToUpper()) { throw "Checksum mismatch for $($zip.name); not installing." }

    $files = Join-Path $work 'files'
    Expand-Archive $zipPath $files

    # Razer's SDK service holds the Chroma SDK port and its stack writes to the
    # same devices, so the two can't run together.
    $razer = Get-Service 'Razer Chroma SDK Server', 'Razer Chroma SDK Service' -ErrorAction SilentlyContinue | Where-Object Status -eq 'Running'
    if ($razer) {
        Write-Warning "Razer's Chroma SDK services are running. They compete with OpenChroma for your devices and for games."
        $answer = Read-Host 'Stop and disable them now? You can undo this with razer-services.ps1 restore [y/N]'
        if ($answer -match '^(y|yes)$') {
            & (Join-Path $files 'razer-services.ps1') disable
        }
    }

    Write-Step "Installing OpenChroma $($release.tag_name)"
    & (Join-Path $files 'openchroma.exe') service install
    if ($LASTEXITCODE) { throw 'The service install failed; see the messages above.' }
    Copy-Item (Join-Path $files 'razer-services.ps1') $installDir -Force

    Write-Step "OpenChroma $($release.tag_name) is installed. Open it from the Start menu: OpenChroma."
}
catch {
    Write-Host "Error: $($_.Exception.Message)" -ForegroundColor Red
    $failed = $true
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    if ($Elevated) { Read-Host 'Press Enter to close this window' | Out-Null }
}
if ($failed) {
    # `exit` would close the user's shell under `irm | iex`; only the
    # elevated relaunch runs as its own process.
    if ($Elevated) { exit 1 } else { throw 'OpenChroma was not installed.' }
}
