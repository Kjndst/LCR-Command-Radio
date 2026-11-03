# Third-Party Notices

LCR repository code is licensed under the root `LICENSE` (Apache-2.0). Exact current Rust dependency versions and sources are authoritative in `Cargo.lock`.

## Current Rust product

The Native-only Rust workspace uses Tokio, Twilight, Songbird and related media/crypto crates. The workspace manifest pins Songbird `0.6.0` to exact Git revision:

`e26486f4c4c169662a1b375e4312a15385f55d45`

Do not infer dependency versions from this notice; `Cargo.toml` and `Cargo.lock` are the exact build authority. Canonical CI also reviews the active production dependency graph and RustSec advisory set.

The active Rust media stack uses `opus2`/`libopus_sys`; the bundled libopus notice remains applicable. The current Rust dependency graph contains `davey`; the retained Discord MIT notice is preserved conservatively for current DAVE-related distribution obligations.

## Retired Windows Helper presentation assets

G5 removes the Windows Helper executable, Tabler icon assets and Jura font from the current source/distribution tree. The original Tabler MIT and Jura SIL OFL notices and their upstream provenance remain available with the archived Helper source in Git history. Existing pre-G5 binary releases retain their original licensing obligations; no licensing rights or notices are altered retroactively.
## Retired Go compatibility source

The former compatibility/regeneration path based on `sealbro/go-discord-caller` was removed from the current source tree after the Rust rewrite. Historical commits retain the exact patcher, overlay, upstream pin and historical notices if forensic reproduction is ever required. That historical Go source is not current build, validation, rollback or release authority.

## Artifacts

Binaries, Docker archives, SBOMs, ZIP/TAR packages and generated server trees are release/build artifacts, not durable source authority, and are intentionally not tracked on clean `main`. A release that distributes such an artifact must generate and ship the applicable notices/inventory for that exact artifact.

LCR is an independent project and is not affiliated with the upstream projects or organizations named above.
