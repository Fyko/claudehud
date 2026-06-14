# Worktree-safe git + background-agent identity — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the silent git breakage that hits claudehud inside worktrees, and render background-agent identity from the new CC payload fields (`agent`, `agent_type`, `worktree`).

**Architecture:** Two coupled fixes shipped in one branch. (1) A new `common::resolve_gitdir` helper that follows the `gitdir:` pointer when `.git` is a file, used by both the statusline slow path (`read_git_status`) and the daemon watcher (`watcher.rs`). (2) Three new optional `Input` fields plus a render-layer precedence rule that prefers `worktree.original_branch` over git introspection, and a leftmost `🤖` badge segment whenever `agent_type` is present.

**Tech Stack:** Rust (edition 2021), `serde` / `serde_json`, `memmap2`, `notify`, `crossbeam_channel`. Tests via `cargo test` with `tempfile` (already in dev-deps; if not, add it). No new runtime deps.

**Spec:** `docs/superpowers/specs/2026-06-13-worktree-and-bg-agent-support-design.md`

---

## File Structure

| File | Purpose |
|------|---------|
| `common/src/lib.rs` | Add `resolve_gitdir` helper; update `read_git_status` to use it. |
| `claudehud-daemon/src/watcher.rs` | Resolve worktree gitdir on registration; re-key `repo_cwds` from `git_root` to `gitdir`; closure does `path.parent()` once instead of twice. |
| `claudehud/src/input.rs` | Add `agent`, `agent_type`, `worktree` fields + `Agent` / `Worktree` structs + `BG_AGENT_FIXTURE` and `BG_AGENT_WORKTREE_FIXTURE` consts. |
| `claudehud/src/git.rs` | New `resolve_branch(input, cwd)` helper implementing the precedence rule (payload `worktree.original_branch` wins over `branch_and_dirty`). |
| `claudehud/src/render.rs` | New `push_agent_badge(input, &mut out)` function; called at the top of both `render_comfortable` and `render_condensed` when `agent_type` is `Some`. |
| `claudehud/src/main.rs` | Replace direct `branch_and_dirty` call with `git::resolve_branch(&input, cwd)`. |

---

## Task 1: Add `resolve_gitdir` helper in `common`

**Files:**
- Modify: `common/src/lib.rs` (insert helper after `find_git_root`, add tests)

- [ ] **Step 1: Write the failing tests**

Append to `common/src/lib.rs` inside the `#[cfg(test)] mod tests` block at the bottom:

```rust
#[test]
fn test_resolve_gitdir_regular_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let dotgit = tmp.path().join(".git");
    std::fs::create_dir(&dotgit).unwrap();
    std::fs::write(dotgit.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    let resolved = resolve_gitdir(tmp.path()).unwrap();
    assert_eq!(resolved, dotgit);
    assert!(resolved.join("HEAD").is_file());
}

#[test]
fn test_resolve_gitdir_worktree_absolute_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let real_gitdir = tmp.path().join("repo/.git/worktrees/wt-one");
    std::fs::create_dir_all(&real_gitdir).unwrap();
    std::fs::write(real_gitdir.join("HEAD"), "ref: refs/heads/feature/one\n").unwrap();

    let wt_root = tmp.path().join("wt-one");
    std::fs::create_dir(&wt_root).unwrap();
    let pointer = format!("gitdir: {}\n", real_gitdir.display());
    std::fs::write(wt_root.join(".git"), pointer).unwrap();

    let resolved = resolve_gitdir(&wt_root).unwrap();
    assert_eq!(resolved, real_gitdir);
    assert!(resolved.join("HEAD").is_file());
}

#[test]
fn test_resolve_gitdir_worktree_relative_pointer() {
    let tmp = tempfile::tempdir().unwrap();
    let real_gitdir = tmp.path().join("repo/.git/worktrees/wt-one");
    std::fs::create_dir_all(&real_gitdir).unwrap();
    std::fs::write(real_gitdir.join("HEAD"), "ref: refs/heads/feature/one\n").unwrap();

    let wt_root = tmp.path().join("wt-one");
    std::fs::create_dir(&wt_root).unwrap();
    // Relative path, must be resolved against wt_root.
    std::fs::write(wt_root.join(".git"), "gitdir: ../repo/.git/worktrees/wt-one\n").unwrap();

    let resolved = resolve_gitdir(&wt_root).unwrap();
    assert!(resolved.join("HEAD").is_file(), "resolved gitdir must contain HEAD");
}

#[test]
fn test_resolve_gitdir_missing_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_gitdir(tmp.path()).is_none());
}
```

