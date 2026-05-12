# Git Ahead/Behind and Operation State

## Problem

The statusline shows branch name and dirty flag, but nothing about divergence from
upstream or in-progress git operations (rebase, merge, cherry-pick, etc.). Users
have no visual cue when they're ahead/behind remote or mid-rebase inside Claude Code.

## Goal

Extend the daemon cache and client render to surface:

1. Ahead/behind commit counts relative to `@{upstream}`
2. In-progress operation state: None, Merge, Rebase (with step/total), CherryPick,
   Revert, Bisect
3. Conflict count when in a merge/rebase/cherry-pick with unmerged paths

## Non-Goals

- Fetching remotes — daemon reads refs that are already on disk after `git fetch`
- Displaying full diverge graph or branch topology
- Tracking ahead/behind without an upstream configured (graceful zero/hide)
- git2 crate — shell invocations suffice; daemon already shells for status

## Architecture

```
.git/index / .git/HEAD / .git/MERGE_HEAD / ...
              │
          notify (FS watch)
              │
claudehud-daemon  ──seqlock write──▶  /tmp/clhud-{hash}.bin  (extended layout)
                                              │
                                       seqlock read
                                              │
claudehud render  ──────────────────▶  ANSI statusline
```

The cache file gains 13 bytes appended after the existing 138-byte layout. A
layout version byte is placed at offset 138 so new readers can detect old files
and old readers simply ignore bytes past 138 (they only mmap-read 138).

## Cache Layout (new: 151 bytes)

| Offset | Size | Field |
|--------|------|-------|
| 0      | 8    | `u64` seqlock counter |
| 8      | 1    | `u8` dirty flag |
| 9      | 1    | `u8` branch name length |
| 10     | 128  | `[u8; 128]` branch name |
| 138    | 1    | `u8` layout version (`1` = this extension present) |
| 139    | 4    | `u32` ahead (LE) |
| 143    | 4    | `u32` behind (LE) |
| 147    | 1    | `u8` op_state enum |
| 148    | 1    | `u8` op_step (fits rebase 1..=255) |
| 149    | 1    | `u8` op_total (fits rebase 1..=255) |
| 150    | 1    | `u8` conflict_count (saturates at 255) |

**Total: 151 bytes.**

`op_state` values: 0=None, 1=Merge, 2=Rebase, 3=CherryPick, 4=Revert, 5=Bisect

Version byte: old daemons write 138-byte files (no version byte). The client
checks `file.len() >= 151` before reading the extension fields. When the file is
only 138 bytes, the client treats ahead/behind/op_state/conflicts as zeros/None.
Old clients reading a 151-byte file read only bytes 0–137 (branch+dirty) and work
correctly — they just ignore the extension.

## Components

### `common/src/lib.rs`

- Bump `MMAP_SIZE` from 138 to 151, add `MMAP_SIZE_V0: usize = 138`.
- Add `OpState` enum (`#[repr(u8)]`) with variants None..Bisect.
- Add `GitExtra` struct: `{ ahead: u32, behind: u32, op_state: OpState, op_step: u8, op_total: u8, conflict_count: u8 }`.
- New `seqlock_read_full(mmap: &[u8]) -> (String, bool, Option<GitExtra>)`: reads
  branch+dirty always, reads extension fields only when `mmap.len() >= 151` and
  `mmap[138] == 1`. Returns `None` for `GitExtra` on v0 files.
- Keep existing `seqlock_read` unchanged — client callers that only need branch+dirty
  can keep using it; the render call site will switch to `seqlock_read_full`.

### `claudehud-daemon/src/cache.rs`

- `update(cwd)` calls both `read_git_status` (existing) and new `read_git_extra`.
- `seqlock_write` gains signature: `fn seqlock_write(buf, branch, dirty, extra: &GitExtra)`.
- File is now sized to `MMAP_SIZE` (151).
- Extra fields written inside the seqlock window, version byte set to `1`.

### `claudehud-daemon/src/watcher.rs`

Additional watch targets per git root (registered once alongside index+HEAD):

- `.git/MERGE_HEAD` — triggers on Merge state change
- `.git/CHERRY_PICK_HEAD`
- `.git/REVERT_HEAD`
- `.git/BISECT_LOG`
- `.git/rebase-merge/` (directory, NonRecursive)
- `.git/rebase-apply/` (directory, NonRecursive)
- `.git/packed-refs` — picks up `git fetch` ref updates for ahead/behind
- `.git/refs/heads/` (NonRecursive) — local ref changes
- `.git/refs/remotes/` (NonRecursive) — remote ref changes after fetch

All events still funnel through the existing `event_tx → cache::update` path, so
debounce is handled implicitly by the existing single-threaded update loop.

### New: `claudehud-daemon/src/git_extra.rs`

`pub fn read_git_extra(git_root: &Path) -> GitExtra`

- Detects op state by checking file/dir existence in `.git/`.
- For Rebase: reads `rebase-merge/msgnum` + `rebase-merge/end` (or `rebase-apply/`
  equivalents) as `u8`; falls back to 0/0 on parse failure.
- Runs `git rev-list --count --left-right @{upstream}...HEAD` via `Command` to get
  ahead/behind; returns `(0, 0)` on any error including no upstream configured.
