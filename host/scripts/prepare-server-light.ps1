[CmdletBinding()]
param(
    [string]$ProjectRoot = 'D:\Project\LLB Command Radio',
    [switch]$Refresh
)
$ErrorActionPreference = 'Stop'
$BaseCommit = '9845af2e26d7e39ad15843a674102311b1ef43df'
$ArchiveSha256 = '3D241D2DD92A5796E54655B85924A70812B75302447F78C83F9ABAF6767E0F8E'
$ArchiveUrl = "https://github.com/sealbro/go-discord-caller/archive/$BaseCommit.zip"
$Server = Join-Path $ProjectRoot 'server'

if (-not (Test-Path $ProjectRoot)) { throw "Project root not found: $ProjectRoot" }
if (-not (Get-Command docker.exe -ErrorAction SilentlyContinue)) { throw 'Docker CLI is required; the patcher runs in an ephemeral Go container.' }
& docker.exe info *> $null
if ($LASTEXITCODE -ne 0) { throw 'Docker daemon is unavailable or permission was denied.' }

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
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash
    if ($hash -ne $ArchiveSha256) { throw "Pinned upstream checksum mismatch: expected $ArchiveSha256, got $hash" }
    Expand-Archive -LiteralPath $zip -DestinationPath $extract -Force
    $src = Get-ChildItem -LiteralPath $extract -Directory | Select-Object -First 1
    if (-not $src) { throw 'Upstream archive did not contain a source directory.' }
    Move-Item -LiteralPath $src.FullName -Destination $Server

    Write-Host '[PATCH] guarded Command Radio overlay'
    $mount = "${ProjectRoot}:/src"
    & docker.exe run --rm -v $mount -w /src/host/patcher golang:1.23-bookworm go run . -repo /src/server -project /src
    if ($LASTEXITCODE -ne 0) { throw "patch-server.exe failed with exit code $LASTEXITCODE" }
    Set-Content -LiteralPath (Join-Path $Server '.llb-generated-base') -Value $BaseCommit -NoNewline -Encoding ascii

    Write-Host 'PASS generated patched server source'
    Write-Host "Server: $Server"
    Write-Host "Base:   $BaseCommit"
    Write-Host 'No Git/Go/Python/libdave toolchain was installed on this host.'
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
}
