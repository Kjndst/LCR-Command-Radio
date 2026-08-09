<p align="center">
  <img src="client/assets/lcr-banner.png"
       alt="LCR — Linh Lan Bang Command Radio"
       width="680">
</p>

# LCR — Linh Lan Bang Command Radio

> COMMAND — CONNECT — COORDINATE

A lightweight Discord command-radio layer for guild/team coordination.

**Maturity:** Beta / functional live deployment. One Commander/Owner plus four
distinct Speaker bots have been live-tested. This is not a claim of unlimited
Speaker scale.

## Product model

- A Commander in the Command Channel broadcasts to configured Units.
- Unit Leaders normally speak only inside their own Unit.
- Holding the LCR Radio Key additionally uplinks a Unit Leader to Command.
- There is no Unit-to-Unit relay.

## Components

1. **LCR Server** — Linux/amd64 Docker runtime for the Owner/Commander bot and
   one distinct Speaker bot per configured Unit.
2. **LCR Helper** — Windows desktop radio-key controller for Unit Leaders; it
   controls the radio gate and does not carry microphone audio.

## Self-host quick start

For the tested Linux Docker restore/migration path, prerequisites and security
rules, see [Tự host / di chuyển máy chủ](#tự-host--di-chuyển-máy-chủ-tiếng-việt)
later in this README.

GitHub Releases should distribute `LCR.exe` and `LCR-Server.zip`. Do not commit
`image.tar` directly to Git history.

## Upstream and licensing

LCR builds on [`sealbro/go-discord-caller`](https://github.com/sealbro/go-discord-caller)
at the pinned compatibility commit
[`9845af2e26d7e39ad15843a674102311b1ef43df`](UPSTREAM_BASE.txt). The LCR-original
source in this repository is licensed under [Apache-2.0](LICENSE). LCR adds the
Command Radio role and gate behavior, Windows Helper, Docker deployment overlay,
and self-host packaging. It is an independent project and is not affiliated with
the upstream project or its maintainers.

See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and the included license
texts for dependency and runtime attribution.

## Goal

```text
Shotcaller / Command room
        |
        +----> every bound Team room

Lead normal speech
        +----> own Team room only

Lead + Command Radio key
        +----> own Team room
        +----> Shotcaller

Team A  -X-> Team B
```

The system uses official Discord bots. No self-bot/user-token automation.

## Lead UX

1. Join the normal Discord team voice room.
2. Install one small Windows helper.
3. Pair once and choose a Radio key (`Mouse5` default).
4. Talk to the team normally with Free Mic / Voice Activity.
5. Hold Radio only when the Shotcaller should hear the message.
6. Exit or uninstall normally whenever desired.

Lead users do **not** configure Discord roles, channel IDs, bot tokens, host URLs, relays, mixers or tunnels.

Default client behavior:

- Free Mic / Voice Activity;
- Hold-to-Transmit;
- Mouse5;
- tray green = connected/standby;
- tray red = transmitting;
- tray gray = disconnected;
- fail closed on helper/network loss.

## Architecture

### Audio plane

`RaidModeCommandGuildCaller` reuses the upstream star topology from `sealbro/go-discord-caller`.

```text
Owner / Shotcaller audio
        +----> Speaker bot Team 1
        +----> Speaker bot Team 2
        +----> Speaker bot Team 3
        +----> Speaker bot Team 4

Team Lead audio received by speaker bot
        |
        +-- caller role? no  -> drop
        +-- caller role? yes
                |
                +-- command gate CLOSED -> drop before decode/fanout
                +-- command gate OPEN   -> Shotcaller hub mixer
```

### Control plane

The Windows helper carries **no audio**. It sends only radio state:

- `POST /v1/pair` — one-time code -> signed device token;
- `POST /v1/state` — OPEN/CLOSED;
- `POST /v1/heartbeat` — keeps an OPEN gate alive;
- `GET /v1/status` — connection probe.

Current defaults: heartbeat ~250 ms; stale-open timeout ~900 ms. Lost helper/network therefore closes the gate.

### Client

Single native Windows Go/Win32 executable; no Electron/.NET runtime. The stable host URL is embedded for production builds.

### Host

Primary deployment target:

```text
x86_64 Linux
  Docker Engine + Compose
    patched Discord owner + speaker bot process
      radio API :17777 (loopback publication during smoke)
```

Reusable host tools remain outside the project. Do not permanently install compiler/libdave tooling on the weak server just to build this project if a prebuilt image is practical.

## Tự host / di chuyển máy chủ (Tiếng Việt)

### LCR là gì

**LCR — Linh Lan Bang Command Radio** (`COMMAND — CONNECT — COORDINATE`) là hệ thống relay thoại Discord bằng bot chính thức, dành cho một kênh Command và nhiều kênh Unit. Owner/Commander bot phát tiếng nói của người có **Commander Role** từ Command Channel đến các Unit đã cấu hình. Mỗi Unit dùng một Speaker bot riêng.

- Người có **Unit Leader Role** nói bình thường chỉ nghe trong Unit của mình.
- Giữ Radio Key của Helper sẽ mở uplink đến Command; nhả phím sẽ đóng uplink.
- Nguồn Unit luôn cần **Unit Leader Role + radio gate OPEN**; gate fail-closed.
- Không có relay Unit-to-Unit.
- Commander không cần pair Helper; `/radio-pair` dành cho Unit Leader.

Đã kiểm chứng live với Commander và Speaker 1–4 theo cả hai chiều. Đây **không phải** là bằng chứng cho số Speaker không giới hạn.

### Helper khác máy chủ

- `LCR.exe` là Helper Windows cho Unit Leader: pair một lần, chọn Radio Key và gửi trạng thái OPEN/CLOSED. Helper không thu, mã hóa, hay relay microphone.
- Máy chủ LCR là Linux amd64 chạy Docker: chứa Owner/Commander bot, Speaker bot, Command Radio runtime và dữ liệu binding Discord.

LLB hiện triển khai tại `https://lcr.tail7b8791.ts.net`; đây là tham chiếu cho
deployment hiện tại, không phải endpoint self-host để sao chép. Người tự host
dùng endpoint riêng, ví dụ `https://your-lcr-endpoint.example`, qua Server
Address của Helper; cấu hình đã lưu của người dùng vẫn là nguồn ưu tiên. Cài
đặt Helper và config hiện có không thay đổi.

### Gói portable `LCR-Server.zip`

`LCR-Server.zip` là GitHub Release asset hoặc portable artifact tạo ở máy local, dùng cho backup/migration/self-host đã được kiểm thử. Nó chứa image Docker Linux amd64 chính xác, `compose.yaml`, scripts và snapshot `data/store.yaml`. Nó phù hợp để khôi phục hoặc chuyển host, không phải để chạy trực tiếp trên Windows.

- Windows có thể dùng để **lưu/chuyển** ZIP và chạy `LCR.exe`.
- Image server bên trong chỉ dành cho **Linux amd64 + Docker**.
- `.env.host` không có trong ZIP. Không in, log, commit hoặc gửi token Discord.
- `store.yaml` là snapshot binding tại thời điểm đóng gói. Nếu chuyển sang guild khác hoặc setup đã đổi, chạy `/setup` lại hoặc dùng snapshot đáng tin cậy mới hơn.

### Điều kiện trước khi restore

- Linux amd64 (Linux/VPS là mục tiêu đã được kiểm chứng), Docker Engine và Docker Compose plugin.
- Mạng outbound cho Discord/Tailscale và `curl` trên host.
- Một file `.env.host` riêng tư, đáng tin cậy, đặt cạnh `compose.yaml` sau khi giải nén. Không chép file này vào Git hoặc ZIP.

### Restore hoặc migrate

1. Chuyển `LCR-Server.zip` từ Windows sang Linux qua USB, LAN share hoặc SCP, ví dụ: `scp LCR-Server.zip user@server:~/`.
2. Trên Linux, giải nén vào một thư mục làm việc rồi vào thư mục đó:

   ```bash
   mkdir -p "$HOME/lcr-server"
   unzip LCR-Server.zip -d "$HOME/lcr-server"
   cd "$HOME/lcr-server"
   ```

3. Chép `.env.host` riêng tư vào thư mục hiện tại, cạnh `compose.yaml`.
4. Load image và khởi động bằng script có sẵn:

   ```bash
   bash scripts/install.sh
   ```

5. Kiểm tra local health và container:

   ```bash
   bash scripts/status.sh
   ```

Gói portable đã được chứng minh với image `llb-command-radio:0.3.2`, local `/healthz` HTTP 204, Funnel health HTTP 204, restart policy `unless-stopped`, và full live Discord routing hai chiều. API luôn phải giữ loopback-only tại `127.0.0.1:17777:17777`; **không bao giờ public raw port 17777**.

Nếu Helper cần pair từ xa, cấu hình Tailscale/Funnel **sau khi** local health PASS. Funnel/Tailscale không nằm trong portable ZIP.

### Update, rollback và GitHub

- Trước khi migrate/update, giữ một ZIP portable đã kiểm thử, `.env.host` riêng tư và snapshot `store.yaml` đáng tin cậy ở nơi bảo mật.
- Xác minh local health trước, rồi mới chuyển Helper từ xa sang host mới. Nếu restore thất bại, dừng host mới và quay về image/snapshot đã kiểm thử.
- Git chỉ nên chứa source, docs và scripts. Không commit `image.tar` hoặc `.env.host`; `LCR-Server.zip` và `LCR.exe` phù hợp hơn làm GitHub Release assets. Tài liệu này không tự push hay upload bất cứ asset nào.

## Roadmap

- **P0 — Protocol/core:** PASS — one-time pairing, signed device token, OPEN/CLOSED gate, heartbeat, fail-closed timeout.
- **P1 — Lightweight client:** PASS — native Win32 helper; fresh pairing, standby, Mouse5 transmit and release-to-standby manually verified.
- **P2 — Linux Docker host:** PASS — reproducible pinned host path and Command Radio runtime validated with 1 owner + 1 speaker.
- **P3 — Real Discord relay:** PASS — Command-channel downlink and fail-closed gated unit uplink are functionally accepted; legacy modes remain preserved.
- **P4 — Controlled tester rollout:** NEXT — distribute the single helper EXE to a limited tester group.
- **P5 — Multi-unit reliability:** validate reconnect/failure behavior, latency, and 3–4 simultaneous speaker rooms before broader release work.

## Current host smoke

For Linux Docker work, start with:

```bash
./host/scripts/preflight.sh
./host/scripts/prepare-server.sh
cp host/.env.host.example host/.env.host
./host/scripts/new-secret.sh
# fill owner token + ONE speaker token + generated radio secret
./host/scripts/start.sh
./host/scripts/status.sh
```

`prepare-server.sh` verifies the pinned upstream archive and runs the guarded
patcher in a disposable `golang:1.23-bookworm` container; no Go toolchain or
patcher binary is installed on the host. The first Discord smoke is exactly
one owner bot plus one speaker bot. Keep port `17777` loopback-only during
smoke; do not expose it publicly.

The generated `server/` tree is derived from the pinned upstream commit and is **not canonical project source**.

## Non-goals

- self-bots or automated normal Discord accounts;
- multiple human alt accounts as the production architecture;
- microphone capture/encoding in the helper;
- Electron/.NET runtime dependency for the Lead client;
- cross-talk between Team rooms;
- exposing bot/admin complexity to Leads;
- a governance database, dashboard, harness, agent fleet or parallel continuity system;
- auto-starting heavy host runtimes when the project is not in use;
- unbounded product expansion before controlled tester feedback.
