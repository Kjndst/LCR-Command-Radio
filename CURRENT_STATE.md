# Current State

**Baseline:** Hybrid-Lite 0.3.2  
**Current phase:** P2 — Linux Docker host  
**Pinned upstream:** `sealbro/go-discord-caller @ 9845af2e26d7e39ad15843a674102311b1ef43df`

## Last verified

- Windows native helper fresh pairing: PASS.
- Helper `STANDBY` state after pairing: PASS.
- Hold `Mouse5` -> `TRANSMITTING`: PASS.
- Release `Mouse5` -> standby: PASS.
- Free Mic remains the default local voice model.
- Radio registry/core race tests from the implementation slice: PASS.
- Linux amd64 guarded patcher cross-build from the smoke slice: PASS.
- Linux host shell scripts passed syntax validation in the development environment.

## Locked decisions

- Lead UX: install one helper -> pair once -> choose Radio key -> done.
- Admin owns Discord role setup, bot tokens, channel bindings and deployment.
- Command topology: Shotcaller -> all Team rooms; authorized Lead -> Shotcaller only while radio gate is OPEN; Team rooms do not cross-talk.
- Free Mic + Hold + Mouse5 is the default.
- Gate is fail-closed.
- Primary bot-host target is x86_64 Linux + Docker.
- First real Discord smoke is 1 owner + 1 speaker; do not scale to 3–4 speakers before that passes.
- UI polish waits until the real Discord audio path is proven.

## Current slice

Turn the P2 Docker smoke material into a maintainable canonical host path without adding governance/runtime bloat, then run the first container and Discord 1+1 smoke.

Target server class: old i3-3220-class CPU, 4 GB RAM. Runtime efficiency matters more than making that machine compile toolchains repeatedly.

## Known gaps / unverified

- Full patched Discord voice engine has not yet been built and run in Docker in the target environment.
- DAVE/libdave end-to-end Discord voice relay is not yet verified.
- Real Shotcaller -> Team downlink is not yet verified.
- Real gated Team Lead -> Shotcaller uplink is not yet verified.
- End-to-end latency, reconnect behavior and 3–4 simultaneous speaker rooms are not yet measured.
- Bounded pre-roll/release-tail audio polish is not implemented yet.

## Next safe action

Use Codex on this canonical repository for **P2 only**: audit the existing Docker/patch path, remove avoidable duplication, make the Linux host path reproducible and lightweight, run all locally available tests/build checks, and return exact commands/evidence for a 1-owner + 1-speaker Docker smoke.

Do not redesign the product, create new governance surfaces, expose the API publicly, or advance to P3/P4 automatically.
