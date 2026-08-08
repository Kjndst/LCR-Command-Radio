[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$PublicUrl,
    [string]$Output = "",
    [string]$Version = "0.2.1"
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Client = Join-Path $Root 'client'
if (-not $Output) { $Output = Join-Path $Root 'bin\LLBCommandRadio.exe' }

$GoCandidates = @(
    'D:\Tools\Go\bin\go.exe',
    (Get-Command go.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -ErrorAction SilentlyContinue)
) | Where-Object { $_ -and (Test-Path $_) }
if (-not $GoCandidates) { throw 'Go was not found. Install/reuse it under D:\Tools; do not duplicate Go inside this project.' }
$Go = $GoCandidates[0]
if (-not $PublicUrl.StartsWith('https://')) { throw 'Production PublicUrl must use https://.' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null
Push-Location $Client
try {
    $env:GOOS='windows'; $env:GOARCH='amd64'; $env:CGO_ENABLED='0'
    & $Go build -trimpath -ldflags "-s -w -H=windowsgui -X main.buildVersion=$Version -X main.defaultServerURL=$PublicUrl" -o $Output .
    if ($LASTEXITCODE -ne 0) { throw 'client build failed' }
    Write-Host "PASS client built: $Output"
    Write-Host "Embedded server: $PublicUrl"
} finally { Pop-Location }
