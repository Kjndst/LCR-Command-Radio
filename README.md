# LLB Command Radio

Lightweight Discord command-radio relay for large-group PvP/GvG.

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

Single native Windows Go/Win32 executable; no Electron/.NET runtime. The stable host URL is embedded for production builds. UI polish is intentionally deferred until the real Discord path is proven.

### Host

Primary deployment target:

```text
x86_64 Linux
  Docker Engine + Compose
    patched Discord owner + speaker bot process
      radio API :17777 (loopback publication during smoke)
```

Reusable host tools remain outside the project. Do not permanently install compiler/libdave tooling on the weak server just to build this project if a prebuilt image is practical.

## Roadmap

- **P0 — Protocol/core:** PASS — one-time pairing, signed device token, OPEN/CLOSED gate, heartbeat, fail-closed timeout.
- **P1 — Lightweight client:** PASS — native Win32 helper; fresh pairing, standby, Mouse5 transmit and release-to-standby manually verified.
- **P2 — Linux Docker host:** ACTIVE — canonicalize and validate the Docker/libdave host path; first real target is 1 owner + 1 speaker.
- **P3 — Real Discord relay:** NEXT — verify Shotcaller downlink, gated Lead uplink, reconnect/failure behavior; then scale to 3–4 speaker rooms.
- **P4 — Audio polish:** add bounded ~60–100 ms pre-roll and ~100–150 ms release tail; benchmark latency and simultaneous Leads.
- **P5 — UI/release polish:** bring native UI to the approved black minimalist mockup; final error/status polish and optional code signing. No auto-update until justified.

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
- polishing UI before the real Discord audio path is validated.
