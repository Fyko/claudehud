# Windows Support — Design

**Status:** Approved, pending implementation plan
**Date:** 2026-05-11

## Summary

Add first-class Windows support to `claudehud`: native binaries in releases (x64 + arm64 msvc), a PowerShell installer invoked via `irm | iex`, and Task Scheduler-based daemon registration on par with launchd / systemd on the existing platforms. As a prerequisite, every release artifact ships a per-binary SHA256 sidecar that both installers verify before placement.

## Scope

In scope:

- Release matrix gains `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`.
- `release.yml` emits `<artifact>.sha256` sidecars for every binary on every platform.
- New `install.ps1`, symmetric to `install.sh`, that downloads + verifies binaries, installs to `%LOCALAPPDATA%\Programs\claudehud\`, modifies user `PATH`, registers a Task Scheduler "at logon" task for the daemon, and runs `claudehud install` to wire the statusLine.
- `install.sh` retrofit to verify `.sha256` sidecars.
- `common/` refactor so cache files (mmap files, watch markers, incidents file) live under a platform-appropriate directory.
- Daemon binary built with `windows_subsystem = "windows"` in release mode to avoid a stray console window at logon.
- `claudehud install` subcommand reads `USERPROFILE` as a fallback when `HOME` is unset.
- README updated with Windows install snippet, daemon section, and runtime path notes.

Out of scope (deferred):

- Code signing the `.exe` artifacts. SmartScreen will warn on first run; users click through. Revisit if friction shows up.
- Full Windows test job in CI (build-only is sufficient for v1).
- `uninstall.ps1` / `uninstall.sh`. Manual uninstall steps documented in README.
- WSL-specific handling (WSL hits the Linux install path naturally).
- Unifying `install.sh` and `install.ps1` into a Rust-owned bootstrap (the "Approach B" alternative from brainstorming).

## Locked decisions

| Decision | Choice |
|----------|--------|
| Daemon registration | Per-user Task Scheduler "at logon" task, no admin required |
| Install directory | `%LOCALAPPDATA%\Programs\claudehud\` |
| Checksum verification | Sweeping prereq across both installers; per-binary `.sha256` sidecars |
| Architecture matrix | x64 (`windows-latest`) **and** arm64 (`windows-11-arm`) |
| Installer architecture | Symmetric to `install.sh` — Approach A from brainstorming |

## Architecture

### Release workflow (`.github/workflows/release.yml`)

Matrix gains two entries:

```yaml
- target: x86_64-pc-windows-msvc
  os: windows-latest
- target: aarch64-pc-windows-msvc
  os: windows-11-arm
```

Stage-artifacts step preserves the `.exe` suffix on Windows. Output filenames: `claudehud-<triple>.exe`, `claudehud-daemon-<triple>.exe`.

New step after stage-artifacts emits `.sha256` sidecars. Cfg by `runner.os`:

```yaml
- name: Emit checksums (unix)
  if: runner.os != 'Windows'
  shell: bash
  run: |
    cd dist
    for f in *; do shasum -a 256 "$f" > "$f.sha256"; done

- name: Emit checksums (windows)
  if: runner.os == 'Windows'
  shell: pwsh
  run: |
    Get-ChildItem dist -File | Where-Object { $_.Name -notlike '*.sha256' } |
      ForEach-Object {
        $h = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
        "$h  $($_.Name)" | Out-File "$($_.FullName).sha256" -Encoding ascii -NoNewline
      }
