[CmdletBinding()]
param(
    [string]$PublicUrl = "https://lcr.tail7b8791.ts.net",
    [string]$Output = "",
    [string]$Version = "0.0.1 Beta"
)
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Client = Join-Path $Root 'client'
if (-not $Output) { $Output = Join-Path $Root 'bin\LCR.exe' }

$GoCandidates = @(
    'D:\Tools\Go\bin\go.exe',
    (Get-Command go.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -ErrorAction SilentlyContinue)
) | Where-Object { $_ -and (Test-Path $_) }
if (-not $GoCandidates) { throw 'Go was not found. Install/reuse it under D:\Tools; do not duplicate Go inside this project.' }
$Go = @($GoCandidates)[0]
if (-not $PublicUrl.StartsWith('https://')) { throw 'Production PublicUrl must use https://.' }
$Winres = 'D:\Tools\go-winres\go-winres.exe'
if (-not (Test-Path $Winres)) { throw 'go-winres was not found at D:\Tools\go-winres\go-winres.exe. Install/reuse the external tool under D:\Tools.' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $Output) | Out-Null
Push-Location $Client
try {
    & $Winres make --in 'winres\winres.json' --arch 'amd64' --out 'rsrc'
    if ($LASTEXITCODE -ne 0) { throw 'Windows resource generation failed' }
    $env:GOOS='windows'; $env:GOARCH='amd64'; $env:CGO_ENABLED='0'
    $LdFlags = "-s -w -H windowsgui -X `"main.buildVersion=$Version`" -X main.defaultServerURL=$PublicUrl"
    & $Go build -trimpath -ldflags $LdFlags -o $Output .
    if ($LASTEXITCODE -ne 0) { throw 'client build failed' }
    Write-Host "PASS client built: $Output"
    Write-Host "Embedded server: $PublicUrl"
} finally { Pop-Location }
