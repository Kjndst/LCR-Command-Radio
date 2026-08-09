[CmdletBinding()]
param([string]$ImageTar)

$ErrorActionPreference = 'Stop'
$expectedImageSha256 = '1185829926f797833c8cad293a13682be72094d65493fd24dd4147ab63b57593'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if ([string]::IsNullOrWhiteSpace($ImageTar)) { $ImageTar = Join-Path $projectRoot 'bin\LCR-Server\image.tar' }
$imageTarPath = (Resolve-Path $ImageTar).Path
$storePath = Join-Path $projectRoot 'host\data\store.yaml'
$sbomPath = Join-Path $projectRoot 'release\server.spdx.json'
$outputPath = Join-Path $projectRoot 'bin\LCR-Server.zip'

function Assert-File([string]$Path, [string]$Name) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Name is required: $Path" }
}

Assert-File $imageTarPath 'Exact image archive'
Assert-File $storePath 'Store snapshot'
Assert-File $sbomPath 'SBOM'
$imageHash = (Get-FileHash -LiteralPath $imageTarPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($imageHash -ne $expectedImageSha256) { throw "Refusing unexpected image.tar SHA-256: $imageHash" }

$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("lcr-server-package-" + [Guid]::NewGuid().ToString('N'))
try {
    New-Item -ItemType Directory -Path $stage | Out-Null
    'data', 'scripts', 'LICENSES' | ForEach-Object { New-Item -ItemType Directory -Path (Join-Path $stage $_) | Out-Null }
    Copy-Item -LiteralPath $imageTarPath -Destination (Join-Path $stage 'image.tar')
    Copy-Item -LiteralPath $storePath -Destination (Join-Path $stage 'data\store.yaml')
    Copy-Item -LiteralPath $sbomPath -Destination (Join-Path $stage 'server.spdx.json')
    Copy-Item -LiteralPath (Join-Path $projectRoot 'host\scripts\runtime-env.sh') -Destination (Join-Path $stage 'scripts\runtime-env.sh')
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $projectRoot 'THIRD_PARTY_NOTICES.md') -Destination $stage
    Get-ChildItem -LiteralPath (Join-Path $projectRoot 'LICENSES') -File | ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $stage 'LICENSES') }

    @'
name: llb-command-radio
services:
  bot:
    image: llb-command-radio:0.3.2
    container_name: llb-command-radio
    env_file:
      - .env.host
    user: "${LCR_RUNTIME_UID}:${LCR_RUNTIME_GID}"
    environment:
      LLB_RADIO_ADDR: 0.0.0.0:17777
      STORE_PATH: /data/store.yaml
    ports:
      - "127.0.0.1:17777:17777"
    volumes:
      - ./data:/data
    restart: unless-stopped
    init: true
    mem_limit: 1536m
    cpus: 2.0
    pids_limit: 128
    stop_grace_period: 15s
    read_only: true
    tmpfs:
      - /tmp:size=64m,noexec,nosuid
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"
'@ | Set-Content -LiteralPath (Join-Path $stage 'compose.yaml') -Encoding utf8

    @'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/runtime-env.sh"
command -v docker >/dev/null || { echo "FAIL: Docker Engine required"; exit 2; }
command -v curl >/dev/null || { echo "FAIL: curl required"; exit 2; }
docker info >/dev/null 2>&1 || { echo "FAIL: Docker daemon unavailable"; exit 2; }
docker compose version >/dev/null 2>&1 || { echo "FAIL: Docker Compose plugin required"; exit 2; }
[ -f "$ROOT/.env.host" ] || { echo "FAIL: $ROOT/.env.host missing"; exit 3; }
chmod 600 "$ROOT/.env.host"
ensure_lcr_runtime_env "$ROOT"
docker load -i "$ROOT/image.tar"
docker image inspect llb-command-radio:0.3.2 >/dev/null 2>&1 || {
    echo "FAIL: image.tar did not provide llb-command-radio:0.3.2"
    echo "Use the approved portable image archive; install.sh never builds images."
    exit 4
}
cd "$ROOT"
docker compose --env-file .env.runtime -f compose.yaml up -d --no-build bot
for i in $(seq 1 60); do
    if curl -fsS -o /dev/null http://127.0.0.1:17777/healthz; then
        echo "PASS: LCR healthy"; docker compose --env-file .env.runtime -f compose.yaml ps; exit 0
    fi
    sleep 1
done
echo "FAIL: healthz not ready"
docker compose --env-file .env.runtime -f compose.yaml logs --tail 120 bot
exit 4
'@ | Set-Content -LiteralPath (Join-Path $stage 'scripts\install.sh') -Encoding utf8

    @'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/runtime-env.sh"