```

Existing build-provenance attestations stay on the `.exe`s only — the sidecars are public hashes, so attesting them is circular.

The final release upload step is unchanged (`files: dist/*` already picks up the sidecars).

### Checksum verification

Sidecar format: single line, `<lowercase hex hash>  <basename>` (GNU coreutils style). Both installers fetch the sidecar alongside each binary, parse the first whitespace-delimited field, and compare against `Get-FileHash` (PS) or `shasum -a 256` (sh) of the downloaded file.

On mismatch: delete the downloaded binary, print a clear error, exit 1.

Escape hatch: `CLAUDEHUD_SKIP_CHECKSUM=1` (env var) skips verification but prints a stderr warning. For debugging only, not documented in README.

`install.sh` retrofit needs a `need` check for `shasum` or `sha256sum` (one or the other) and a small helper that prefers `shasum -a 256` and falls back to `sha256sum`.

**Compatibility:** sidecars only exist on the release that first ships them and onward. The new `install.sh` and `install.ps1` are bundled in the same change, so users who reinstall pick up both at once via `git clone` / curl. Old `install.sh` checking out historical releases without sidecars is not a regression — that flow has always been a no-checksum trust-GH model.

### Runtime cache directory abstraction (`common/`)

Current hardcoded paths in `common/`:

- `WATCH_DIR: &str = "/tmp/clhud-watch"` (const)
- `INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin"` (const)
- `mmap_path(hash) -> /tmp/clhud-{hash}.bin` (fn)
- `watch_marker_path(hash) -> /tmp/clhud-watch/{hash}` (fn)

Refactor introduces a `cache_dir()` function:

```rust
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CLAUDEHUD_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Local"));
        local.join("claudehud").join("cache")
    }
    #[cfg(unix)]
    { PathBuf::from("/tmp") }
}
```

Each hardcoded constant becomes a function:

- `watch_dir() -> cache_dir().join("clhud-watch")`
- `incidents_path() -> cache_dir().join("clhud-incidents.bin")`
- `mmap_path(hash) -> cache_dir().join(format!("clhud-{hash}.bin"))`
- `watch_marker_path(hash) -> watch_dir().join(hash.to_string())`

Filename pattern (`clhud-{hash}.bin`) is unchanged. Only the parent directory moves per-platform — Unix users keep their existing cache files at `/tmp/clhud-*.bin` with zero migration.

Cache dir is created lazily via `fs::create_dir_all` on first daemon use and on first client registration write.

Test seam: an internal helper `mmap_path_in(root, hash)` and `watch_marker_path_in(root, hash)` take an explicit root. Public functions are thin wrappers over `cache_dir()`. Tests assert against the explicit-root helpers — no env-var mutation, no `serial_test` dependency.

### Daemon log path on Windows

Task Scheduler can't easily redirect stdout the way launchd plists can. The daemon, on Windows only, opens `cache_dir().join("daemon.log")` and writes its own log output there. Unix continues to rely on launchd / systemd stdout capture. Cfg-gated, small scope.

### Daemon console subsystem

```rust
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
```

Applied to `claudehud-daemon` only. Release builds run with no console window (clean at logon). Debug builds keep the console subsystem so developers still see stdout/stderr.

The client binary (`claudehud`) stays console subsystem on all platforms — it's invoked by Claude Code's statusLine, which captures stdout.

### `install.ps1` flow

Usage:

```powershell
irm https://raw.githubusercontent.com/fyko/claudehud/main/install.ps1 | iex
```

Env opt-outs. The first five mirror `install.sh` 1:1. The last three are Windows-specific (no `install.sh` equivalent — `install.sh` always modifies `PATH` hints only, has no separate daemon-skip flag, and currently does not checksum):

- `$env:CLAUDEHUD_SKIP_CONFIG` — skip statusLine config
- `$env:CLAUDEHUD_FORCE_CONFIG` — pass `--force` to `claudehud install`
- `$env:CLAUDEHUD_FORCE_INSTALL` — reinstall even if already up to date
- `$env:CLAUDEHUD_VERSION` — pin a specific release tag
- `$env:CLAUDEHUD_INSTALL_DIR` — override install directory
- `$env:CLAUDEHUD_SKIP_PATH` — don't modify user PATH (Windows-only)
- `$env:CLAUDEHUD_SKIP_DAEMON` — don't register the Task Scheduler entry (Windows-only)
- `$env:CLAUDEHUD_SKIP_CHECKSUM` — skip sidecar verification, debug only, warns to stderr (also added to `install.sh` as part of the §Checksum verification retrofit)

Steps:

1. **Preflight:** `Set-StrictMode -Version Latest`, `$ErrorActionPreference = 'Stop'`, `$ProgressPreference = 'SilentlyContinue'`. Reject non-64-bit OS.
2. **Detect arch:** `$env:PROCESSOR_ARCHITECTURE -eq 'ARM64'` → `aarch64-pc-windows-msvc`, else `x86_64-pc-windows-msvc`.
3. **Resolve version:** read `$env:CLAUDEHUD_VERSION` if set, else `Invoke-RestMethod https://api.github.com/repos/fyko/claudehud/releases/latest` → `.tag_name`. User-Agent header set to dodge anonymous GH rate limits.
4. **Up-to-date short-circuit:** if `& "$installDir\claudehud.exe" --version` matches the target tag (modulo leading `v`), skip download but still re-run statusLine config + daemon registration so first-time `SKIP_CONFIG` users can pick those up on a later run.
5. **Download + verify:** for each of `claudehud.exe`, `claudehud-daemon.exe`: `Invoke-WebRequest` to a tmp dir, fetch the `.sha256` sidecar, `Get-FileHash`, compare. On mismatch: delete, error, exit 1.
6. **Install:** `New-Item -Force` the install dir (`$env:LOCALAPPDATA\Programs\claudehud`), `Move-Item -Force` binaries in.
7. **PATH:** unless `CLAUDEHUD_SKIP_PATH`: `[Environment]::GetEnvironmentVariable('PATH','User')`, append install dir if absent, write back at `'User'` scope (no admin needed). Patch `$env:PATH` in the current session too. Do not broadcast `WM_SETTINGCHANGE` — adds Win32 interop for marginal benefit; users re-open their terminal.
8. **statusLine config:** unless `CLAUDEHUD_SKIP_CONFIG`: `& "$installDir\claudehud.exe" install $forceArg`. `$LASTEXITCODE` non-zero prints a hint but does not abort (matches `install.sh`).
9. **Daemon registration:** unless `CLAUDEHUD_SKIP_DAEMON`: register the Task Scheduler task (see below).
10. **PATH hint:** print a one-liner if install dir wasn't on PATH before this run, asking the user to restart their terminal.

Error handling: every `irm` / `iwr` in a `try { } catch { Write-Error; exit 1 }`. Tmp dir cleaned via `try / finally`.

**No `$Target` param:** the bootstrap.ps1 reference uses a positional `[stable|latest|x.y.z]` argument, but `irm | iex` doesn't pass args cleanly. Use `CLAUDEHUD_VERSION` env, symmetric with `install.sh`.

### Task Scheduler registration

PowerShell builds an inline XML document and registers via `Register-ScheduledTask -Xml $xml -TaskName 'claudehud-daemon' -User $env:USERNAME -Force`. `-Force` makes registration idempotent (overwrites existing task — equivalent to `launchctl unload && launchctl load`).

```xml
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{username}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RestartOnFailure>
      <Interval>PT1M</Interval>
      <Count>3</Count>
    </RestartOnFailure>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Hidden>true</Hidden>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{installDir}\claudehud-daemon.exe</Command>
    </Exec>
  </Actions>
</Task>
```

Key non-defaults:

- `LogonTrigger` w/ user SID — runs at user logon, no admin, equivalent to launchd `RunAtLoad` for a user agent.
- `LeastPrivilege` — no UAC prompt, runs as the user.
- `RestartOnFailure` 3x at 1-minute intervals — equivalent to launchd `KeepAlive` / systemd `Restart=always`. Less aggressive than systemd's infinite restart but plenty for a status daemon.
- `Hidden=true` — task hidden from the default Task Scheduler view.
- `ExecutionTimeLimit=PT0S` — no time limit (default is 72 hours, would kill the daemon).

The daemon binary's `windows_subsystem = "windows"` cfg-gate (see above) prevents a console window flash even on logon.

### `claudehud install` subcommand changes

Current code at `claudehud/src/install.rs:56`:

```rust
let home = std::env::var_os("HOME").map(PathBuf::from);
```

Updated:

```rust
let home = std::env::var_os("HOME")
    .or_else(|| std::env::var_os("USERPROFILE"))
    .map(PathBuf::from);
```

New unit test: `resolve_settings_path(None, None, Some(userprofile))` → `userprofile/.claude/settings.json`. Trivial.

No other changes to the subcommand — Task Scheduler registration lives in `install.ps1`, not in the Rust binary. (That would be Approach B from brainstorming; deferred.)

### CI changes (`.github/workflows/ci.yml`)

Add a `build-windows` job parallel to `test`:

```yaml
build-windows:
  name: Build (Windows)
  runs-on: windows-latest
  steps:
    - uses: actions/checkout@...
    - uses: dtolnay/rust-toolchain@...
      with:
        toolchain: "1.95"
    - uses: actions/cache@...
      with:
        path: |
          ~/.cargo/registry/index
          ~/.cargo/registry/cache
          ~/.cargo/git/db
          target
        key: windows-ci-cargo-${{ hashFiles('**/Cargo.lock') }}
    - name: Build
      run: cargo build --workspace --locked
