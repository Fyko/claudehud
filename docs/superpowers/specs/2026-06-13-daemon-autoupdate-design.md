# Daemon autoupdate — design

**Status:** approved (brainstorm), pending implementation plan
**Date:** 2026-06-13

## Goal

The `claudehud-daemon` self-updates: on an interval it checks GitHub for a newer
release, and when one exists it downloads + verifies + swaps both binaries in
place, then restarts itself. The client surfaces a short-lived "updated to vX"
notice. Zero user action; opt-out + version-pin via a config file.

## Decisions (from brainstorm)

- **Daemon self-update**, not client-nudge — the daemon is the long-lived piece.
- **Native swap** in the daemon (reuse `ureq`), *not* shelling out to the install
  script — avoids the "daemon kills its own updater" double-fork dance and gets a
  uniform cross-OS path.
- **Policy:** always track latest stable; opt-out + pin persisted to a config file
  (shell env vars from install time don't reach a launchd/systemd process).
- **Visibility:** one-shot statusline notice, shown for ~5 min after an update.
- **Cadence:** check ~60s after daemon start, then every 5 min (conditional GET).
- **Scope:** unix-first (macOS + Linux). Windows is shaped-for via `#[cfg]` seams
  but **not implemented in v1**.

## Architecture

New poll thread in the daemon, sibling to `status::start()`:

```
claudehud-daemon/src/
├── main.rs        spawn update::start() alongside status::start()
├── status.rs      (existing) status.claude.com poller
└── update.rs      (new) release poller + perform_update + swap
```

Pure, shared logic moves up into `common`.

### 1. `common::version` (new)

Move the pure version helpers out of the client's `claudehud/src/update.rs` into
`common`:

- `parse_semver(&str) -> Option<(u64,u64,u64,Option<String>)>`
- `compare(installed, tag) -> VersionState`
- `enum VersionState { UpToDate, Newer(String), Ahead(String) }`
- `parse_tag(&[u8]) -> io::Result<String>` (GitHub JSON → tag)

The client keeps its curl/wget fetch; the daemon fetches with `ureq`. One semver
impl, two callers. Existing `update.rs` tests move with the code and stay green.

### 2. `common::config` (new)

Path: `${XDG_CONFIG_HOME:-$HOME/.config}/claudehud/config` (unix). Windows path
is a `#[cfg]` seam (e.g. `%APPDATA%\claudehud\config`), unimplemented in v1.

Format — hand-parsed `key=value`, no new dependency:

```
autoupdate=false
pin=v0.1.0
```

- Blank lines and `#`-prefixed comments ignored; surrounding whitespace trimmed.
- **Absent file → defaults: `autoupdate=on`, no pin.**
- `pin=vX.Y.Z` — daemon never auto-updates past this tag.
- `autoupdate=false` — daemon update thread exits immediately.

Parsed shape:

```rust
struct Config { autoupdate: bool, pin: Option<String> }
```

### 3. daemon `update.rs` — poll thread

`start()` mirrors `status::start()`:

**Guards (checked before any network activity):**
- **No-op in debug builds** (`#[cfg(debug_assertions)]` → return). Prevents a dev
  binary run from `target/` from updating itself.
- Only self-update if `current_exe()` resolves inside the install dir (defense in
  depth against clobbering a dev/checkout binary).
- Read config; if `autoupdate=false`, the thread returns.

**Loop:**
1. Sleep ~60s on first iteration, 300s thereafter.
2. Conditional GET `https://api.github.com/repos/fyko/claudehud/releases/latest`
   with stored `ETag` (304 → nothing to do), `User-Agent: claudehud-daemon/<ver>`.
3. `parse_tag` → `compare(CARGO_PKG_VERSION, tag)`. Also reject if a `pin` is set
   and `tag` is newer than the pin.
4. `VersionState::Newer(_)` and allowed → `perform_update(tag)`.

### 4. `perform_update(tag) -> Result<(), String>`

1. **Target triple** from `std::env::consts::{OS, ARCH}`:
   - `macos`+`aarch64` → `aarch64-apple-darwin`
   - `macos`+`x86_64` → `x86_64-apple-darwin`
   - `linux`+`x86_64` → `x86_64-unknown-linux-musl`
   - `linux`+`aarch64` → `aarch64-unknown-linux-musl`
   - windows arms behind `#[cfg(windows)]` → `Err("unimplemented")` in v1.
2. **Install dir** = `dirname(current_exe())`.
3. **Download both** `claudehud-{target}` and `claudehud-daemon-{target}` plus
   their `.sha256` sidecars to temp files in the cache dir (same filesystem as the
   install dir where possible, so the later rename is atomic).
4. **Verify sha256 on BOTH before swapping EITHER** (new `sha2` dep on the daemon —
   pure Rust, tiny). Mismatch → abort, delete temps.
5. **Swap:** atomic `rename` each temp over its target, `chmod 0755`. On unix,
   replacing a running binary's path is safe — the live process keeps its inode.
6. **Write the notice** (§5), then **`exit(0)`**. launchd `KeepAlive` / systemd
   `Restart=always` relaunch the *new* daemon binary. Version converges → the next
   check is `UpToDate`, so no restart loop.

Factor the pure parts (target resolution, "which files to fetch", version
decision) from the IO (download/verify/rename) so logic is unit-testable without
network or a real install.

### 5. one-shot update notice

Plain file in the cache dir, e.g. `clhud-update-notice`:

```
v0.2.0
<show_until_unix_epoch>
```

- Written by `perform_update` with `show_until = now + 300s`.
- **Client** reads it each render (cheap; absent/garbled → silently ignored, same
  posture as the git cache). If `now < show_until`, render `updated to vX.Y.Z`
  under line 1; else ignore (and best-effort unlink).
- Survives the daemon restart because it carries an absolute deadline rather than
  living in daemon memory.

### 6. service-manager restart settings (defensive)

The installers' unit/agent definitions get tightened so a one-restart-per-release
event never trips a rate limit:

- **systemd** (`install.sh` `start_daemon_linux`): add `RestartSec` and a generous
  `StartLimitIntervalSec` / `StartLimitBurst` (or `StartLimitIntervalSec=0` to
  disable the limiter) under `[Unit]`/`[Service]` so back-to-back restarts during
  an update window don't stop the unit.
- **launchd** already relaunches on any exit with `KeepAlive=true`; `ThrottleInterval`
  default (10s) is fine — note it, no change required.

## Installer changes

- `install.sh` / `install.ps1`: write the config file from
  `CLAUDEHUD_VERSION` (→ `pin=`) and a new `CLAUDEHUD_NO_AUTOUPDATE` (→
  `autoupdate=false`). Only write keys that are set; otherwise leave defaults.
- Document the new env var + config file in the installer header comments and
  README.

## Error handling

Same posture as `status.rs`:

- Network error / 304 / parse error → log `WARN`, retry next cycle.
- Checksum mismatch / partial download → log `WARN`, delete temps, **do not swap**,
  retry next cycle.
- Verify-all-before-swap keeps the version-skew window near-zero.
- Any `perform_update` error is non-fatal: log and keep polling.

## Testing

- `common::version`: move existing client tests; keep green.
- `common::config`: missing file → defaults; partial keys; comments + whitespace;
  `autoupdate` truthiness; `pin` round-trip.
- target detection: table test per `(os, arch)`, incl. the windows `unimplemented`
  arm.
- sha256 verify: known vector (match + mismatch).
- swap logic: pure decision (`should_update`, file plan) tested directly;
  integration-test the rename-into-place step against a temp dir with fake binaries
  (no network).
- notice: write → read round-trip; expired deadline ignored; garbled file ignored.

## Out of scope (v1)

- Windows implementation (seams only).
- Delta/binary-diff updates — full binary download each time.
- Rollback / A-B slots — relies on verify-before-swap + the next release to fix a
  bad push.
- In-statusline "update available, run X" nudge — superseded by auto-apply.

## Open questions

None outstanding — all brainstorm questions resolved.
