[CmdletBinding()]
param([string]$ProjectRoot = 'D:\Project\LLB Command Radio')
$ErrorActionPreference='Continue'
$HostDir = Join-Path $ProjectRoot 'host'
$Compose = Join-Path $HostDir 'compose.yaml'
Write-Host '=== LLB COMMAND RADIO HOST STATUS ==='
try { & docker.exe compose -f $Compose ps } catch { Write-Host "Docker status unavailable: $($_.Exception.Message)" }
try {
    $r = Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:17777/healthz' -TimeoutSec 3
    if ($r.StatusCode -eq 204) { Write-Host 'PASS radio API localhost healthz' }
} catch { Write-Host "FAIL radio API localhost: $($_.Exception.Message)" }
try {
    $svc = Get-Service cloudflared -ErrorAction Stop
    Write-Host "cloudflared service: $($svc.Status)"
} catch { Write-Host 'INFO cloudflared Windows service not found' }
