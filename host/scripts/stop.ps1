[CmdletBinding()]
param([string]$ProjectRoot = 'D:\Project\LLB Command Radio')
$ErrorActionPreference='Stop'
$HostDir = Join-Path $ProjectRoot 'host'
Push-Location $HostDir
try { & docker.exe compose -f (Join-Path $HostDir 'compose.yaml') down } finally { Pop-Location }
