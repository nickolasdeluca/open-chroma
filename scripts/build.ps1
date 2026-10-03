# Build release binaries and assemble them in .\dist
#   openchroma.exe       CLI (and `openchroma run` for a console service)
#   openchromad.exe      windowless service, used for autostart
#   openchroma-app.exe   desktop app
#   RzChromaSDK64.dll    Chroma SDK replacement for 64-bit games
#   RzChromaSDK.dll      Chroma SDK replacement for 32-bit games
# Native tools write progress to stderr, which Windows PowerShell 5.1 turns
# into errors under 'Stop'; check exit codes instead.
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    rustup target add i686-pc-windows-msvc 2>&1 | Out-Null
    if ($LASTEXITCODE) { throw 'rustup failed' }
    cargo build --release -p openchroma -p rzchromasdk -p openchroma-app
    if ($LASTEXITCODE) { throw 'x64 build failed' }
    cargo build --release -p rzchromasdk --target i686-pc-windows-msvc
    if ($LASTEXITCODE) { throw 'x86 build failed' }

    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force $dist -ErrorAction Stop | Out-Null

    # A running service locks its exe. Stop only instances started from dist
    # (not dev builds elsewhere), and only now that the build succeeded, so
    # the lights are out for as short a time as possible.
    $running = @(Get-Process -Name openchroma, openchromad, openchroma-app -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith($dist, [StringComparison]::OrdinalIgnoreCase) })
    if ($running) {
        Write-Host "Stopping running OpenChroma ($($running.Name -join ', '))"
        $running | Stop-Process -Force
        $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
    }

    Copy-Item target\release\openchroma.exe, target\release\openchromad.exe, target\release\openchroma-app.exe $dist -ErrorAction Stop
    Copy-Item target\release\rzchromasdk.dll (Join-Path $dist 'RzChromaSDK64.dll') -ErrorAction Stop
    Copy-Item target\i686-pc-windows-msvc\release\rzchromasdk.dll (Join-Path $dist 'RzChromaSDK.dll') -ErrorAction Stop
    Get-ChildItem $dist | Format-Table Name, Length

    if ($running | Where-Object Name -ne 'openchroma-app') {
        # Bring the service back windowless, whichever way it was running.
        Start-Process (Join-Path $dist 'openchromad.exe')
        Write-Host 'Restarted openchromad'
    }
    if (Get-Service OpenChroma -ErrorAction SilentlyContinue) {
        # The service runs its own copy in Program Files; updating it needs admin.
        Write-Host 'The OpenChroma service still runs the previous build. To update it, run as administrator:'
        Write-Host "  $dist\openchroma.exe service install"
    }
} finally {
    Pop-Location
}