cd "$ROOT"
ensure_lcr_runtime_env "$ROOT"
docker compose --env-file .env.runtime -f compose.yaml ps
echo
docker stats --no-stream llb-command-radio || true
echo
curl -sS -o /dev/null -w 'healthz: HTTP %{http_code}\n' http://127.0.0.1:17777/healthz || true
'@ | Set-Content -LiteralPath (Join-Path $stage 'scripts\status.sh') -Encoding utf8

    @'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT/scripts/runtime-env.sh"
cd "$ROOT"
ensure_lcr_runtime_env "$ROOT"
docker compose --env-file .env.runtime -f compose.yaml stop bot
'@ | Set-Content -LiteralPath (Join-Path $stage 'scripts\stop.sh') -Encoding utf8

    @'
LCR — Linh Lan Bang Command Radio
Gói máy chủ Linux di động (Linux amd64)

Gói gồm image Docker, compose.yaml, scripts/, data/store.yaml (snapshot cấu
hình), SBOM, thông báo bên thứ ba và license texts. Gói không chứa .env.host,
Discord token, device token hoặc cấu hình Tailscale riêng tư.

Yêu cầu: Linux amd64, Docker Engine và Docker Compose plugin. LCR Helper là ứng
dụng Windows riêng cho Unit Leader; Helper không nằm trong gói máy chủ này.

CÀI ĐẶT / KHÔI PHỤC

  mkdir -p "$HOME/lcr-server"
  unzip LCR-Server.zip -d "$HOME/lcr-server"
  cd "$HOME/lcr-server"
  # đặt .env.host riêng tư cạnh compose.yaml
  bash scripts/install.sh
  bash scripts/status.sh

Script chỉ load image đã đóng gói và khởi động với `--no-build`; không tự build
image. Nếu `llb-command-radio:0.3.2` không có sau khi load, script sẽ dừng rõ
ràng. Source build là thao tác developer riêng biệt.

Khóa .env.host bằng chmod 600. API điều khiển chỉ publish loopback tại
127.0.0.1:17777; không mở trực tiếp cổng này ra Internet. Chỉ cấu hình Tailscale
hoặc tunnel sau khi /healthz cục bộ trả PASS.

Installer tự động chạy container bằng UID/GID của Linux user đang vận hành gói.
Metadata `.env.runtime` chỉ chứa UID/GID, được tạo tự động, host-local và không
phải secret; `.env.host` vẫn là cấu hình Discord/server riêng tư. Không dùng
`chmod 777` cho thư mục `data`.

data/store.yaml là snapshot để hỗ trợ migration, có thể cũ hơn binding hiện tại.
Dùng snapshot tin cậy mới hơn hoặc chạy /setup cho guild/cấu hình khác.

VẬN HÀNH

  bash scripts/status.sh   # trạng thái + healthz
  bash scripts/stop.sh     # dừng bot

Mô hình đã live-test: 1 Owner/Commander và 4 Speaker bots riêng biệt. Không có
cam kết hoặc tuyên bố đã xác minh Speaker thứ 5 trở lên.

NGUỒN GỐC VÀ LICENSE

LCR dùng upstream sealbro/go-discord-caller tại pin ghi trong THIRD_PARTY_NOTICES.md.
LCR-original source là Apache-2.0 (LICENSE). Xem THIRD_PARTY_NOTICES.md,
LICENSES/, và server.spdx.json để biết attribution và giới hạn inventory runtime.
'@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding utf8

    $payload = Get-ChildItem -LiteralPath $stage -Recurse -File | Where-Object { $_.Name -ne 'manifest.txt' } | Sort-Object { $_.FullName.Substring($stage.Length).Replace('\', '/') }
    $manifest = @('LCR PORTABLE SERVER MANIFEST', ('Image SHA256: ' + $expectedImageSha256), '', 'FILES / SHA256')
    foreach ($file in $payload) {
        $relative = $file.FullName.Substring($stage.Length).TrimStart('\').Replace('\', '/')
        $manifest += ('{0}  {1}' -f (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $relative)
    }
    Set-Content -LiteralPath (Join-Path $stage 'manifest.txt') -Value $manifest -Encoding utf8

    if (Test-Path -LiteralPath $outputPath) { Remove-Item -LiteralPath $outputPath -Force }
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $outputPath -CompressionLevel Optimal
    $archiveHash = (Get-FileHash -LiteralPath $outputPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $archiveSize = (Get-Item -LiteralPath $outputPath).Length
    Write-Output "PASS: $outputPath"
    Write-Output "ZIP SHA256: $archiveHash"
    Write-Output "ZIP bytes: $archiveSize"
}
finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}