```

`fmt` and `clippy` stay on Ubuntu only — they're platform-independent and re-running them on Windows wastes CI minutes.

The full test suite does not run on Windows in v1. Daemon mechanics are identical across platforms (notify crate handles ReadDirectoryChangesW vs. inotify/FSEvents transparently). A Windows test job is the kind of thing to add when the first Windows-specific regression bites.

### Test updates

1. **`common/src/lib.rs:118,124`** — currently asserts string-literal paths under `/tmp/`. Rewrite to use the new injectable-root helpers `mmap_path_in(Path::new("/tmp"), 12345)` so the test runs identically on every platform without env-var mutation.
2. **`claudehud/src/install.rs`** — add a unit test for the `USERPROFILE` fallback in `resolve_settings_path`.
3. **`claudehud/src/git.rs:67`** — leaves `Path::new("/tmp")` literal; the function walks up looking for `.git` and returns `None` on miss, so the literal path is irrelevant to test correctness. No change needed.

### Documentation (`README.md`)

1. **Install snippet** at top of README — add a PowerShell block alongside the existing curl block:

   ```bash
   # macOS / Linux
   curl -fsSL https://raw.githubusercontent.com/fyko/claudehud/main/install.sh | sh
   ```

   ```powershell
   # Windows
   irm https://raw.githubusercontent.com/fyko/claudehud/main/install.ps1 | iex
   ```

2. **New `### Daemon (Windows Task Scheduler)` subsection** after the systemd block. Mirrors the existing launchd/systemd structure: explains what the installer registers, links to manual uninstall via `schtasks /Delete /TN claudehud-daemon /F`.

