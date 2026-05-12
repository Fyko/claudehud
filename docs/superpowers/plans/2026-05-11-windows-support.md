# Windows Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship native Windows binaries (x64 + arm64 msvc) in releases, a PowerShell installer (`irm | iex`), Task Scheduler daemon registration, and a cross-platform cache-dir abstraction. Per-binary `.sha256` sidecars become a release artifact that both `install.sh` and `install.ps1` verify.

**Architecture:** Refactor `common/` so cache paths come from a `cache_dir()` function (Unix → `/tmp/`, Windows → `%LOCALAPPDATA%\claudehud\cache`). Add a `USERPROFILE` fallback to `claudehud install`. Extend `release.yml` with two Windows matrix entries and a sidecar-emission step. Retrofit `install.sh` with checksum verification. Add `install.ps1` symmetric to `install.sh` (Approach A from spec): downloads + verifies binaries, installs to `%LOCALAPPDATA%\Programs\claudehud`, modifies user `PATH`, registers a per-user Task Scheduler logon task, runs `claudehud install` for statusLine wiring.

**Tech Stack:** Rust 1.95 (workspace), `notify` (cross-platform fs watching, already supports ReadDirectoryChangesW on Windows), GitHub Actions (`windows-latest`, `windows-11-arm` runners), PowerShell 5.1+, Windows Task Scheduler (`Register-ScheduledTask` with inline XML).

**Spec:** `docs/superpowers/specs/2026-05-11-windows-support-design.md`

---

## File Structure

**Modify:**
- `common/src/lib.rs` — replace `WATCH_DIR` const with `watch_dir()` fn; add `cache_dir()` and `mmap_path_in()` / `watch_path_in()` helpers; rewrite existing path-format tests to use the helpers
- `common/src/incidents.rs` — replace `INCIDENTS_MMAP_PATH` const with `incidents_path()` fn
- `claudehud/src/git.rs` — update imports + callsites for the renamed symbols
- `claudehud/src/incidents.rs` — update imports + callsites
- `claudehud-daemon/src/registrar.rs` — update imports + callsites
- `claudehud-daemon/src/cache.rs` — update imports (uses `mmap_path` — unchanged signature)
- `claudehud-daemon/src/status.rs` — update imports + callsites
- `claudehud-daemon/src/main.rs` — add `windows_subsystem` cfg-gate; ensure cache + watch dirs exist on startup
- `claudehud/src/install.rs` — add `USERPROFILE` fallback when `HOME` is unset; add unit test
- `.github/workflows/release.yml` — add two Windows matrix entries; handle `.exe` suffix in stage-artifacts; emit `.sha256` sidecars
- `.github/workflows/ci.yml` — add `build-windows` job
- `install.sh` — add `verify_sha256` helper + `CLAUDEHUD_SKIP_CHECKSUM` env opt-out
- `README.md` — Windows install snippet, Task Scheduler subsection, runtime paths note, build section update

**Create:**
- `install.ps1` — PowerShell installer

---

## Task 1: Add `cache_dir()` + injectable path helpers to `common`

**Files:**
- Modify: `common/src/lib.rs:7-34, 96-146`

The strategy: keep `mmap_path(hash)` and `watch_path(hash)` exporting the same signature so consumers don't have to change for these two. Internals delegate to injectable `mmap_path_in(root, hash)` / `watch_path_in(root, hash)` helpers. Add `cache_dir()` that picks `/tmp` on Unix and `%LOCALAPPDATA%\claudehud\cache` on Windows, with a `CLAUDEHUD_CACHE_DIR` env override. The `WATCH_DIR` const becomes a `watch_dir()` function in this task — consumers will be migrated in Task 2.

- [ ] **Step 1: Rewrite path-format tests to use injectable helpers**

Replace the existing tests at `common/src/lib.rs:115-125` with these:

```rust
    #[test]
    fn test_mmap_path_in_format() {
        let p = mmap_path_in(Path::new("/tmp"), 12345);
        assert_eq!(p, Path::new("/tmp/clhud-12345.bin"));
    }

    #[test]
    fn test_watch_path_in_format() {
        let p = watch_path_in(Path::new("/tmp"), 12345);
        assert_eq!(p, Path::new("/tmp/clhud-watch/12345"));
    }

    #[test]
    fn test_cache_dir_respects_env_override() {
        // SAFETY: this test mutates process env; serial with other env-mutating tests.
        // We isolate via a tempdir-style path that won't collide with real cache.
        let key = "CLAUDEHUD_CACHE_DIR";
        let prev = std::env::var_os(key);
        std::env::set_var(key, "/tmp/claudehud-test-override");
        let got = cache_dir();
        if let Some(p) = prev {
            std::env::set_var(key, p);
        } else {
            std::env::remove_var(key);
        }
        assert_eq!(got, Path::new("/tmp/claudehud-test-override"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p common --locked`
Expected: compile errors — `mmap_path_in`, `watch_path_in`, `cache_dir` not found.

- [ ] **Step 3: Implement `cache_dir()` and the `_in` helpers**

Replace `common/src/lib.rs:7-34` (the `WATCH_DIR` const, `mmap_path`, `watch_path`) with:

