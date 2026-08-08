# Current State

**Baseline:** Hybrid-Lite 0.3.2  
**Current phase:** Pre-latency/reliability investigation
**Pinned upstream:** `sealbro/go-discord-caller @ 9845af2e26d7e39ad15843a674102311b1ef43df`

## Last verified

- **LCR functional slice: PASS.**
- Linux Docker Command mode is functional: Owner and Speaker gateways, local `/start` Command default, Commander-only Command-channel broadcast, Unit Leader plus open device gate unit uplink, and legacy modes are accepted.
- Helper RC5 is functional: native one-EXE install, pairing, arbitrary Radio Key capture, Mouse5 default, fail-closed gate/heartbeat, tray behavior, Known Folder Desktop shortcut, and uninstall cleanup are accepted.
- Controlled tester build produced: `bin/LCR.exe`.
- Remote pairing and audio through the current test tunnel: **PASS**. The helper's Server Address is configurable through Settings, and the Remote Settings visual/manual usability repair is accepted.
- **LCR 4-Speaker bidirectional live pass:** one Owner/Commander bot plus Speakers 1–4 were live. Commander downlink reached Units 1–4; ordinary Unit Leader speech stayed local; held radio-key uplink reached Command only; release closed it; no Unit-to-Unit leakage was observed. Speaker 4 used the next numbered Speaker token and the existing `/setup` binding flow.
- `/setup` Add Another Speaker continuation is functional and manually smoke-tested. Its current presentation is a deferred, non-blocking UX refinement; do not redesign it without a separate approved task.

## Locked decisions

- Lead UX: install one helper -> pair once -> choose Radio key -> done.
- Admin owns Discord role setup, bot tokens, channel bindings and deployment.
- Command topology: Shotcaller -> all Team rooms; authorized Lead -> Shotcaller only while radio gate is OPEN; Team rooms do not cross-talk.
- Free Mic + Hold + Mouse5 is the default.
- Gate is fail-closed.
- Primary bot-host target is x86_64 Linux + Docker.
- First real Discord smoke is 1 owner + 1 speaker; do not scale to 3–4 speakers before that passes.
- Controlled tester rollout precedes wider operational rollout.

## Next phase

Investigate audio latency and multi-unit reliability (reconnect/failure behavior) before broader rollout work. Four live Speakers are verified; this is not evidence of an arbitrary or unlimited Speaker count.
