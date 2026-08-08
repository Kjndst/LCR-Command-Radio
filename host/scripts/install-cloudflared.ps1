[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$TunnelToken,
    [string]$ToolRoot = 'D:\Tools\cloudflared'
)
$ErrorActionPreference='Stop'
if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script from an Administrator PowerShell. Installing a Windows service requires elevation.'
}
New-Item -ItemType Directory -Force -Path $ToolRoot | Out-Null
$Exe = Join-Path $ToolRoot 'cloudflared.exe'
if (-not (Test-Path $Exe)) {
    $url = 'https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe'
    Write-Host "[DOWNLOAD] official cloudflared -> $Exe"
    Invoke-WebRequest -Uri $url -OutFile $Exe -TimeoutSec 120
}
& $Exe --version
if ($LASTEXITCODE -ne 0) { throw 'cloudflared executable check failed' }
Write-Host '[SERVICE] installing remotely-managed Cloudflare Tunnel connector'
& $Exe service install $TunnelToken
if ($LASTEXITCODE -ne 0) { throw 'cloudflared service install failed' }
Start-Sleep -Seconds 2
Get-Service cloudflared | Format-Table Status,Name,DisplayName -AutoSize
Write-Host 'PASS cloudflared service installed. Configure the tunnel public hostname in Cloudflare to http://127.0.0.1:17777.'
