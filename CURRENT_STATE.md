# Current State

**Product:** LCR — Linh Lan Bang Command Radio (`COMMAND — CONNECT — COORDINATE`)
**Branch:** `feat/p2-linux-docker`
**Last proven implementation checkpoint:** `af763fcfdf22eec84f301eb99e548a336c972700` (`feat: release LCR Helper 0.0.1 Beta`)
**Previous Linux patch checkpoint:** `b4f9739bcf7a52741a79ec7af845f2fb0ef8fa2f`
**Pinned upstream:** `sealbro/go-discord-caller @ 9845af2e26d7e39ad15843a674102311b1ef43df`

## PASS

- Command topology is live-proven: one Command Channel, multiple Unit Channels, one Owner/Commander bot, and one distinct Speaker bot per Unit.
- Commander Role in Command Channel reaches configured Units. Commander needs no Helper pairing.
- Unit Leader normal speech remains in its own Unit. Unit source requires Unit Leader Role **and** OPEN radio gate; holding the LCR Radio Key uplinks Command, release closes it, and no Unit-to-Unit leak was observed.
- Commander -> Units 1/2/3/4, gated Unit uplink -> Command, remote Helper pairing, Linux migration, Tailscale Funnel, Helper 0.0.1 Beta smoke, and portable Docker restore with bidirectional live Discord audio: **PASS**.
- YAML store persistence + restart: **LIVE PASS**. LCR Unit 2's changed binding survived restart; the prior Speaker 2 old-room incident was stale on-disk state from the YAML permission failure, not a routing regression.
- Active-raid unexpected voice displacement: **LIVE PASS**. Manually disconnecting/moving LCR Unit 2 returned it to bound Unit 2 without `/start`. An intentional graceful restart still requires `/start` again by design; do not add persisted `raid_active` or crash-marker behavior.
- **LCR LINUX PHYSICAL COLD-BOOT RECOVERY — LIVE PASS.** After manual power-off/power-on, the bot/container and Windows Helper reconnected without server reconfiguration; existing pairing remained usable, manual unpair plus fresh one-time-code pairing passed, and `/start` started Command Radio. Intentional shutdown still does not resume an active Voice Raid automatically.
- CPU early-drop audit: **PASS / no fix recommended**. Unauthorized audio is filtered in `VoiceReceiver` before decode, mixer processing, or fanout.
- Public-release compliance is prepared: LCR-original source is Apache-2.0; the public README uses the approved LCR banner; third-party notices and license texts are included; no commit, push, or publication has occurred.

## CURRENT

- Helper release: `LCR - Command Radio` 0.0.1 Beta; FileVersion `0.0.1.0`; default endpoint `https://lcr.tail7b8791.ts.net`; DNS-specific pair error is implemented and tested. Install/config locations are unchanged.
- Proven host: Debian 13 + Xfce on Acer Aspire E5-575G (i3-6100U, 4 GB RAM, 4 GB swap); Docker + Compose and Tailscale enabled. Current observed LAN IP: `192.168.1.22` (operational detail; may change).
- Production runtime uses `llb-command-radio:0.3.2`, linux/amd64; restart policy `unless-stopped`; API stays `127.0.0.1:17777:17777`. The restore-proven archive image ID was `sha256:ef777965dd960fdf98f3acec5d1c66d15b0b13ba6127b61e740629dce5ca8b81`; an accidental start-script build later changed that tag identity, so no current production image ID is asserted here.
- YAML persistence fix is **LIVE PASS**: the container runs as the deployment Linux user's UID/GID through non-secret host-local `.env.runtime`, so it can atomically create the replacement temp file in host-owned `/data`. No routing bug was established.
- Start-script repair: normal start now requires an existing `llb-command-radio:0.3.2` and uses `--no-build`; missing images fail with instructions. Source build remains an explicit developer operation.
- Funnel `https://lcr.tail7b8791.ts.net` public `/healthz` is proven HTTP 204.
- Portable package: `bin/LCR-Server.zip`; tested image.tar SHA256 `1185829926f797833c8cad293a13682be72094d65493fd24dd4147ab63b57593` (unchanged); final no-build ZIP SHA256 `c1702cf546fd0ca3087def403e84881164b937e9d3f04263f73c2d01c2e311e9`. It includes the Apache license, third-party notices, component license texts, `release/server.spdx.json`, and the non-secret UID/GID runtime helper; install only loads the packaged image and uses `--no-build`. The SBOM inventories the final archive but cannot fully pin the upstream Docker build inputs.
- `data/store.yaml` in the portable ZIP is a usable setup snapshot, but may be older than current bindings. Use a newer trusted snapshot or run `/setup` for another guild/setup. `.env.host` is intentionally excluded.

## NEXT

1. Review this final checkpoint and decide whether to authorize a GitHub push/release; publication has not occurred.
2. Restore/reload the approved archive before any normal production start if the current tag identity is not trusted; normal start will not rebuild it.
3. Clean Debian Docker builder/cache after the restore-proven backup exists.
4. Safely clean main-PC build/AppData/transient LCR residue.
5. Perform unattended reboot proof: SSH, Docker, Tailscale, LCR container, local/Funnel health, and Discord reconnect/routing.
6. Make the final production-domain decision.

## DEFERRED

- `/setup` presentation refinement; keep its working continuation flow intact.
- Voice idle-latency measurement. Cause is uncertain; do not add guessed keepalive, DSP, or resampling workarounds without measurement.

## LOCKED RULES

- No Discord self-bot/user-token automation; Helper never captures microphone audio.
- Lead UX remains: install one Helper -> pair once -> choose Radio Key -> use it.
- Free Mic / Voice Activity, Hold + Mouse5 default, and fail-closed gate remain.
- Do not expose raw port 17777 publicly. Keep tokens and `.env.host` private; never print, log, hash, or commit them.
- Four live Speakers are proven. Do not claim Speaker 5+ or unlimited scale.
- Canonical continuity is only `AGENTS.md`, `README.md`, and this file. Source, docs and scripts stay in Git; `image.tar`, `.env.host`, portable ZIP, and Windows EXE belong outside commits (release assets when publication is chosen).
