# Stop Razer's lighting stack so OpenChroma has the devices and the Chroma SDK
# port (54235) to itself, or put it back.
#
#   .\razer-services.ps1 status
#   .\razer-services.ps1 stop      # stop now; they come back on reboot
#   .\razer-services.ps1 disable   # stop and keep them from starting
#   .\razer-services.ps1 restore   # restore the original startup types and start them
#
# stop/disable/restore need an elevated PowerShell. Original startup types are
# saved to %ProgramData%\OpenChroma\razer-services.json before the first change.
param([Parameter(Mandatory)][ValidateSet('status', 'stop', 'disable', 'restore')][string]$Action)
$ErrorActionPreference = 'Stop'

$services = @(
    'Razer Chroma SDK Server',
    'Razer Chroma SDK Service',
    'Razer Chroma SDK Diagnostic Service',
    'Razer Chroma Stream Server'
)
# Synapse 4's app and its device helpers are user processes, not services.
$processes = @('RazerAppEngine', 'RzDeviceManager', 'RzDeviceManagerEx', 'RzChromaConnectManager', 'RzChromaConnectServer',
    'RzSmartlightingDeviceManager', 'RzWDLDeviceManager', 'RzIoTDeviceManager', 'RzBTLEManager', 'RzAppManager', 'RzEngineMon')
$state = Join-Path $env:ProgramData 'OpenChroma\razer-services.json'

$present = Get-Service -Name $services -ErrorAction SilentlyContinue

switch ($Action) {
    'status' {
        $present | Format-Table Name, Status, StartType -AutoSize
        Get-Process -Name $processes -ErrorAction SilentlyContinue | Group-Object ProcessName | Format-Table Name, Count -AutoSize
    }
    { $_ -in 'stop', 'disable' } {
        if (-not (Test-Path $state)) {
            New-Item -ItemType Directory -Force (Split-Path $state) | Out-Null
            $present | ForEach-Object { @{ Name = $_.Name; StartType = "$($_.StartType)" } } | ConvertTo-Json | Set-Content $state
            Write-Host "Saved original startup types to $state"
        }
        foreach ($s in $present) {
            if ($Action -eq 'disable') { Set-Service -Name $s.Name -StartupType Disabled }
            if ($s.Status -ne 'Stopped') { Stop-Service -Name $s.Name -Force; Write-Host "stopped $($s.Name)" }
        }
        Get-Process -Name $processes -ErrorAction SilentlyContinue | Stop-Process -Force
        Write-Host 'Razer lighting stack stopped. Quit Synapse from the tray too if it is still showing.'
    }
    'restore' {
        if (Test-Path $state) {
            foreach ($s in (Get-Content $state | ConvertFrom-Json)) {
                Set-Service -Name $s.Name -StartupType $s.StartType
                if ($s.StartType -eq 'Automatic') { Start-Service -Name $s.Name; Write-Host "started $($s.Name)" }
            }
            Remove-Item $state
        } else {
            Write-Host 'No saved state; starting services that exist.'
            $present | Where-Object StartType -ne 'Disabled' | Start-Service
        }
        Write-Host 'Start Razer Synapse again from the Start menu to bring its UI back.'
    }
}