```rust
pub mod incidents;

pub const MMAP_SIZE: usize = 138;
pub const BRANCH_MAX: usize = 128;

/// Runtime cache directory. Honored env override: `CLAUDEHUD_CACHE_DIR`.
/// Unix default: `/tmp`. Windows default: `%LOCALAPPDATA%\claudehud\cache`.
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
    {
        PathBuf::from("/tmp")
    }
}

/// Directory under `cache_dir()` where the daemon watches for client registration markers.
pub fn watch_dir() -> PathBuf {
    cache_dir().join("clhud-watch")
}

pub fn mmap_path(hash: u32) -> PathBuf {
    mmap_path_in(&cache_dir(), hash)
}

pub fn watch_path(hash: u32) -> PathBuf {
    watch_path_in(&cache_dir(), hash)
}

/// Test seam: build mmap path under an explicit root.
pub fn mmap_path_in(root: &Path, hash: u32) -> PathBuf {
    root.join(format!("clhud-{hash}.bin"))
}

/// Test seam: build watch marker path under an explicit root.
pub fn watch_path_in(root: &Path, hash: u32) -> PathBuf {
    root.join("clhud-watch").join(hash.to_string())
}
```

Note: the file's existing `pub mod incidents;` declaration at line 5 moves up next to the constants block. Keep `use std::path::{Path, PathBuf};` at the top of the file (already present). The `hash_path`, `seqlock_read`, `read_git_status`, `find_git_root` functions and their helper `read_u64_le` are untouched.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p common --locked`
Expected: PASS, including the three new tests. Existing tests (`test_hash_path_stable`, `test_hash_path_distinct`, `test_seqlock_read_stable`, `test_find_git_root_found`) also pass.

- [ ] **Step 5: Run workspace check to verify nothing else broke**

Run: `cargo check --workspace --locked`
Expected: error in `claudehud-daemon/src/registrar.rs` and `claudehud/src/git.rs` — unresolved import `WATCH_DIR`. That's Task 2.

- [ ] **Step 6: Commit**

```bash
git add common/src/lib.rs
git commit -m "feat(common): add cache_dir() abstraction for cross-platform paths"
```

---

## Task 2: Migrate consumers to fn-based path helpers (single atomic commit)

Tasks 1 left `WATCH_DIR` and `INCIDENTS_MMAP_PATH` consumers referencing removed constants. This task replaces the constants AND updates all consumers in one shot so the workspace compiles after every commit (bisect-friendly). Split into per-file steps for clarity, but they land in one commit.

**Files:**
- Modify: `common/src/incidents.rs:4` (replace const with fn)
- Modify: `claudehud/src/git.rs:6, 44`
- Modify: `claudehud/src/incidents.rs:4-11`
- Modify: `claudehud-daemon/src/registrar.rs:4, 12-13, 32`
- Modify: `claudehud-daemon/src/status.rs:2, 34`

- [ ] **Step 1: Replace `INCIDENTS_MMAP_PATH` const with fn in `common/src/incidents.rs`**

Remove line 4 (`pub const INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin";`). Add near the top of the file, after the existing `use` statements:

```rust
use std::path::PathBuf;
use crate::cache_dir;

/// Path to the daemon-maintained incidents mmap file.
pub fn incidents_path() -> PathBuf {
    cache_dir().join("clhud-incidents.bin")
}
```

The remaining constants (`MAX_STORED_INCIDENTS`, `TITLE_MAX`, `URL_MAX`, `INCIDENTS_MMAP_SIZE`, `SLOT_SIZE`) stay unchanged.

- [ ] **Step 2: Update `claudehud/src/git.rs` imports + callsite**

At line 6 the current import is:

```rust
use common::{
    hash_path, mmap_path, read_git_status, seqlock_read, watch_path, MMAP_SIZE, WATCH_DIR,
};
```

Change to:

```rust
use common::{
    hash_path, mmap_path, read_git_status, seqlock_read, watch_dir, watch_path, MMAP_SIZE,
};
```

At line 44 (`let _ = fs::create_dir_all(WATCH_DIR);`) change to:

```rust
    let _ = fs::create_dir_all(watch_dir());
```

- [ ] **Step 3: Update `claudehud/src/incidents.rs` imports + callsite**

Change the import at lines 4-6 from:

```rust
use common::incidents::{
    seqlock_read_incidents, Incident, INCIDENTS_MMAP_PATH, INCIDENTS_MMAP_SIZE,
};
```

To:

```rust
use common::incidents::{
    incidents_path, seqlock_read_incidents, Incident, INCIDENTS_MMAP_SIZE,
};
```

Change line 11 from:

```rust
    read_incidents_from(Path::new(INCIDENTS_MMAP_PATH))
```

To:

```rust
    read_incidents_from(&incidents_path())
```

- [ ] **Step 4: Update `claudehud-daemon/src/registrar.rs`**

Replace `use common::WATCH_DIR;` at line 4 with:

```rust
use common::watch_dir;
```

The current block at lines 12-13 is:

```rust
    let watch_dir = std::path::Path::new(WATCH_DIR);
    fs::create_dir_all(watch_dir).expect("failed to create /tmp/clhud-watch");
```

Replace with (rename the local binding to `dir` so it doesn't shadow the imported function, and make the error messages platform-neutral):

```rust
    let dir = watch_dir();
    fs::create_dir_all(&dir).expect("failed to create clhud-watch dir");
```

The subsequent uses of `watch_dir` as a value at line 31 (`watcher.watch(watch_dir, RecursiveMode::NonRecursive)`) and line 37 (`fs::read_dir(watch_dir)`) become `&dir` in both places. The `.expect("failed to watch /tmp/clhud-watch")` message at line 32 becomes `.expect("failed to watch clhud-watch dir")`.

Final state of `pub fn start(tx: Sender<PathBuf>) { ... }` (lines 11-46):

```rust
pub fn start(tx: Sender<PathBuf>) {
    let dir = watch_dir();
    fs::create_dir_all(&dir).expect("failed to create clhud-watch dir");

    let tx2 = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Create(_)) {
                    for path in &event.paths {
                        send_path_from_marker(path, &tx2);
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create notify watcher");

    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .expect("failed to watch clhud-watch dir");

    // Drain existing markers AFTER starting the watcher so no new
    // markers are missed in the window between drain and watch start.
    // Duplicates are safe — the consumer deduplicates by git root.
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            send_path_from_marker(&entry.path(), &tx);
        }
    }

    // Park this thread — `_watcher` must stay alive to keep watching.
    let _watcher = watcher;
    std::thread::park();
}
```

- [ ] **Step 5: Update `claudehud-daemon/src/status.rs`**

Open the file and find the `use common::incidents::{...}` import at line 2. Replace the symbol `INCIDENTS_MMAP_PATH` with `incidents_path` in the import set (preserve every other imported symbol).

At line 34, replace:

```rust
                        write_incidents_to_path(Path::new(INCIDENTS_MMAP_PATH), &incidents, total);
```

With:

```rust
                        write_incidents_to_path(&incidents_path(), &incidents, total);
```

- [ ] **Step 6: Verify workspace builds + all tests pass**

Run: `cargo test --workspace --locked`
Expected: all tests pass. Both `common`, `claudehud`, and `claudehud-daemon` compile cleanly.

- [ ] **Step 7: Commit (single commit covering all 5 files)**

```bash
git add common/src/incidents.rs claudehud/src/git.rs claudehud/src/incidents.rs \
        claudehud-daemon/src/registrar.rs claudehud-daemon/src/status.rs
git commit -m "refactor: migrate WATCH_DIR/INCIDENTS_MMAP_PATH consumers to fn-based helpers"
```

---

## Task 3: Ensure cache dir exists at daemon startup; add `windows_subsystem` cfg

**Files:**
- Modify: `claudehud-daemon/src/main.rs:1-9`

- [ ] **Step 1: Update `claudehud-daemon/src/main.rs`**

Replace lines 1-9 (the module declarations and use lines) with:

```rust
// claudehud-daemon/src/main.rs

// Release builds on Windows use the windows subsystem so no console window
// flashes at logon when Task Scheduler launches the daemon. Debug builds keep
// the console subsystem so developers still see stdout/stderr.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cache;
mod registrar;
mod status;
mod watcher;

use std::path::PathBuf;
use std::process::ExitCode;
```

Then in `fn main()`, right after the version/help arg handling (after line 37, before `let (tx, rx) = ...`), add:

```rust
    // Ensure cache + watch dirs exist before any thread tries to write into them.
    // On Unix these resolve to /tmp/ (already present); on Windows they live
    // under %LOCALAPPDATA%\claudehud\cache and may not exist yet.
    let _ = std::fs::create_dir_all(common::cache_dir());
    let _ = std::fs::create_dir_all(common::watch_dir());
```

- [ ] **Step 2: Build release on host platform to verify cfg compiles**

Run: `cargo build --release -p claudehud-daemon --locked`
Expected: success. The `windows_subsystem` cfg has no effect on non-Windows release builds (cfg-gated out).

- [ ] **Step 3: Run tests**

Run: `cargo test --workspace --locked`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add claudehud-daemon/src/main.rs
git commit -m "feat(daemon): create cache dirs on startup + windows subsystem cfg"
```

---

## Task 4: `USERPROFILE` fallback in `claudehud install`

**Files:**
- Modify: `claudehud/src/install.rs:56` (the inline `var_os("HOME")` call) and the test module

Goal: when running on Windows where `$HOME` is typically unset, `claudehud install` should resolve to `$USERPROFILE\.claude\settings.json`. Extract the home-dir resolution into a small helper, write tests that pin the precedence (HOME wins over USERPROFILE), then implement.

- [ ] **Step 1: Write failing tests referencing a not-yet-existing helper**

Add these two tests to the `mod tests` block in `claudehud/src/install.rs` (insert before the closing `}` of the test module):

```rust
    #[test]
    fn test_resolve_home_prefers_home_over_userprofile() {
        // Tests mutate process env and must run serially — see Step 2.
        let prev_home = std::env::var_os("HOME");
        let prev_up = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", "/tmp/home-wins");
        std::env::set_var("USERPROFILE", "/tmp/userprofile-loses");
        let got = resolve_home_dir();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_up {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        assert_eq!(got, Some(PathBuf::from("/tmp/home-wins")));
    }

    #[test]
    fn test_resolve_home_falls_back_to_userprofile() {
        let prev_home = std::env::var_os("HOME");
        let prev_up = std::env::var_os("USERPROFILE");
        std::env::remove_var("HOME");
        std::env::set_var("USERPROFILE", "/tmp/userprofile-only");
        let got = resolve_home_dir();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_up {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        assert_eq!(got, Some(PathBuf::from("/tmp/userprofile-only")));
    }
```

- [ ] **Step 2: Run tests to verify compile failure (red)**

Run: `cargo test -p claudehud --locked install -- --test-threads=1`
Expected: compile error — `cannot find function resolve_home_dir in this scope`. `--test-threads=1` is required because the tests mutate process-global env vars; without it they race.

- [ ] **Step 3: Implement `resolve_home_dir()` + update the inline callsite**

Add this free function to `claudehud/src/install.rs` (place it above the existing `resolve_settings_path` fn definition, at module scope):

```rust
/// Resolves the user's home directory. Prefers `$HOME` (set on Unix and inside
/// most Git Bash / MSYS environments); falls back to `$USERPROFILE` (the
/// standard Windows env var). Returns `None` if neither is set.
fn resolve_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
```

Then replace the inline call at line 56. The current code:

```rust
    let home = std::env::var_os("HOME").map(PathBuf::from);
```

Becomes:

```rust
    let home = resolve_home_dir();
```

- [ ] **Step 4: Run tests to verify pass (green)**

Run: `cargo test -p claudehud --locked install -- --test-threads=1`
Expected: all `install` tests pass, including the two new ones.

- [ ] **Step 5: Commit**

```bash
git add claudehud/src/install.rs
git commit -m "feat(install): fall back to USERPROFILE when HOME is unset"
```

---

## Task 5: `release.yml` — add Windows targets + checksum sidecar emission

**Files:**
- Modify: `.github/workflows/release.yml:20-30, 62-69, 81-85`

- [ ] **Step 1: Extend the matrix**

In `.github/workflows/release.yml` at lines 20-30 the current matrix is:

```yaml
      matrix:
        include:
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: x86_64-unknown-linux-musl
            os: ubuntu-latest
          - target: aarch64-unknown-linux-musl
            os: ubuntu-24.04-arm
```

Append two more entries:

```yaml
          - target: x86_64-pc-windows-msvc
            os: windows-latest
          - target: aarch64-pc-windows-msvc
            os: windows-11-arm
```

- [ ] **Step 2: Update Stage Artifacts step to handle `.exe` suffix**

At lines 62-69 (the current `Stage Artifacts` step) replace the run block with this cross-platform shell script. Note `shell: bash` is already set; bash on Windows runners is git-bash + handles `.exe` paths correctly:

```yaml
      - name: Stage Artifacts
        shell: bash
        run: |
          mkdir dist
          ext=""
          if [[ "${{ matrix.target }}" == *windows* ]]; then ext=".exe"; fi
          cp target/${{ matrix.target }}/release/claudehud${ext} \
            dist/claudehud-${{ matrix.target }}${ext}
          cp target/${{ matrix.target }}/release/claudehud-daemon${ext} \
            dist/claudehud-daemon-${{ matrix.target }}${ext}
```

- [ ] **Step 3: Add checksum emission step**

Insert this step AFTER `Stage Artifacts` and BEFORE the two `Attest ...` steps (around line 70):

```yaml
      - name: Emit Checksums (unix)
        if: runner.os != 'Windows'
        shell: bash
        run: |
          cd dist
          for f in *; do
            if command -v shasum >/dev/null 2>&1; then
              shasum -a 256 "$f" > "$f.sha256"
            else
              sha256sum "$f" > "$f.sha256"
            fi
          done

      - name: Emit Checksums (windows)
        if: runner.os == 'Windows'
        shell: pwsh
        run: |
          Get-ChildItem dist -File | Where-Object { $_.Name -notlike '*.sha256' } |
            ForEach-Object {
              $h = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower()
              "$h  $($_.Name)" | Out-File "$($_.FullName).sha256" -Encoding ascii -NoNewline
            }
```

The sidecar format matches GNU coreutils style — `<lowercase hex hash>  <basename>` on a single line, no trailing newline (the PS step uses `-NoNewline`; the unix step's `shasum`/`sha256sum` already produces a single newline-terminated line, which the bash installer parses by taking the first whitespace-delimited field anyway).

- [ ] **Step 4: Verify attestations still point at the right binary names**

At lines 71-79 (the `Attest claudehud` and `Attest claudehud-daemon` steps), the `subject-path` uses `dist/claudehud-${{ matrix.target }}` — on Windows that won't exist because the file is now `dist/claudehud-${{ matrix.target }}.exe`. Update both:

```yaml
      - name: Attest claudehud
        uses: actions/attest-build-provenance@a2bbfa25375fe432b6a289bc6b6cd05ecd0c4c32 # v4
        with:
          subject-path: dist/claudehud-${{ matrix.target }}${{ contains(matrix.target, 'windows') && '.exe' || '' }}

      - name: Attest claudehud-daemon
        uses: actions/attest-build-provenance@a2bbfa25375fe432b6a289bc6b6cd05ecd0c4c32 # v4
        with:
          subject-path: dist/claudehud-daemon-${{ matrix.target }}${{ contains(matrix.target, 'windows') && '.exe' || '' }}
```

Sidecars are intentionally NOT attested (they're public hashes — attesting them is circular and adds release-time cost for no integrity gain).

- [ ] **Step 5: Lint the workflow file**

Run: `gh workflow view release.yml 2>/dev/null` or just visually inspect `.github/workflows/release.yml` for YAML structure.

If you have `actionlint` installed locally:

```bash
actionlint .github/workflows/release.yml
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): build Windows targets and emit .sha256 sidecars"
```

---

## Task 6: `install.sh` — add SHA256 verification

**Files:**
- Modify: `install.sh:8-14, 27-29, 84-92, 232-239`

- [ ] **Step 1: Document the new env var in the header**

In the comment block at the top of `install.sh` (around line 8-14), add a new line after `CLAUDEHUD_FORCE_INSTALL=1`:

```sh
#   CLAUDEHUD_SKIP_CHECKSUM=1 Skip .sha256 sidecar verification (debug only)
```

- [ ] **Step 2: Add the SHA256 tool discovery helper**

After the `need()` helper (currently at lines 27-29) add:

```sh
# Pick a sha256 tool. macOS ships `shasum`; many Linux distros ship `sha256sum`;
# musl + busybox boxes may only have one. Prefer `shasum -a 256` because the
# stable invocation matches across both — falls back to `sha256sum`.
sha256_cmd() {
    if command -v shasum >/dev/null 2>&1; then
        echo 'shasum -a 256'
    elif command -v sha256sum >/dev/null 2>&1; then
        echo 'sha256sum'
    else
        err "no sha256 tool found (need shasum or sha256sum)"
    fi
}
```

- [ ] **Step 3: Add the `verify_sha256` helper**

After the `download()` helper (currently around lines 84-92) add:

```sh
verify_sha256() {
    file="$1"; url="$2"
    if [ -n "${CLAUDEHUD_SKIP_CHECKSUM:-}" ]; then
        printf '\033[33mwarning:\033[0m CLAUDEHUD_SKIP_CHECKSUM set, skipping verification for %s\n' "$(basename "$file")" >&2
        return 0
    fi
    sidecar="${file}.sha256"
    download "${url}.sha256" "$sidecar"
    expected="$(awk '{print $1}' "$sidecar")"
    actual="$($(sha256_cmd) "$file" | awk '{print $1}')"
    rm -f "$sidecar"
    if [ "$expected" != "$actual" ]; then
        rm -f "$file"
        err "checksum mismatch for $(basename "$file") — expected $expected, got $actual"
    fi
}
```

- [ ] **Step 4: Wire `verify_sha256` into the download loop**

In `main()`, around lines 232-239 the current download loop is:

```sh
    for bin in claudehud claudehud-daemon; do
        url="${base_url}/${bin}-${target}"
        dest="${tmpdir}/${bin}"
        say "downloading $bin..."
        download "$url" "$dest"
        chmod +x "$dest"
        mv "$dest" "${INSTALL_DIR}/${bin}"
    done
```

Replace with:

```sh
    for bin in claudehud claudehud-daemon; do
        url="${base_url}/${bin}-${target}"
        dest="${tmpdir}/${bin}"
        say "downloading $bin..."
        download "$url" "$dest"
        verify_sha256 "$dest" "$url"
        chmod +x "$dest"
        mv "$dest" "${INSTALL_DIR}/${bin}"
    done
```

- [ ] **Step 5: Smoke-test the installer locally**

This is shell — no automated test framework. The smoke test:

1. Stage a fake sidecar to verify the happy path against a known file:

```bash
echo "hello" > /tmp/cltest-bin
shasum -a 256 /tmp/cltest-bin > /tmp/cltest-bin.sha256
# Source the script's verify_sha256 fn into the current shell to test it directly:
bash -c '
. install.sh 2>/dev/null || true
# Manually re-source just the helpers (install.sh runs main "$@" at the end,
# so dot-sourcing fires the whole flow; instead, copy-paste verify_sha256 +
# sha256_cmd + err + download into a separate test harness if needed).
'
```

Pragmatic alternative: just visually inspect `install.sh` for correctness and trust the next real release to validate end-to-end (the first release with sidecars will exercise this code path).

2. Verify `sh install.sh --syntax-check` style — POSIX sh doesn't have `--syntax-check`, but you can:

```bash
sh -n install.sh
```

Expected: no output (syntax OK).

- [ ] **Step 6: Commit**

```bash
git add install.sh
git commit -m "feat(install.sh): verify .sha256 sidecars on every download"
```

---

## Task 7: Create `install.ps1`

**Files:**
- Create: `install.ps1`

- [ ] **Step 1: Write the full installer**

Create `install.ps1` with the following content (entire file):

```powershell
# claudehud installer (Windows)
# usage: irm https://raw.githubusercontent.com/fyko/claudehud/main/install.ps1 | iex
#
# Env opt-outs (all optional):
#   $env:CLAUDEHUD_VERSION         Pin a specific release tag (default: latest)
#   $env:CLAUDEHUD_INSTALL_DIR     Override install directory (default: %LOCALAPPDATA%\Programs\claudehud)
#   $env:CLAUDEHUD_FORCE_INSTALL=1 Reinstall even if the target version is already present
#   $env:CLAUDEHUD_SKIP_CONFIG=1   Skip configuration of Claude Code statusLine
#   $env:CLAUDEHUD_FORCE_CONFIG=1  Override existing statusLine configuration
#   $env:CLAUDEHUD_SKIP_PATH=1     Don't modify user PATH
#   $env:CLAUDEHUD_SKIP_DAEMON=1   Don't register the Task Scheduler entry
#   $env:CLAUDEHUD_SKIP_CHECKSUM=1 Skip .sha256 sidecar verification (debug only)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repo = 'fyko/claudehud'
$DefaultInstallDir = Join-Path $env:LOCALAPPDATA 'Programs\claudehud'
$InstallDir = if ($env:CLAUDEHUD_INSTALL_DIR) { $env:CLAUDEHUD_INSTALL_DIR } else { $DefaultInstallDir }

function Say($msg) { Write-Host "==> $msg" -ForegroundColor White }
function Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }
function Die($msg) { Write-Host "error: $msg" -ForegroundColor Red; exit 1 }

