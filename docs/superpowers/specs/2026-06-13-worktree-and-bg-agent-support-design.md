# Worktree-safe git + background-agent identity — design

**Date:** 2026-06-13
**Status:** Approved (brainstorming complete; implementation plan TBD)

## Problem

Two related defects surface when Claude Code runs background agents in git worktrees of the same repo:

1. **Branch detection silently fails inside a worktree.** `common::read_git_status` reads `<repo>/.git/HEAD` directly. In a worktree `.git` is a *file* (containing `gitdir: <mainrepo>/.git/worktrees/<name>`), not a directory, so the read returns `None`. The render falls back to the cwd basename and drops the branch entirely — e.g. shows `wt-one` instead of `feature/one`.
2. **The daemon never refreshes the cache for worktrees.** `watcher::start` calls `watcher.watch(&git_root.join(".git/index"), …)` and `.git/HEAD`. Those paths don't exist inside a worktree (the real index/HEAD live under `<mainrepo>/.git/worktrees/<name>/`). The errors are swallowed by `let _ = …`, so the daemon silently fails to subscribe and nothing ever fires `cache::update` for worktree cwds.

Combined effect: in a worktree, `read_git_status` returns `None` → `cache::update` early-returns → no mmap bin is written → fast path keeps missing → the statusline renders with no branch and no dirty flag forever. Empirically confirmed: rendering from `wt-one`/`wt-two` produces zero `clhud-*.bin` files; only the marker file in `clhud-watch/` gets created.

Independently, Claude Code now ships **background agents** with their own identity and (sometimes) a CC-managed worktree. The statusline payload exposes this via three new keys: `agent`, `agent_type`, and (for CC-native worktrees) a `worktree` block including the user's `original_branch`. Today claudehud ignores all of it, so multiple background-agent sessions running in parallel are visually indistinguishable from each other and from foreground sessions.

## Scope

**In scope:**

1. Worktree-aware gitdir resolution in `common`, applied to both `read_git_status` (statusline slow path) and `watcher.rs` (daemon FS subscription).
2. Parser support for the new payload keys: `agent`, `agent_type`, and a subset of `worktree` (`name`, `branch`, `original_cwd`, `original_branch`).
3. Render-time changes: a leftmost agent badge segment (`🤖`) when `agent_type` is present, and a new branch-source precedence that prefers `worktree.original_branch` over git introspection.
4. Tests covering both the worktree git layout fix and the new payload parsing/rendering.

**Out of scope (deferred to a separate spec):**

- An aggregate cross-agent dashboard subcommand that lists every running agent's state.
- Parsing the `workspace.git_worktree` field.
- Special handling for `agent.name` values other than `"claude"` (today every payload reports `"claude"`).
- New CLI surface.

## Empirical findings backing the design

Captured from a real Claude Code session inside a hand-rolled worktree tree (`repo`, `wt-one` on `feature/one`, `wt-two` on `feature/two`) and from the existing `CLAUDEHUD_LOG` capture of 327 real statusline payloads (258 foreground, 84 background):

- A worktree's `.git` file points to `<mainrepo>/.git/worktrees/<name>`, which contains `HEAD`, `index`, `commondir`, `gitdir`, and worktree-scoped refs. Standard `git worktree add` semantics.
- `agent` and `agent_type` are present in 100% of background-agent payloads and 0% of foreground payloads. Clean discriminator.
- `worktree` is background-only and rare (only ~1% of background payloads — fires when a background agent runs inside a CC-native worktree under `.claude/worktrees/`).
- When `worktree` is present, the auto-generated `worktree.branch` (e.g. `"worktree-carter+ai-authorship-guardrails"`) is ugly. `worktree.original_branch` (e.g. `"carter/refactor-be--delegating-wrapper-adapter"`) is the human-meaningful branch name.
- `hash_path` is plain FNV-1a over the raw cwd bytes. Distinct cwds always produce distinct hashes — no separate hashing/collision bug.

## Design

### 1. Worktree-safe gitdir resolution (`common`)

Add a new helper:

```rust
/// Resolve the actual gitdir for a working tree root. For a regular repo this
/// returns `<repo_root>/.git`. For a worktree (where `.git` is a file
/// containing `gitdir: <abs-or-rel-path>`), follows that pointer to the real
/// per-worktree control directory under `<mainrepo>/.git/worktrees/<name>`.
pub fn resolve_gitdir(repo_root: &Path) -> Option<PathBuf>
```

Algorithm:

1. `let dotgit = repo_root.join(".git");`
2. If `dotgit.is_dir()`, return `Some(dotgit)`.
3. If `dotgit.is_file()`, read it, look for a line starting with `gitdir: `, take the remainder, trim.
4. Resolve that path: if absolute, use as-is; if relative, resolve against `repo_root`.
5. Return `Some(resolved)` if it points to an existing directory, else `None`.

Apply at two call sites:

