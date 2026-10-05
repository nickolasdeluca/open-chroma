# Delete Cargo's build cache in .\target to free disk space. Run it after
# build.ps1: the finished binaries live in .\dist, which is left alone. The
# next build starts from scratch and takes correspondingly longer.
# Same reasoning as build.ps1: check exit codes, not stderr.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $target = Join-Path $root 'target'
    if (-not (Test-Path $target)) {
        Write-Host 'Nothing to clean.'
        return
    }

    # A process started from target (e.g. a dev build of the service) locks
    # its exe and would leave cargo clean half done; say which one to stop.
    $running = @(Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and $_.Path.StartsWith("$target\", [StringComparison]::OrdinalIgnoreCase) })
    if ($running) {
        Write-Host "Running from target, stop these first: $(($running.Path | Sort-Object -Unique) -join ', ')"
        return
    }

    # cargo clean reports how much it removed.
    cargo clean
    if ($LASTEXITCODE) { throw 'cargo clean failed' }
} finally {
    Pop-Location
}
