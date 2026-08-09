# Third-Party Notices

LCR is an independent project. It builds from the pinned compatibility source
[`sealbro/go-discord-caller`](https://github.com/sealbro/go-discord-caller) at
`9845af2e26d7e39ad15843a674102311b1ef43df`; the upstream work is licensed under
Apache License 2.0. The generated `server/` tree is not this repository's public
source authority: reproduce it from that pin plus `host/patcher/` and
`server-patch/overlay/`.

LCR-original source is licensed under Apache License 2.0; the repository root
[`LICENSE`](LICENSE) is the applicable Apache license copy. LCR changes include
the Command Radio role/gate overlay, Helper, host tooling, and release package.
LCR is not affiliated with upstream or the organizations named below.

## Direct source dependencies

- `github.com/disgoorg/disgo` `v0.19.3` — Apache License 2.0.
- `github.com/disgoorg/godave/golibdave` `v0.3.0` — Apache License 2.0.
- Discord `libdave` `v1.1.1/cpp` — MIT License. The Docker source requests
  `v1.1.1`; the upstream tag name includes `/cpp`.
- `github.com/hraban/opus`
  `v0.0.0-20260708213942-bde8e4304501` — MIT License. Its license text is in
  [`LICENSES/hraban-opus-MIT.txt`](LICENSES/hraban-opus-MIT.txt).
- `libopus` — BSD-style COPYING. The exact package version in the runtime image
  is recorded only where the image inventory can identify it; source builds use
  an unpinned distribution package input.

## Windows Helper

`LCR.exe` is built with Go `go1.23.12` and the Go standard library. The Go
license text is in [`LICENSES/Go-LICENSE.txt`](LICENSES/Go-LICENSE.txt).

## Runtime inventory scope

`release/server.spdx.json` is the SBOM for the exact distributed Linux/amd64
Docker archive. It inventories what the scanner can identify in the final image;
it is not a complete source-build provenance record. In particular, the Docker
build inputs for Go, distroless, apt packages, and the `godave` checkout are not
all source-pinned in the upstream Dockerfile. Refer to the image SHA-256 stated
in the release/package manifest before relying on that SBOM.