3. **Runtime paths note** in the IPC / Architecture section — short paragraph: "On Windows, cache files live under `%LOCALAPPDATA%\claudehud\cache\` instead of `/tmp/`. Filename pattern (`clhud-{hash}.bin`) is identical."

4. **Build section** — note that Windows builds use the MSVC toolchain and that `--release` is required for the daemon's `windows_subsystem = "windows"` cfg to apply.

5. **Dependencies table** — no changes; all current crates work on Windows. No new deps introduced.

No standalone `docs/windows.md` — fragmenting the install story across files is worse than one slightly longer README section.

## Compatibility

- **Unix users:** zero-impact. Cache files stay at `/tmp/clhud-*.bin`. `install.sh` gains checksum verification (additive, sidecars co-publish in the same release).
- **First Windows release:** all-new code path. Users hit `irm | iex`, get a working setup with no pre-existing migration concerns.
- **Existing `install.sh` users mid-upgrade:** they fetch the new `install.sh` from `main` (curl pipe), which expects sidecars to exist. Sidecars start existing on the release that ships this PR. Cutover is atomic.

## Release plan (post-implementation)

Per [[project_release_process]]:

1. Bump `Cargo.toml` workspace.version.
2. Verify CI green on the merge commit (including the new `build-windows` job).
3. Push `vX.Y.Z` tag. `release.yml` builds all 6 targets (4 unix + 2 windows), emits sidecars, attests, publishes.
4. Manually smoke-test `irm | iex` on a real Windows host before announcing.

A "Windows smoke test" checklist should be added to the release process memory after the first successful Windows release.

## Unresolved questions

- None at design time. All decisions locked above.