- **`common::read_git_status`** (`common/src/lib.rs:98`): replace `std::fs::read_to_string(git_root.join(".git/HEAD"))` with `std::fs::read_to_string(resolve_gitdir(&git_root)?.join("HEAD"))`. The `git status --porcelain` half already works in worktrees (it shells `git -C <cwd>`) so no change there.
- **`claudehud-daemon::watcher`** (`watcher.rs:46-53`): two coupled changes.
  - On first registration of a cwd, compute `let gitdir = resolve_gitdir(&git_root)?;` and call `watcher.watch(&gitdir.join("index"), …)` and `.join("HEAD")` against the **resolved** gitdir, not `git_root.join(".git/…")`.
  - Re-key the `repo_cwds` map from `git_root → Vec<cwd>` to `gitdir → Vec<cwd>`, and update the event-handling closure to do `path.parent()` **once** (landing on the gitdir) instead of twice. Two reasons this is mandatory, not optional: (a) for a worktree, `path.parent().parent()` lands on `<mainrepo>/.git`, which is not the worktree's cwd-root and would collide across every worktree of the same repo; (b) keying by gitdir gives correct fan-out for free, since every cwd that shares a gitdir (regular repo OR worktree) is the set of cwds we want to refresh when that gitdir's `HEAD`/`index` changes.

### 2. `Input` parser additions (`claudehud/src/input.rs`)

```rust
pub struct Input {
    // … existing fields …
    pub agent: Option<Agent>,
    pub agent_type: Option<String>,
    pub worktree: Option<Worktree>,
}

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

All fields `Option<_>` — the existing payload-tolerant pattern. `workspace.git_worktree` and `worktree.path` are deliberately omitted; trivially addable later.

### 3. Render rules (`claudehud/src/render.rs`)

**Branch source precedence (new):**

1. If `input.worktree.as_ref().and_then(|w| w.original_branch.as_deref())` is `Some(b)`, use `b` for the branch text. This entirely bypasses git introspection on the background-agent native-worktree path — CC already told us the right answer, with no auto-generated worktree name leakage.
2. Otherwise fall through to `branch_and_dirty(cwd)` (now worktree-safe via §1).

**Agent badge segment (new):**

- Rendered when `input.agent_type.is_some()`.
- Placement: leftmost, before the model segment, so parallel agent sessions are visually distinct at a glance.
- Content: a single `🤖` glyph. If `agent.name` exists and is not `"claude"`, append it (e.g. `🤖 foo`). Today every payload reports `"claude"`, so for now this is just the glyph.

**Dirty flag:** unchanged. Still derived from `git status --porcelain` against cwd, which works in worktrees today.

## Testing

- **`common` unit:** `resolve_gitdir` against a tempdir with (a) a real `.git` directory, (b) a `.git` file containing `gitdir: <abs path>`, (c) a `.git` file with a `gitdir: <relative path>`. Each must return a path that points to a directory containing a readable `HEAD`.
- **`common` integration:** tempdir + `git init` + `git worktree add ../wt` + commit. Assert `read_git_status(&wt)` returns `Some((<branch_name>, _))` where `<branch_name>` is the actual ref, not the cwd basename, and not `None`.
- **Daemon integration:** start a watcher, register a worktree cwd via the marker pathway, mutate `HEAD` inside the resolved gitdir (e.g. `git -C wt switch`), assert the mmap bin for that cwd receives a new branch value via `seqlock_read`. Catches a regression of the silent-watcher-failure.
- **`claudehud` parser:** new `BG_AGENT_FIXTURE` constant next to `REAL_STDIN_FIXTURE` and `API_BILLING_FIXTURE`, sanitized from a real captured background payload. Test asserts `agent_type == Some("claude")`, `agent.name == Some("claude")`. Add a separate `BG_AGENT_WORKTREE_FIXTURE` variant asserting `worktree.original_branch` deserializes correctly.
- **`claudehud` render:** golden tests for two new cases — (a) background payload without `worktree` block renders the agent badge plus git-derived branch, (b) background payload with `worktree.original_branch` renders the agent badge plus that branch text and does **not** invoke git introspection. For (b), set cwd to a non-git tempdir to prove the payload branch wins.

## Rollout

- Single PR. The two fixes ship together — the agent-identity rendering is independent of the git fix, but the git fix is required regardless and there's no value in gating them separately.
- No backward-compatibility shims; all additions are tolerant of missing fields via the existing `Option<_>` pattern.
- **Windows:** the on-disk worktree layout is standardized by git itself, and `resolve_gitdir` is pure path operations + `fs::read_to_string`, so no platform branching is needed. No Windows-specific tests planned.

## Non-goals (deferred)

- Cross-agent aggregate dashboard subcommand (e.g. `claudehud agents` listing every running agent and its state).
- `workspace.git_worktree` parsing.
- Surfacing `worktree.original_cwd` or `worktree.path` anywhere in the render.
- Any change to CLI surface or environment variables.
