# Contributing to tv-shell

tv-shell is a Quickshell (QML) + Rust couch-gaming shell for Moonlight streaming on Hyprland. All changes go through the workflow below.

## Prerequisites

- **Rust** (stable toolchain — see `host/`, `daemon/`, and `panel/` for minimum versions; daemon requires ≥1.75 MSRV, host and panel crates need Cargo ≥1.85)
- **Qt 6.8** (for `qmlformat` and `qmltestrunner`)
- **Linux** with evdev/uinput access (daemon only; `host/` and `protocol/` build on Linux, macOS, and Windows; `panel/` builds on Linux/macOS — it dials the daemon's Unix-socket IPC unconditionally, so it does not build on Windows; `core/` is Linux-only — it is an X11 client and binds a Unix socket)

## Build, test & lint

### Daemon (`daemon/` — Linux only)

```bash
# Format check
cargo fmt --check -p tv-shell-input

# Lint
cargo clippy -p tv-shell-input --all-targets -- -D warnings

# Build (default, no C deps)
cargo build --release -p tv-shell-input

# Build with CEC + MCP (canonical deploy build — also run by CI)
./scripts/build-daemon.sh   # equivalent: cd daemon && cargo build --release --features cec,mcp

# Test
cargo test -p tv-shell-input
```

### Host sidecar (`host/` + `protocol/` — cross-platform)

```bash
cargo fmt --check -p tv-shell-host -p tv-shell-protocol
cargo clippy -p tv-shell-host -p tv-shell-protocol --all-targets -- -D warnings
cargo build -p tv-shell-host -p tv-shell-protocol
cargo test -p tv-shell-host -p tv-shell-protocol
```

The broker-backed MQTT tests are `#[ignore]`-gated behind `TV_SHELL_TEST_BROKER`,
so the line above stays fast and offline. To run them you need a broker — see
[docs/MQTT.md](docs/MQTT.md#testing).

### Panel (`panel/` — LAN web control panel, Linux/macOS)

```bash
cargo fmt --check -p tv-shell-panel
cargo clippy -p tv-shell-panel --all-targets -- -D warnings
cargo build --release -p tv-shell-panel   # equivalent: ./scripts/build-panel.sh
cargo test -p tv-shell-panel
```

### Core (`core/` — v2 gamescope core, Linux)

```bash
cargo fmt --check -p tv-shell-core
cargo clippy -p tv-shell-core --all-targets -- -D warnings
cargo build --release -p tv-shell-core
cargo test -p tv-shell-core
```

The X-backed integration test is `#[ignore]`-gated behind `TV_SHELL_TEST_XVFB`,
which names an X display (e.g. `:99`) — the same opt-in shape as the MQTT broker
tests above, so the line above stays offline and needs no display server. To run
it you need an X server:

```bash
Xvfb :99 -screen 0 1920x1080x24 &
TV_SHELL_TEST_XVFB=:99 cargo test -p tv-shell-core --test atoms_xvfb -- --ignored
```

Those tests **share that one server** — several write root-window properties and
one reads whole-root state — so they serialise on a process-wide lock inside
`core/tests/atoms_xvfb.rs`, and CI additionally passes `--test-threads=1`. Run in
parallel they raced, and it presented as a *connection* error
(`failed to read whole buffer`), not an assertion. Keep both.

`core/tests/input_uinput.rs` is gated the same way, on `TV_SHELL_TEST_UINPUT`. It
covers the one part of the input layer no unit test can reach: that the kernel
accepts the canonical pad profile, publishes a devnode for it, and that discovery
then refuses that devnode as ours.

```bash
sudo modprobe uinput
TV_SHELL_TEST_UINPUT=1 cargo test -p tv-shell-core --test input_uinput -- --ignored
```

**It needs two permissions, not one.** Writing `/dev/uinput` is enough to CREATE
a presenter and not enough to read one back: the `/dev/input/eventN` the kernel
publishes for it is `root:input 0660` with no ACL, so on a desktop session
(where logind grants an ACL on `/dev/uinput` alone) creation succeeds and every
readback fails `EACCES` — and `evdev::enumerate()` silently omits the device,
because it skips what it cannot open. Join the `input` group (`sudo usermod -aG
input "$USER"`, then log out and back in) or run the suite as root. The test's
preflight checks both halves and fails naming the group, rather than letting the
permission error surface as a confusing assertion deep inside a test.

**Not wired into CI.** `/dev/uinput` is a kernel device, not an apt package, so
whether a GitHub-hosted runner can `modprobe uinput` has to be *observed*, not
assumed from the runner's shape — that assumption is how the Xvfb tests spent
months compiled everywhere and run nowhere.

The default `cargo test` above also **runs `scripts/install-v2.sh`** into a
scratch tree under `target/` and asserts on the files it writes (no leftover
prefix token, no path under v1's prefix, the session entry's name). So that
script must stay lint-clean and executable:

```bash
shellcheck -x core/units/*.sh scripts/install-v2.sh
```

### QML shell (`shell/` — no build step; formatting and headless tests)

```bash
# Format all QML files in-place (Qt 6.8 qmlformat)
find . -name '*.qml' -not -path './worktrees/*' -not -path './.git/*' \
  -exec qmlformat -i {} +

# Headless QML unit tests (offscreen, no compositor)
./tests/qml/run.sh
```

### gamescope kit (`dev/gamescope/` — offline shell fixture)

The kit's decision logic runs against a fake X display and fake clients — no
gamescope, no Steam, no Moonlight, no network, no display server:

```bash
shellcheck -x dev/gamescope/*.sh dev/gamescope/tests/*.sh dev/gamescope/tests/bin/*
./dev/gamescope/tests/run.sh         # Moonlight + focus/tagging  (57 assertions)
./dev/gamescope/tests/run-steam.sh   # Steam / Steam Link / env    (69 assertions)
```

See [dev/gamescope/tests/README.md](dev/gamescope/tests/README.md).

## Documentation

Keep documentation current as part of the change — update the README and any affected docs (`docs/`, `config/*.example`) in the same PR. A new IPC command belongs in `docs/IPC_PROTOCOL.md`; a new daemon config key belongs in `config/config.toml.example`.

## Before you open a PR

- Make sure all CI checks pass locally first — run the formatter, linter, and tests for the area you changed (Rust, host, or QML).

## Branching & commits

- Branch off `main`; never commit directly to `main`.
- Use [Conventional Commits](https://www.conventionalcommits.org/) prefixes (`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `test:`, …).
- Sign your commits where possible (`git commit -S`).
- Keep each PR focused; delete dead code rather than commenting it out.

## Pull requests

- Open the PR against `main`.
- Every PR runs CI (the single required check is `CI / ci-gate`; path-based filters skip areas you did not touch). Resolve **all** review threads before the PR is merged.
- An automated code review runs on each PR; address and resolve its threads like any other review.
- A PR can be merged once CI is green and all review threads are resolved.

## Releases

Each artifact is versioned independently within the monorepo, so a release trigger has to say *which* artifact to cut. The two binary streams are **opt-in via a component-scoped label on the merged PR**; the widget stream stays tag-driven (a label can't carry the widget id).

- **`semver:host:patch|minor|major`** → [`.github/workflows/release-host.yml`](.github/workflows/release-host.yml) tags `host-v<semver>`, builds `tv-shell-host` for linux-musl, macOS (arm64 + x86_64), and Windows, then publishes a single GitHub Release with all binaries and a `checksums.txt`.
- **`semver:input:patch|minor|major`** → [`.github/workflows/release-input.yml`](.github/workflows/release-input.yml) tags `input-v<semver>`, builds `tv-shell-input` with `--features cec,mcp` on Linux (glibc 2.41 / Trixie), and publishes the binary and `checksums.txt`.
- **`widget-<id>-v<semver>` tag push** → [`.github/workflows/release-widget.yml`](.github/workflows/release-widget.yml) publishes a notes-only GitHub Release for the named QML widget. No binary is built — QML runs interpreted at runtime. Example: `git tag widget-moonlight-v1.1.0 && git push origin widget-moonlight-v1.1.0`.

To cut a binary release: add the label to your PR before merging it. **No label → no release**, so chore merges stay quiet, and a `semver:host:*` label never cuts a daemon release (or vice versa). Both binary workflows also accept a manual **workflow_dispatch** with a `bump_type` — pass `dry_run: true` to preview the computed version and notes without tagging.

Version compute, tagging, and **AI-generated release notes** come from the shared [`ai-release.yml@v1`](https://github.com/jedwards1230/release-workflows) reusable, called once per stream with its own `tag_prefix` (`host-` / `input-`) so each next version is computed from that artifact's tag series alone. Do **not** hand-push a `host-v*` or `input-v*` tag — the reusable owns those tags now. Widget releases still get GitHub's auto-generated notes.

### Crate versions

Each binary workflow **stamps the computed tag version into its own crate's `Cargo.toml`** after checking out the tag and before `cargo build` (and fails the job if the stamp didn't land). So `CARGO_PKG_VERSION` — and with it `GET /status`, the MQTT state payloads, the HA `host_version` / `daemon_version` diagnostic entities, `build-info`, and the MCP server version — reports the **real release version**, not a manifest constant. A host release only touches `host/Cargo.toml`, a daemon release only `daemon/Cargo.toml`.

The committed `version` in `host/Cargo.toml` / `daemon/Cargo.toml` tracks the **last released tag** as a floor for local and dev builds; editing it cuts no release. `protocol` and `panel` are `0.0.0` — neither has a tag series, and nothing reports their version.

### Widget release workflow

Before pushing a `widget-<id>-v<X.Y.Z>` tag:

1. Bump `"version"` for the widget in `shell/widgets/lib/WidgetManifests.qml`.
2. Update the matching entry in `widgets-index.json` to the same version.
3. Commit both files together and merge to `main` (CI checks they stay in sync).
4. Push the tag: `git tag widget-<id>-vX.Y.Z && git push origin widget-<id>-vX.Y.Z`

`WidgetManifests.qml` is the authoring SSOT; `widgets-index.json` is kept in-sync and is checked by `scripts/check-widgets-index.py` on every PR that touches either file.