If `tempfile` is not already in `common`'s `dev-dependencies`, add it. Check first:

```bash
grep -A5 '\[dev-dependencies\]' common/Cargo.toml
```

If missing, add: `tempfile = "3"` under `[dev-dependencies]`.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p common resolve_gitdir
```

Expected: FAIL with "cannot find function `resolve_gitdir`".

- [ ] **Step 3: Write the minimal implementation**

In `common/src/lib.rs`, insert this function immediately after `find_git_root` (around line 127):

```rust
/// Resolve the actual gitdir for a working-tree root.
///
/// For a regular repo this returns `<repo_root>/.git`. For a worktree (where
/// `.git` is a file containing `gitdir: <abs-or-rel-path>`), follows the
/// pointer to the per-worktree control directory under
/// `<mainrepo>/.git/worktrees/<name>`.
pub fn resolve_gitdir(repo_root: &Path) -> Option<PathBuf> {
    let dotgit = repo_root.join(".git");
    if dotgit.is_dir() {
        return Some(dotgit);
    }
    if dotgit.is_file() {
        let contents = std::fs::read_to_string(&dotgit).ok()?;
        for line in contents.lines() {
            if let Some(rest) = line.strip_prefix("gitdir: ") {
                let candidate = PathBuf::from(rest.trim());
                let resolved = if candidate.is_absolute() {
                    candidate
                } else {
                    repo_root.join(candidate)
                };
                if resolved.is_dir() {
                    return Some(resolved);
                }
            }
        }
    }
    None
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p common resolve_gitdir
```

Expected: 4 tests pass.

- [ ] **Step 5: Run the full `common` test suite to catch regressions**

```bash
cargo test -p common
```

Expected: all existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add common/src/lib.rs common/Cargo.toml
git commit -m "feat(common): add resolve_gitdir helper for worktree-safe gitdir lookup"
```

---

## Task 2: Use `resolve_gitdir` in `read_git_status`

**Files:**
- Modify: `common/src/lib.rs:98-114`

- [ ] **Step 1: Write the failing integration test**

Append to `common/src/lib.rs`'s `mod tests`:

```rust
#[test]
fn test_read_git_status_in_worktree_returns_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();

    // Init repo, make an initial commit on `main`, branch `feature/one`,
    // then `git worktree add ../wt-one feature/one`.
    let run = |cwd: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git failed to start");
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    };
    run(&repo, &["init", "-q", "-b", "main"]);
    run(&repo, &["config", "user.email", "t@t"]);
    run(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("a"), "hi").unwrap();
    run(&repo, &["add", "a"]);
    run(&repo, &["commit", "-q", "-m", "init"]);
    run(&repo, &["branch", "feature/one"]);

    let wt = tmp.path().join("wt-one");
    run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/one"]);

    let (branch, _dirty) = read_git_status(&wt).expect("worktree branch must resolve");
    assert_eq!(branch, "feature/one", "branch from worktree HEAD, not cwd basename");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p common test_read_git_status_in_worktree_returns_branch
```

Expected: FAIL — `read_git_status` returns `None` because `git_root.join(".git/HEAD")` doesn't resolve to a file inside the worktree.

- [ ] **Step 3: Modify `read_git_status` to use `resolve_gitdir`**

In `common/src/lib.rs`, replace lines 98-114:

```rust
pub fn read_git_status(cwd: &Path) -> Option<(String, bool)> {
    let git_root = find_git_root(cwd)?;
    let gitdir = resolve_gitdir(&git_root)?;
    let head = std::fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let branch = if let Some(b) = head.trim().strip_prefix("ref: refs/heads/") {
        b.to_owned()
    } else {
        head.trim().chars().take(7).collect()
    };
    let dirty = Command::new("git")
        .args(["--no-optional-locks", "-C"])
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    Some((branch, dirty))
}
```

(Only line 100 actually changes — the `head` read now goes through `resolve_gitdir`. The rest is unchanged.)

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p common test_read_git_status_in_worktree_returns_branch
```

Expected: PASS.

- [ ] **Step 5: Run the full `common` test suite**

```bash
cargo test -p common
```

Expected: all tests pass, including the existing `test_find_git_root_found`.

- [ ] **Step 6: Commit**

```bash
git add common/src/lib.rs
git commit -m "fix(common): read HEAD via resolve_gitdir so worktrees stop returning None"
```

---

## Task 3: Daemon watcher uses `resolve_gitdir` + re-keys `repo_cwds` by gitdir

**Files:**
- Modify: `claudehud-daemon/src/watcher.rs`

This is one task because the watch-path change and the key change must move together — keeping the old `git_root` keying with new gitdir-rooted watch events would cause cross-worktree collisions (every worktree's `path.parent().parent()` lands on `<mainrepo>/.git`).

- [ ] **Step 1: Write the failing integration test**

Append a new test module to `claudehud-daemon/src/watcher.rs` (or extend the existing `#[cfg(test)] mod tests` if one exists — check first):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use common::{hash_path, mmap_path_in, seqlock_read, MMAP_SIZE};
    use crossbeam_channel::unbounded;
    use std::time::{Duration, Instant};

    fn wait_for<F: Fn() -> bool>(timeout: Duration, f: F) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if f() { return true; }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn test_watcher_updates_cache_on_worktree_head_change() {
        // Isolate the cache dir for this test.
        let cache = tempfile::tempdir().unwrap();
        // SAFETY: this test mutates process env; the test runs serially with
        // other env-mutating tests in this crate (cargo test is per-process).
        std::env::set_var("CLAUDEHUD_CACHE_DIR", cache.path());

        // Build a real worktree to exercise the gitdir-pointer path.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let run = |cwd: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args).current_dir(cwd).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&repo, &["init", "-q", "-b", "main"]);
        run(&repo, &["config", "user.email", "t@t"]);
        run(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "hi").unwrap();
        run(&repo, &["add", "a"]);
        run(&repo, &["commit", "-q", "-m", "init"]);
        run(&repo, &["branch", "feature/one"]);
        run(&repo, &["branch", "feature/two"]);
        let wt = tmp.path().join("wt");
        run(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature/one"]);

        // Spin up the watcher with a private channel.
        let (tx, rx) = unbounded();
        let _watcher_thread = std::thread::spawn(move || start(rx));
        tx.send(wt.clone()).unwrap();

        // Wait for the initial cache write triggered by registration.
        let bin = mmap_path_in(cache.path(), hash_path(&wt));
        assert!(
            wait_for(Duration::from_secs(3), || bin.exists()),
            "watcher should write a cache bin on registration"
        );

        // Read initial branch.
        let initial = std::fs::read(&bin).unwrap();
        let mut buf = [0u8; MMAP_SIZE];
        buf.copy_from_slice(&initial[..MMAP_SIZE]);
        let (branch, _) = seqlock_read(&buf);
        assert_eq!(branch, "feature/one");

        // Mutate HEAD inside the worktree, expect the cache to refresh.
        run(&wt, &["switch", "-q", "feature/two"]);
        assert!(
            wait_for(Duration::from_secs(3), || {
                let bytes = std::fs::read(&bin).unwrap();
                let mut buf = [0u8; MMAP_SIZE];
                buf.copy_from_slice(&bytes[..MMAP_SIZE]);
                let (b, _) = seqlock_read(&buf);
                b == "feature/two"
            }),
            "watcher should fire cache::update when worktree HEAD changes"
        );

        // Drop tx so the watcher thread can exit cleanly (not strictly required
        // — the test process exits anyway — but keeps cargo test output clean).
        drop(tx);

        std::env::remove_var("CLAUDEHUD_CACHE_DIR");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p claudehud-daemon test_watcher_updates_cache_on_worktree_head_change
```

Expected: FAIL. Because task 2 has shipped, the initial cache write may succeed (registration calls `cache::update(&wt)` once and `read_git_status` now works in worktrees). But the watcher still subscribes to the wrong path (`<wt>/.git/index` doesn't exist; the real index is under the resolved gitdir), so the post-`git switch` refresh never fires. The second `wait_for` assertion is the load-bearing one for this task.

- [ ] **Step 3: Rewrite `watcher::start` to resolve gitdir and key by it**

Replace the entire body of `claudehud-daemon/src/watcher.rs` with:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

use crossbeam_channel::Receiver;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache;
use common::{find_git_root, resolve_gitdir};

/// Receive new cwd paths from the registrar, find their resolved gitdir
/// (handles worktree `.git`-as-file pointers), watch `<gitdir>/index` +
/// `<gitdir>/HEAD`, and call `cache::update` on every FS change for every
/// cwd registered against that gitdir.
pub fn start(rx: Receiver<PathBuf>) {
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<PathBuf>();

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    for path in &event.paths {
                        // path = {gitdir}/index or {gitdir}/HEAD  →  parent = gitdir
                        if let Some(gitdir) = path.parent() {
                            let _ = event_tx.send(gitdir.to_path_buf());
                        }
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create FS watcher");

    // gitdir → all registered cwds whose `.git` resolves to this gitdir.
    // For regular repos: one entry per repo, one cwd inside.
    // For worktrees: one entry per *worktree* (each worktree has its own gitdir).
    let mut cwds_by_gitdir: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();

    loop {
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                let Ok(cwd) = msg else { break };
                let Some(git_root) = find_git_root(&cwd) else { continue };
                let Some(gitdir) = resolve_gitdir(&git_root) else { continue };

                let cwds = cwds_by_gitdir.entry(gitdir.clone()).or_insert_with(|| {
                    let _ = watcher.watch(&gitdir.join("index"), RecursiveMode::NonRecursive);
                    let _ = watcher.watch(&gitdir.join("HEAD"), RecursiveMode::NonRecursive);
                    Vec::new()
                });
                if !cwds.contains(&cwd) {
                    cwds.push(cwd.clone());
                    cache::update(&cwd);
                }
            }
            recv(event_rx) -> msg => {
                let Ok(gitdir) = msg else { break };
                if let Some(cwds) = cwds_by_gitdir.get(&gitdir) {
                    for cwd in cwds {
                        cache::update(cwd);
                    }
                }
            }
        }
    }
}
```

Changes vs the previous version:
- imports `resolve_gitdir`
- map renamed to `cwds_by_gitdir`, keyed by resolved gitdir
- registration computes `gitdir` via `find_git_root` + `resolve_gitdir` (was only `find_git_root`)
- `watcher.watch(...)` targets `gitdir.join("index")` and `gitdir.join("HEAD")` (was `git_root.join(".git/index")`)
- closure does `path.parent()` once (was `.parent().and_then(|p| p.parent())`)
- event recv looks up by `gitdir` (was by `git_root`)

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p claudehud-daemon test_watcher_updates_cache_on_worktree_head_change
```

Expected: PASS. If flaky (notify backend timing), bump the `wait_for` timeouts from 3s to 5s, but do **not** weaken the assertions.

- [ ] **Step 5: Run the full daemon test suite**

```bash
cargo test -p claudehud-daemon
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add claudehud-daemon/src/watcher.rs
git commit -m "fix(daemon): watch resolved gitdir + key cwds map by gitdir for worktrees"
```

---

## Task 4: `Input` parser additions for background-agent fields

**Files:**
- Modify: `claudehud/src/input.rs`

- [ ] **Step 1: Write the failing tests**

Append to `claudehud/src/input.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
/// Anonymized capture of a real background-agent statusline payload.
/// Distinguishing keys vs `REAL_STDIN_FIXTURE`: `agent`, `agent_type`.
#[cfg(test)]
pub(crate) const BG_AGENT_FIXTURE: &str = r#"{
    "session_id": "00000000-0000-0000-0000-000000000000",
    "transcript_path": "/tmp/transcripts/bg.jsonl",
    "cwd": "/home/user/project",
    "agent": {"name": "claude"},
    "agent_type": "claude",
    "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
    "workspace": {
        "current_dir": "/home/user/project",
        "project_dir": "/home/user/project",
        "added_dirs": []
    },
    "version": "2.1.139",
    "output_style": {"name": "Gen-Z"},
    "exceeds_200k_tokens": false,
    "fast_mode": false,
    "thinking": {"enabled": true}
}"#;

/// Background-agent payload running inside a CC-native worktree. Adds the
/// top-level `worktree` block.
#[cfg(test)]
pub(crate) const BG_AGENT_WORKTREE_FIXTURE: &str = r#"{
    "session_id": "00000000-0000-0000-0000-000000000000",
    "transcript_path": "/tmp/transcripts/bg.jsonl",
    "cwd": "/home/user/.claude/worktrees/example",
    "agent": {"name": "claude"},
    "agent_type": "claude",
    "worktree": {
        "name": "example/branch-name",
        "path": "/home/user/.claude/worktrees/example",
        "branch": "worktree-example+branch-name",
        "original_cwd": "/home/user/project",
        "original_branch": "feature/example"
    },
    "model": {"id": "claude-opus-4-7", "display_name": "Opus 4.7"},
    "version": "2.1.139",
    "exceeds_200k_tokens": false,
    "fast_mode": false,
    "thinking": {"enabled": true}
}"#;

#[test]
fn test_deserialize_bg_agent_fixture() {
    let input: Input = serde_json::from_str(BG_AGENT_FIXTURE).unwrap();
    assert_eq!(input.agent_type.as_deref(), Some("claude"));
    assert_eq!(
        input.agent.as_ref().and_then(|a| a.name.as_deref()),
        Some("claude")
    );
    assert!(input.worktree.is_none(), "non-native-worktree bg payload has no worktree block");
}

#[test]
fn test_deserialize_bg_agent_worktree_fixture() {
    let input: Input = serde_json::from_str(BG_AGENT_WORKTREE_FIXTURE).unwrap();
    let wt = input.worktree.as_ref().expect("worktree block must parse");
    assert_eq!(wt.original_branch.as_deref(), Some("feature/example"));
    assert_eq!(wt.name.as_deref(), Some("example/branch-name"));
    assert_eq!(wt.original_cwd.as_deref(), Some("/home/user/project"));
    // Auto-generated branch present but ignored downstream.
    assert_eq!(wt.branch.as_deref(), Some("worktree-example+branch-name"));
}

#[test]
fn test_fg_fixture_has_no_agent_fields() {
    let input: Input = serde_json::from_str(REAL_STDIN_FIXTURE).unwrap();
    assert!(input.agent.is_none());
    assert!(input.agent_type.is_none());
    assert!(input.worktree.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud test_deserialize_bg_agent test_fg_fixture_has_no_agent_fields
```

Expected: FAIL — `Input` has no `agent` / `agent_type` / `worktree` fields, and the `Agent` / `Worktree` structs don't exist.

- [ ] **Step 3: Add the new fields and structs**

In `claudehud/src/input.rs`, add to the `Input` struct (after the existing fields, before the closing brace at line 21):

```rust
    pub agent: Option<Agent>,
    pub agent_type: Option<String>,
    pub worktree: Option<Worktree>,
```

Then add these new struct definitions anywhere after the existing `Deserialize` blocks (e.g. after `RateWindow` around line 93):

```rust
#[derive(Deserialize)]
pub struct Agent {
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct Worktree {
    pub name: Option<String>,
    pub branch: Option<String>,
    pub original_cwd: Option<String>,
    pub original_branch: Option<String>,
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p claudehud test_deserialize_bg_agent test_fg_fixture_has_no_agent_fields
```

Expected: PASS (3 tests).

- [ ] **Step 5: Run the full claudehud parser tests to confirm no regression**

```bash
cargo test -p claudehud --lib input
```

Expected: all input tests pass, including the original `test_deserialize_real_stdin_fixture` and `test_deserialize_api_billing_fixture`.

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/input.rs
git commit -m "feat(input): parse agent / agent_type / worktree background-agent fields"
```

---

## Task 5: Agent badge segment in render

**Files:**
- Modify: `claudehud/src/render.rs`

- [ ] **Step 1: Write the failing tests**

Append to `claudehud/src/render.rs` inside the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn test_render_agent_badge_present_when_agent_type_set() {
    let json = r#"{
        "cwd": "/tmp",
        "agent": {"name": "claude"},
        "agent_type": "claude",
        "model": {"display_name": "Opus 4.7"}
    }"#;
    let input: Input = serde_json::from_str(json).unwrap();
    let out = strip_ansi(&render(
        &input, None, &[], 0, None,
        RoundingMode::default(), Layout::Comfortable,
    ));
    assert!(out.starts_with("🤖"), "agent badge should be leftmost segment, got: {out:?}");
}

#[test]
fn test_render_no_agent_badge_when_agent_type_absent() {
    let input: Input = serde_json::from_str(REAL_STDIN_FIXTURE).unwrap();
    let out = strip_ansi(&render(
        &input, None, &[], 0, None,
        RoundingMode::default(), Layout::Comfortable,
    ));
    assert!(!out.contains("🤖"), "no agent badge for foreground sessions, got: {out:?}");
}

#[test]
fn test_render_agent_badge_in_condensed_layout() {
    let json = r#"{
        "cwd": "/tmp",
        "agent_type": "claude",
        "model": {"display_name": "Opus 4.7"}
    }"#;
    let input: Input = serde_json::from_str(json).unwrap();
    let out = strip_ansi(&render(
        &input, None, &[], 0, None,
        RoundingMode::default(), Layout::Condensed,
    ));
    assert!(out.starts_with("🤖"), "agent badge in condensed layout, got: {out:?}");
}
```

Note: `REAL_STDIN_FIXTURE` is `pub(crate)` in `input.rs` — it's already imported in `render.rs`'s test module (search for it). If not, add `use crate::input::REAL_STDIN_FIXTURE;` next to the other test imports.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud test_render_agent_badge test_render_no_agent_badge
```

Expected: FAIL — no `🤖` glyph appears in output.

- [ ] **Step 3: Add `push_agent_badge` and call from both layouts**

In `claudehud/src/render.rs`, add this function near the other `push_*` helpers (e.g. just before `push_model_short` around line 178):

```rust
fn push_agent_badge(input: &Input, out: &mut String) {
    if input.agent_type.is_none() {
        return;
    }
    out.push_str("🤖");
    // Future: append agent.name when it's not just "claude".
    let name = input.agent.as_ref().and_then(|a| a.name.as_deref());
    if let Some(n) = name {
        if n != "claude" {
            out.push(' ');
            out.push_str(n);
        }
    }
    out.push_str(SEP);
}
```

Then in `render_comfortable` (line 79), add as the **first** call before `push_model_full`:

```rust
fn render_comfortable(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    update_notice: Option<&str>,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Agent badge (background agents only) ───────────────
    push_agent_badge(input, &mut out);

    // ── Model ──────────────────────────────────────────────
    push_model_full(input, &mut out);
    // ... rest unchanged
```

And the same in `render_condensed` (line 129) before `push_model_short`:

```rust
fn render_condensed(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    update_notice: Option<&str>,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Agent badge (background agents only) ───────────────
    push_agent_badge(input, &mut out);

    // ── Model (short) ──────────────────────────────────────
    push_model_short(input, &mut out);
    // ... rest unchanged
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p claudehud test_render_agent_badge test_render_no_agent_badge
```

Expected: PASS (3 tests).

- [ ] **Step 5: Run full render test suite**

```bash
cargo test -p claudehud --lib render
```

Expected: all existing render tests still pass. (None of them set `agent_type`, so the new segment is dormant.)

- [ ] **Step 6: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "feat(render): show 🤖 badge as leftmost segment for background agents"
```

---

## Task 6: Branch source precedence — prefer payload `worktree.original_branch`

**Files:**
- Modify: `claudehud/src/git.rs` (new `resolve_branch` helper)
- Modify: `claudehud/src/main.rs:126-130` (call the helper)

- [ ] **Step 1: Write the failing tests**

Append to `claudehud/src/git.rs` inside the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn test_resolve_branch_prefers_worktree_original_branch() {
    let json = r#"{
        "cwd": "/nonexistent/path",
        "worktree": {"original_branch": "feature/from-payload"}
    }"#;
    let input: crate::input::Input = serde_json::from_str(json).unwrap();
    let cwd = std::path::Path::new("/nonexistent/path");
    let (branch, dirty) = resolve_branch(&input, cwd).expect("payload branch wins even without git");
    assert_eq!(branch, "feature/from-payload");
    assert!(!dirty, "no git → no dirty signal");
}

#[test]
fn test_resolve_branch_falls_back_to_git_when_no_worktree_block() {
    // Run from within the claudehud repo — git introspection succeeds.
    let cwd = std::env::current_dir().unwrap();
    let input: crate::input::Input = serde_json::from_str("{}").unwrap();
    let result = resolve_branch(&input, &cwd);
    assert!(result.is_some(), "should fall through to branch_and_dirty in a real repo");
}

#[test]
fn test_resolve_branch_payload_wins_over_real_git_branch() {
    // Even when cwd IS a real repo, payload's original_branch takes precedence.
    let cwd = std::env::current_dir().unwrap();
    let json = r#"{"worktree": {"original_branch": "from-payload"}}"#;
    let input: crate::input::Input = serde_json::from_str(json).unwrap();
    let (branch, _) = resolve_branch(&input, &cwd).unwrap();
    assert_eq!(branch, "from-payload");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p claudehud test_resolve_branch
```

Expected: FAIL — `resolve_branch` does not exist.

- [ ] **Step 3: Implement `resolve_branch` in `git.rs`**

Add to `claudehud/src/git.rs`:

```rust
use crate::input::Input;

/// Compute the branch + dirty tuple to render, honoring payload precedence.
///
/// Precedence:
///   1. `input.worktree.original_branch` — CC has already told us the branch
///      the user originated from; skip git introspection entirely.
///   2. `branch_and_dirty(cwd)` — derive from disk.
///
/// When precedence (1) fires, dirty is derived from cwd's `git status` (still
/// works in worktrees) and falls back to `false` when git is unavailable.
pub fn resolve_branch(input: &Input, cwd: &std::path::Path) -> Option<(String, bool)> {
    if let Some(branch) = input
        .worktree
        .as_ref()
        .and_then(|w| w.original_branch.as_deref())
    {
        let dirty = common::read_git_status(cwd).map(|(_, d)| d).unwrap_or(false);
        return Some((branch.to_string(), dirty));
    }
    branch_and_dirty(cwd)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p claudehud test_resolve_branch
```

Expected: PASS (3 tests). `test_resolve_branch_falls_back_to_git_when_no_worktree_block` assumes the test runs inside the claudehud git repo — that's how existing tests like `test_branch_and_dirty_in_git_repo` are structured (git.rs:55-63), so this is consistent.

- [ ] **Step 5: Wire `resolve_branch` into `main.rs`**

In `claudehud/src/main.rs`, replace lines 126-130:

```rust
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| git::resolve_branch(&input, Path::new(cwd)));
```

(Only the inner closure changes — `branch_and_dirty` → `resolve_branch(&input, …)`.)

- [ ] **Step 6: Run the full claudehud test suite**

```bash
cargo test -p claudehud
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add claudehud/src/git.rs claudehud/src/main.rs
git commit -m "feat(render): prefer payload worktree.original_branch over git introspection"
```

---

## Task 7: End-to-end sanity check

**Files:** none modified — pure verification.

- [ ] **Step 1: Build the full workspace**

```bash
cargo build --workspace --release
```

Expected: success.

- [ ] **Step 2: Run all workspace tests**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 3: Reproduce the original bug, manually**

```bash
# Set up a synthetic repo + worktree in /tmp.
cd /tmp && rm -rf clhud-smoke && mkdir clhud-smoke && cd clhud-smoke
git init -q repo && cd repo
git config user.email t@t && git config user.name t
echo hi > a && git add a && git commit -q -m init
git branch feature/one
git worktree add -q ../wt-one feature/one
cd ..

# Render from inside the worktree (cold path — daemon off, cache cleared).
pkill claudehud-daemon 2>/dev/null || true
rm -f /tmp/clhud-*.bin /tmp/clhud-watch/* 2>/dev/null
WT=$(pwd)/wt-one
echo "{\"cwd\":\"$WT\",\"model\":{\"id\":\"claude-opus-4-7\"}}" | \
    ./../../path/to/target/release/claudehud render
```

Adjust the binary path to your build (e.g. `<repo-root>/target/release/claudehud`).

Expected: output contains `feature/one` (the branch), NOT `wt-one` (the dirname). Branch segment is visible.

- [ ] **Step 4: Verify daemon updates fire on worktree HEAD change**

```bash
# Start the daemon.
./<repo-root>/target/release/claudehud-daemon &
DAEMON_PID=$!
sleep 1

# Render to register the worktree cwd.
WT=/tmp/clhud-smoke/wt-one
echo "{\"cwd\":\"$WT\"}" | ./<repo-root>/target/release/claudehud render
sleep 1

# A cache bin for this worktree must now exist.
HASH=$(python3 -c "
b = b'$WT'
h = 2166136261
for x in b:
    h ^= x; h = (h * 16777619) & 0xFFFFFFFF
print(h)
")
ls -la /tmp/clhud-$HASH.bin   # expect: 138 bytes

# Switch the worktree's HEAD, expect the daemon to refresh the cache.
cd /tmp/clhud-smoke/repo && git branch feature/two && cd /tmp/clhud-smoke/wt-one
git switch -q feature/two
sleep 2
# Re-read mmap, expect "feature/two" branch text inside.
strings /tmp/clhud-$HASH.bin | head -5   # expect: a line containing feature/two

kill $DAEMON_PID 2>/dev/null
```

Expected: post-switch, `strings` on the bin shows `feature/two`.

- [ ] **Step 5: Verify background-agent payload renders the badge**

```bash
cat <<'EOF' | ./<repo-root>/target/release/claudehud render
{
    "cwd": "/tmp",
    "agent": {"name": "claude"},
    "agent_type": "claude",
    "model": {"display_name": "Opus 4.7"},
    "worktree": {"original_branch": "feature/example"}
}
EOF
```

Expected: output starts with `🤖`, branch segment shows `feature/example` (from payload, not from `/tmp` which is not a git repo).

- [ ] **Step 6: No commit — this task is verification only**

If everything above passes, the branch is ready for PR.

---

## Self-Review Checklist (for the engineer executing this plan)

Before opening a PR:

1. **Spec coverage:** Skim `docs/superpowers/specs/2026-06-13-worktree-and-bg-agent-support-design.md`. Confirm each of: (a) `resolve_gitdir` helper exists and is used in both `read_git_status` and `watcher.rs` → tasks 1-3; (b) `agent`, `agent_type`, `worktree` parser additions → task 4; (c) agent badge segment in both layouts → task 5; (d) branch precedence rule → task 6.
2. **Tests:** `cargo test --workspace` is clean.
3. **Lint:** `cargo clippy --workspace -- -D warnings`. NOTE: `claudehud-daemon` has a pre-existing `never_loop` lint failure unrelated to this work (see project memory `cc_statusline_preexisting`). Do NOT fix it on this branch — flag it in the PR description if it surfaces.
4. **Format:** `cargo fmt --all`. Same caveat re: pre-existing drift.
5. **No new runtime deps.** `tempfile` is dev-only.
6. **Commits are bite-sized,** one per task, each green on its own.
