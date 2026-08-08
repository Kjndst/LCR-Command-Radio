[CmdletBinding()]
param(
    [string]$ProjectRoot = 'D:\Project\LLB Command Radio',
    [switch]$Refresh
)
$ErrorActionPreference = 'Stop'
$BaseCommit = '9845af2e26d7e39ad15843a674102311b1ef43df'
$ArchiveUrl = "https://github.com/sealbro/go-discord-caller/archive/$BaseCommit.zip"
$Server = Join-Path $ProjectRoot 'server'
$Patcher = Join-Path $ProjectRoot 'host\bin\patch-server.exe'

if (-not (Test-Path $ProjectRoot)) { throw "Project root not found: $ProjectRoot" }
if (-not (Test-Path $Patcher)) { throw "Project patcher missing: $Patcher" }

if (Test-Path $Server) {
    if (-not $Refresh) { throw "Generated server already exists: $Server. Refusing to overwrite. Use -Refresh only when you intentionally want to recreate it from the pinned base." }
    $marker = Join-Path $Server '.llb-generated-base'
    if (-not (Test-Path $marker) -or (Get-Content $marker -Raw).Trim() -ne $BaseCommit) {
        throw "Refusing -Refresh because $Server is not positively identified as our generated pinned checkout."
    }
    Remove-Item -LiteralPath $Server -Recurse -Force
}

$tempRoot = Join-Path $env:TEMP ("LLB-Radio-Prepare-" + [guid]::NewGuid().ToString('N'))
$zip = Join-Path $tempRoot 'upstream.zip'
$extract = Join-Path $tempRoot 'extract'
New-Item -ItemType Directory -Force -Path $extract | Out-Null
try {
    Write-Host "[DOWNLOAD] pinned upstream $BaseCommit"
    Invoke-WebRequest -Uri $ArchiveUrl -OutFile $zip -TimeoutSec 120
    Expand-Archive -LiteralPath $zip -DestinationPath $extract -Force
    $src = Get-ChildItem -LiteralPath $extract -Directory | Select-Object -First 1
    if (-not $src) { throw 'Upstream archive did not contain a source directory.' }
    Move-Item -LiteralPath $src.FullName -Destination $Server

    Write-Host '[PATCH] guarded Command Radio overlay'
    & $Patcher -repo $Server -project $ProjectRoot
    if ($LASTEXITCODE -ne 0) { throw "patch-server.exe failed with exit code $LASTEXITCODE" }
    Set-Content -LiteralPath (Join-Path $Server '.llb-generated-base') -Value $BaseCommit -NoNewline -Encoding ascii

    Write-Host 'PASS generated patched server source'
    Write-Host "Server: $Server"
    Write-Host "Base:   $BaseCommit"
    Write-Host 'No Git/Go/Python/libdave toolchain was installed on this host.'
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