# ---------------------------------------------------------------------------
# preflight
# ---------------------------------------------------------------------------

if (-not [Environment]::Is64BitOperatingSystem) {
    Die 'claudehud requires 64-bit Windows.'
}

# ---------------------------------------------------------------------------
# detect arch
# ---------------------------------------------------------------------------

$Target = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') {
    'aarch64-pc-windows-msvc'
} else {
    'x86_64-pc-windows-msvc'
}
Say "detected target: $Target"

# ---------------------------------------------------------------------------
# resolve version
# ---------------------------------------------------------------------------

function Get-LatestTag {
    $url = "https://api.github.com/repos/$Repo/releases/latest"
    $headers = @{ 'User-Agent' = 'claudehud-installer' }
    try {
        $rel = Invoke-RestMethod -Uri $url -Headers $headers -ErrorAction Stop
        return $rel.tag_name
    } catch {
        Die "failed to fetch latest release tag: $_"
    }
}

$Tag = if ($env:CLAUDEHUD_VERSION) {
    Say "using pinned version $env:CLAUDEHUD_VERSION"
    $env:CLAUDEHUD_VERSION
} else {
    Say 'fetching latest release tag...'
    Get-LatestTag
}
$TagVer = $Tag -replace '^v', ''

# ---------------------------------------------------------------------------
# up-to-date short-circuit
# ---------------------------------------------------------------------------

