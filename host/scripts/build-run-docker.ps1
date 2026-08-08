[CmdletBinding()]
param([string]$ProjectRoot = 'D:\Project\LLB Command Radio')
$ErrorActionPreference='Stop'
$HostDir = Join-Path $ProjectRoot 'host'
$Compose = Join-Path $HostDir 'compose.yaml'
$EnvFile = Join-Path $HostDir '.env.host'
$Server = Join-Path $ProjectRoot 'server'
if (-not (Test-Path $Server)) { throw "Generated server missing: $Server. Run host\scripts\prepare-server-light.ps1 first." }
if (-not (Test-Path $EnvFile)) { throw "Host env missing: $EnvFile. Copy .env.host.example to .env.host and fill the bot tokens + secret." }
$text = Get-Content $EnvFile -Raw
if ($text -match 'REPLACE_WITH' -or $text -match 'DISCORD_OWNER_BOT_TOKEN=\s*$') { throw '.env.host still contains placeholders / empty owner token.' }

Push-Location $HostDir
try {
    & docker.exe compose -f $Compose build
    if ($LASTEXITCODE -ne 0) { throw 'docker compose build failed' }
    & docker.exe compose -f $Compose up -d
    if ($LASTEXITCODE -ne 0) { throw 'docker compose up failed' }
} finally { Pop-Location }

$deadline = (Get-Date).AddSeconds(40)
do {
    Start-Sleep -Milliseconds 750
    try {
        $r = Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:17777/healthz' -TimeoutSec 2
        if ($r.StatusCode -eq 204) {
            Write-Host 'PASS bot container started + radio API healthz'
            Write-Host 'Next admin step: configure Discord apps/tokens and then /setup in Discord.'
            exit 0
        }
    } catch {}
} while ((Get-Date) -lt $deadline)

Write-Host 'FAIL healthz did not become ready. Last container logs:'
Push-Location $HostDir
try { & docker.exe compose -f $Compose logs --tail 120 bot } finally { Pop-Location }
exit 1