- Runs `git ls-files --unmerged` piped through counting only when op_state is
  Merge/Rebase/CherryPick; else conflict_count=0.
- All shell invocations use `--no-optional-locks -C {git_root}`.
- Returns default `GitExtra` (all zeros/None) silently on any failure.

### `claudehud/src/git.rs`

- `branch_and_dirty(cwd)` renamed to `branch_status(cwd) -> Option<(String, bool, Option<GitExtra>)>`.
- Fast path: open mmap, call `seqlock_read_full`. Return `GitExtra` if present.
- Slow path (no mmap): call `read_git_status` (common), return `None` for `GitExtra`.
- Old call sites in `render.rs` updated.

### `claudehud/src/render.rs`

`render()`, `render_comfortable()`, `render_condensed()` signatures gain
`git_extra: Option<&GitExtra>` parameter (or include it in the existing `git`
tuple).

**`push_dir_branch` updates:**

Comfortable:
```
claudehud (main*) ↑3 ↓1
REBASE 2/5 · 3 conflicts
```

- Ahead/behind appended after branch parens when nonzero: `↑N` in GREEN, `↓N` in RED.
- Op state badge on same line (before dir-branch segment or prepended inline):
  bright YELLOW `REBASE 2/5`, `MERGING`, `CHERRY-PICK`, `REVERTING`, `BISECTING`.
- Conflict count: DIM `· N conflicts` after the badge (omit when 0).

Condensed (space-constrained):
```
claudehud(main*) ↑3↓1 R2/5 !3
```

- `↑N↓N` tight (no space between), omit zeros individually.
- Op badge: `R N/T`, `M`, `CP`, `REV`, `BIS`.
- Conflicts: `!N` immediately after badge.

## Edge Cases

**No upstream configured / detached HEAD:**
`git rev-list --left-right @{upstream}...HEAD` exits nonzero when no upstream is
set. `read_git_extra` catches the error, returns `ahead=0, behind=0`. The render
omits ahead/behind entirely when both are zero. No special UI for "no upstream" —
zero is correct and unambiguous.

**Detached HEAD:**
Branch name already shows a short SHA (existing behavior). Ahead/behind silently
zero. Op state detection still works (rebase sets files in `.git/rebase-merge/`
even on detached HEAD).

**Stale ahead/behind (daemon lags after `git fetch`):**
Watching `.git/packed-refs` and `.git/refs/remotes/` means the daemon gets a
notify event on `git fetch`. The update triggers `git rev-list` which reads the
just-updated refs. There is a brief window (~ms) between fetch completing and the
mmap update where counts are stale; this is acceptable.

**Rebase step overflow > 255:**
Saturate to 255 in the u8 field. In practice rebases >255 commits long are
unusual and the UI shows `255/255` which is still meaningful.

**Conflict count > 255:**
Saturate to 255. `!255` in condensed mode is a rare but valid signal.

**op_step/op_total = 0 for non-Rebase states:**
For Merge/CherryPick/Revert/Bisect, step and total are meaningless. Set both to 0.
Render ignores them when op_state != Rebase.

**Daemon not running:**
Client slow path only calls `read_git_status` (branch+dirty). `GitExtra` is `None`.
Render omits ahead/behind and op state gracefully.

**Old daemon, new client:**
Client reads a 138-byte file, `seqlock_read_full` sees `len < 151`, returns
`GitExtra = None`. Render omits new fields.

**New daemon, old client:**
Old client reads bytes 0–137 (MMAP_SIZE was 138). New daemon writes 151 bytes
but old client was hardcoded to `MMAP_SIZE = 138` via the `file.len() == MMAP_SIZE`
guard in `git.rs`. To preserve this: the new `MMAP_SIZE` is 151 and `MMAP_SIZE_V0`
is 138; the client guard becomes `file.len() >= MMAP_SIZE_V0`.

## Testing

- `common`: `OpState::from_u8` roundtrip; `seqlock_read_full` on 138-byte (v0) and
  151-byte (v1) buffers; encode/decode for all `GitExtra` field combinations.
- `daemon/git_extra`: unit tests against a fake `.git/` tempdir:
  - `MERGE_HEAD` present → Merge state
  - `rebase-merge/` with `msgnum=2` + `end=5` → Rebase {step=2, total=5}
  - `rebase-apply/` variant
  - no special files → None
  - ahead/behind parsing: mock the `git` output via a stub (or parse the text
    format directly in a pure-parse unit test).
- `render`: fixture tests (each calls `strip_ansi`):
  - ahead-only (`↑3`)
  - behind-only (`↓2`)
  - both (`↑3 ↓1`)
  - zero both: neither `↑` nor `↓` appears
  - MERGING with conflicts
  - REBASE 2/5 with conflicts
  - CHERRY-PICK no conflicts
  - condensed variants of all of the above

## Migration

- Old cache files (138 bytes) are tolerated by the client indefinitely via the
  `MMAP_SIZE_V0` guard.
- No migration of existing on-disk files needed — the daemon truncates and rewrites
  the file at the new size on its first update after upgrade.
- README cache layout table updated to show 151-byte layout.

## Open Questions

None.