$ClientExe = Join-Path $InstallDir 'claudehud.exe'
$DaemonExe = Join-Path $InstallDir 'claudehud-daemon.exe'
$SkipDownload = $false

if (-not $env:CLAUDEHUD_FORCE_INSTALL -and (Test-Path $ClientExe)) {
    try {
        $verLine = & $ClientExe --version 2>$null
        $installedVer = ($verLine -split '\s+')[1]
        if ($installedVer -eq $TagVer) {
            Say "claudehud $installedVer is already up to date"
            Say '(set $env:CLAUDEHUD_FORCE_INSTALL=1 to reinstall)'
            $SkipDownload = $true
        } else {
            Say "upgrading claudehud $installedVer -> $TagVer"
        }
    } catch {
        Say "installing claudehud $Tag"
    }
} else {
    Say "installing claudehud $Tag"
}

# ---------------------------------------------------------------------------
# download + verify
# ---------------------------------------------------------------------------

function Verify-Sha256 {
    param([string]$File, [string]$Url)
    if ($env:CLAUDEHUD_SKIP_CHECKSUM) {
        Warn "CLAUDEHUD_SKIP_CHECKSUM set, skipping verification for $(Split-Path -Leaf $File)"
        return
    }
    $sidecar = "$File.sha256"
    try {
        Invoke-WebRequest -Uri "$Url.sha256" -OutFile $sidecar -UseBasicParsing -ErrorAction Stop
    } catch {
        Remove-Item -Force -ErrorAction SilentlyContinue $File
        Die "failed to download checksum sidecar for $(Split-Path -Leaf $File): $_"
    }
    $expected = ((Get-Content $sidecar -Raw) -split '\s+')[0].ToLower()
    Remove-Item -Force $sidecar
    $actual = (Get-FileHash $File -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) {
        Remove-Item -Force $File
        Die "checksum mismatch for $(Split-Path -Leaf $File) -- expected $expected, got $actual"
    }
}

