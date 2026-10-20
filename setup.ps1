#Requires -RunAsAdministrator
<#
Checks that Npcap and the Albion Data client are installed, then blocks the client's
internet access with a Windows Firewall rule. Traffic to localhost is not affected,
so it can still send market data to the program's local listener.

Usage (admin PowerShell):  .\setup.ps1 [-ClientPath <path to albiondata-client.exe>]
#>
param(
    [string]$ClientPath
)

$ErrorActionPreference = 'Stop'
$ruleName = 'Block Albion Data Client internet'
$ok = $true

function Write-Ok($text)   { Write-Host "[ OK ] $text" -ForegroundColor Green }
function Write-Warn($text) { Write-Host "[WARN] $text" -ForegroundColor Yellow }

# --- 1. Npcap -------------------------------------------------------------
$npcapService = Get-Service -Name npcap -ErrorAction SilentlyContinue
$npcapDll = Test-Path "$env:SystemRoot\System32\Npcap\wpcap.dll"
$winpcapDll = Test-Path "$env:SystemRoot\System32\wpcap.dll"
if ($npcapService -or $npcapDll) {
    Write-Ok 'Npcap is installed.'
    if (-not $winpcapDll) {
        Write-Warn 'Npcap does not look like it was installed in "WinPcap API-compatible mode".'
        Write-Warn 'The client may not find it. Re-run the Npcap installer and tick that option.'
    }
}
else {
    Write-Warn 'Npcap is NOT installed. The client cannot capture traffic without it.'
    Write-Warn 'Download it from https://npcap.com and tick "WinPcap API-compatible mode".'
    $ok = $false
}

# --- 2. Albion Data client ------------------------------------------------
function Find-Client {
    if ($ClientPath) {
        if (Test-Path $ClientPath -PathType Leaf) { return (Resolve-Path $ClientPath).Path }
        Write-Warn "-ClientPath '$ClientPath' does not exist."
        return $null
    }

    $dirs = @()
    foreach ($key in 'HKLM:\SOFTWARE\Albion Data Client', 'HKLM:\SOFTWARE\WOW6432Node\Albion Data Client') {
        $dir = (Get-ItemProperty -Path $key -ErrorAction SilentlyContinue).'(default)'
        if ($dir) { $dirs += $dir }
    }
    $dirs += "$env:ProgramFiles\Albion Data Client"
    $dirs += "${env:ProgramFiles(x86)}\Albion Data Client"
    $dirs += "$env:LOCALAPPDATA\Programs\Albion Data Client"

    foreach ($dir in $dirs | Where-Object { $_ }) {
        $exe = Join-Path $dir 'albiondata-client.exe'
        if (Test-Path $exe -PathType Leaf) { return $exe }
    }
    return $null
}

$client = Find-Client
if ($client) {
    Write-Ok "Albion Data client found: $client"
}
else {
    Write-Warn 'The Albion Data client was NOT found.'
    Write-Warn 'Install it from https://github.com/ao-data/albiondata-client/releases'
    Write-Warn 'or pass its location: .\setup.ps1 -ClientPath C:\path\to\albiondata-client.exe'
    $ok = $false
}

# --- 3. Firewall rule -----------------------------------------------------
if ($client) {
    Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName $ruleName -Direction Outbound -Program $client -Action Block | Out-Null
    Write-Ok "Firewall rule '$ruleName' created: the client can no longer reach the internet (localhost still works)."
    Write-Host "       Remove it with: Remove-NetFirewallRule -DisplayName '$ruleName'"
}
else {
    Write-Warn 'Firewall rule NOT created because the client was not found.'
}

# --- Extra: the installer's autostart copy uploads to the public server ----
$task = Get-ScheduledTask -TaskName 'Albion Data Client' -ErrorAction SilentlyContinue
if ($task -and $task.State -ne 'Disabled') {
    Write-Warn 'The client starts automatically at logon with default settings (public upload).'
    Write-Warn 'Disable it with: Disable-ScheduledTask -TaskName "Albion Data Client"'
}
$running = Get-Process -Name albiondata-client -ErrorAction SilentlyContinue
if ($running) {
    Write-Warn "The client is already running (PID $($running.Id -join ', ')). Stop it and start it again with:"
    Write-Warn '  albiondata-client.exe -i http://localhost:8080'
}

Write-Host ''
if ($ok) {
    Write-Host 'Setup finished. Start the program (cargo run --release), then the client:' -ForegroundColor Cyan
    Write-Host "  & `"$client`" -i http://localhost:8080" -ForegroundColor Cyan
}
else {
    Write-Host 'Setup incomplete: fix the warnings above and run this script again.' -ForegroundColor Yellow
    exit 1
}