if (-not $SkipDownload) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    $tmpDir = New-Item -ItemType Directory -Force -Path (Join-Path $env:TEMP "claudehud-install-$([guid]::NewGuid())")
    try {
        $baseUrl = "https://github.com/$Repo/releases/download/$Tag"
        foreach ($name in 'claudehud', 'claudehud-daemon') {
            $artifact = "$name-$Target.exe"
            $url = "$baseUrl/$artifact"
            $dest = Join-Path $tmpDir.FullName "$name.exe"
            Say "downloading $name..."
            try {
                Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing -ErrorAction Stop
            } catch {
                Die "failed to download $name : $_"
            }
            Verify-Sha256 -File $dest -Url $url
            Move-Item -Force $dest (Join-Path $InstallDir "$name.exe")
        }
        Say "installed to $InstallDir"
    } finally {
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $tmpDir
    }
}

# ---------------------------------------------------------------------------
# PATH
# ---------------------------------------------------------------------------

$pathWasUpdated = $false
if (-not $env:CLAUDEHUD_SKIP_PATH) {
    $userPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $segments = if ($userPath) { $userPath -split ';' } else { @() }
    if ($segments -notcontains $InstallDir) {
        $newPath = if ($userPath) { "$userPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
        $env:PATH = "$env:PATH;$InstallDir"
        Say "added $InstallDir to user PATH"
        $pathWasUpdated = $true
    }
}

# ---------------------------------------------------------------------------
# statusLine config
# ---------------------------------------------------------------------------

if (-not $env:CLAUDEHUD_SKIP_CONFIG) {
    $forceArg = if ($env:CLAUDEHUD_FORCE_CONFIG) { '--force' } else { $null }
    Say 'configuring Claude Code statusLine...'
    if ($forceArg) {
        & $ClientExe install $forceArg
    } else {
        & $ClientExe install
    }
    if ($LASTEXITCODE -ne 0) {
        Warn 'claudehud install returned non-zero; statusLine may not be configured'
    }
} else {
    Say 'skipping Claude Code configuration (CLAUDEHUD_SKIP_CONFIG is set)'
}

# ---------------------------------------------------------------------------
# Task Scheduler registration
# ---------------------------------------------------------------------------

function Register-ClaudehudDaemon {
    $sid = ([System.Security.Principal.WindowsIdentity]::GetCurrent()).User.Value
    $user = $env:USERNAME
    $xml = @"
<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>$user</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>$sid</UserId>
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
      <Command>$DaemonExe</Command>
    </Exec>
  </Actions>
</Task>
"@
    try {
        Register-ScheduledTask -Xml $xml -TaskName 'claudehud-daemon' -User $user -Force | Out-Null
        Start-ScheduledTask -TaskName 'claudehud-daemon' -ErrorAction SilentlyContinue
        Say 'daemon registered + started via Task Scheduler (claudehud-daemon)'
    } catch {
        Warn "failed to register Task Scheduler entry: $_"
        Warn 'you can start the daemon manually with: & "$DaemonExe"'
    }
}

if (-not $env:CLAUDEHUD_SKIP_DAEMON) {
    Register-ClaudehudDaemon
}

# ---------------------------------------------------------------------------
# done
# ---------------------------------------------------------------------------

if ($pathWasUpdated) {
    Write-Host ''
    Write-Host 'hint: ' -ForegroundColor Yellow -NoNewline
    Write-Host 'restart your terminal so the updated PATH takes effect.'
}

Write-Host ''
Say 'done. claudehud is ready.'
```

- [ ] **Step 2: Lint the script (PSScriptAnalyzer if available)**

Run on a Windows host if available:

```powershell
Install-Module -Name PSScriptAnalyzer -Scope CurrentUser -Force  # only if not installed
Invoke-ScriptAnalyzer -Path install.ps1
```

Expected: no errors. A few warnings (e.g., `PSUseApprovedVerbs` on `Verify-Sha256` — PS would prefer `Test-` or `Confirm-`) are tolerable.

If you don't have a Windows host handy, skip this step. The script is exercised end-to-end by the manual smoke test in the release process.

- [ ] **Step 3: Commit**

```bash
git add install.ps1
git commit -m "feat: add install.ps1 PowerShell installer for Windows"
```

---

## Task 8: `ci.yml` — add Windows build job

**Files:**
- Modify: `.github/workflows/ci.yml` (append a new job)

- [ ] **Step 1: Append the build-windows job**

In `.github/workflows/ci.yml`, after the existing `test:` job (after the final `cargo test` line), add:

```yaml

  build-windows:
    name: Build (Windows)
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6

      - name: Install Rust
        uses: dtolnay/rust-toolchain@29eef336d9b2848a0b548edc03f92a220660cdb8 # stable
        with:
          toolchain: "1.95"

      - name: Cache Cargo
        uses: actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae # v5
        with:
          path: |
            ~/.cargo/registry/index
            ~/.cargo/registry/cache
            ~/.cargo/git/db
            target
          key: windows-ci-cargo-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: windows-ci-cargo-

      - name: Build
        run: cargo build --workspace --locked
```

`fmt` and `clippy` stay Ubuntu-only — they're platform-independent. The full test suite stays Ubuntu-only too; Windows is build-only for v1.

- [ ] **Step 2: Lint the workflow**

If `actionlint` is installed:

```bash
actionlint .github/workflows/ci.yml
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add Windows build job"
```

---

## Task 9: README updates

**Files:**
- Modify: `README.md:5-7, 85-95, 158-203` (approximate line ranges)

- [ ] **Step 1: Add Windows install snippet at the top**

Replace lines 5-7 of `README.md` (the existing single curl block) with:

````markdown
```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/fyko/claudehud/main/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/fyko/claudehud/main/install.ps1 | iex
```
````

- [ ] **Step 2: Add Windows note to the IPC / Architecture section**

After the "Cache file layout" table (around line 51, after the daemon-write-path numbered list), insert a short paragraph:

```markdown
On Windows, cache files live under `%LOCALAPPDATA%\claudehud\cache\` instead of `/tmp/`. The filename pattern (`clhud-{hash}.bin`) is identical.
```

- [ ] **Step 3: Update the Build section**

In the Build section (around line 85), add a sentence after `Binaries land at target/release/claudehud and target/release/claudehud-daemon.`:

```markdown
On Windows, builds use the MSVC toolchain. The daemon's `windows_subsystem = "windows"` cfg only applies in `--release` mode — debug builds keep the console window so developers can see stdout/stderr.
```

- [ ] **Step 4: Add Daemon (Windows Task Scheduler) subsection**

After the existing `### Daemon (Linux systemd)` block (around line 203), append a new subsection:

````markdown
### Daemon (Windows Task Scheduler)

`install.ps1` registers a per-user Task Scheduler entry named `claudehud-daemon` that runs at logon, hidden, with no admin required. The action invokes `%LOCALAPPDATA%\Programs\claudehud\claudehud-daemon.exe` and Task Scheduler restarts the daemon up to 3 times if it crashes.

Inspect or modify the registration:

```powershell
schtasks /Query /TN claudehud-daemon /XML
```

Uninstall:

```powershell
schtasks /Delete /TN claudehud-daemon /F
```
````

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: add Windows install, Task Scheduler, and runtime path notes"
```

---

## Task 10: Workspace-wide green check + open PR

**Files:** none modified

- [ ] **Step 1: Format check**

Run: `cargo fmt --all -- --check`
Expected: no output (exit 0). If anything formatting-drifts, run `cargo fmt --all` and amend the relevant task's commit (or land a tiny fixup commit).

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no errors, no warnings.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace --locked`
Expected: all tests pass.

- [ ] **Step 4: Push branch**

```bash
git push -u origin feat/windows-support
```

- [ ] **Step 5: Open PR**

Use `gh pr create` against `main`. Title: `feat: Windows support (binaries, install.ps1, Task Scheduler daemon)`. Body should summarize:

- Cross-platform `cache_dir()` abstraction (no behavior change for Unix users)
- `USERPROFILE` fallback in `claudehud install`
- Two new release targets: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`
- Sweeping `.sha256` sidecar verification across both installers
- New `install.ps1` (`irm | iex`) with PATH modification + Task Scheduler daemon registration
- Windows build job in CI

Test plan checklist for the PR:
- [ ] Unix install path unchanged (cache files still at `/tmp/clhud-*.bin`)
- [ ] `cargo test --workspace --locked` green on Ubuntu
- [ ] `cargo build --workspace --locked` green on `windows-latest` (new CI job)
- [ ] Manual: after a release tag is cut from main, `irm | iex` on a real Windows host succeeds, statusLine renders in Claude Code, daemon shows up under Task Scheduler

---

### Release sequencing (post-merge — not a task step)

The new `install.sh` requires `.sha256` sidecars to exist on whichever release it resolves to as "latest". After this PR merges, the very next release tag must include sidecars (which it will — the workflow change ships in the same PR). Until that tag is cut, anyone running `curl ... install.sh | sh` against `main` hits a 404 fetching the sidecar (no sidecars on prior releases). Cut a release tag immediately after merge per the project release process memory ([[project_release_process]]).

---

## Self-review notes

Coverage cross-check against `docs/superpowers/specs/2026-05-11-windows-support-design.md`:

| Spec section | Implementing task(s) |
|--------------|---------------------|
| Release workflow (matrix + sidecars) | Task 5 |
| Checksum verification (both installers) | Task 5 (sidecar emission), Task 6 (install.sh verify), Task 7 (install.ps1 verify) |
| Runtime cache directory abstraction | Tasks 1, 2 |
| Daemon log path on Windows | Deferred — spec §Daemon log path says either "write to cache_dir" or "discard" is acceptable. v1 discards; Task 3's `create_dir_all` ensures the dir exists if a future task adds explicit logging. |
| Daemon console subsystem (`windows_subsystem`) | Task 3 |
| install.ps1 flow | Task 7 |
| Task Scheduler registration | Task 7 (inline XML in install.ps1) |
| `claudehud install` USERPROFILE fallback | Task 4 |
| CI changes (build-windows job) | Task 8 |
| Test updates | Tasks 1, 4 |
| Documentation (README) | Task 9 |

Compatibility & rollout (spec §Compatibility, §Release plan): covered by the "Release sequencing" note above.
