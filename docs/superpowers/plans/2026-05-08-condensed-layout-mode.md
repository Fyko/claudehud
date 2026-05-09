# Condensed Layout Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `CLAUDEHUD_LAYOUT=condensed` env-driven render mode that collapses the HUD to 1 line idle / 2 lines with active incident. Comfortable stays the default. Session duration is removed from both layouts.

**Architecture:** Introduce a `Layout` enum (`Comfortable | Condensed`) parsed in `main.rs` from `CLAUDEHUD_LAYOUT`. `render::render` becomes a thin dispatcher that delegates to `render_comfortable` or `render_condensed`. Row-building primitives (`push_model_*`, `push_context`, `push_dir_branch`, `push_incidents`, `push_rate_row`, `push_rate_inline`) are extracted as private helpers shared between the two paths so they can't drift.

**Tech Stack:** Rust 1.95, `serde_json`, `pico_args`, `time`. Workspace-level `cargo test --workspace`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings` (CI commands at `.github/workflows/`).

**Spec:** `docs/superpowers/specs/2026-05-08-condensed-mode-design.md`

---

## File Structure

| Path | Action | Responsibility |
|---|---|---|
| `claudehud/src/render.rs` | Modify | Add `Layout` enum + parse. Split `render` into dispatcher + `render_comfortable` + `render_condensed`. Extract row helpers. Drop session-duration block. Remove unused `session_duration` helper. |
| `claudehud/src/main.rs` | Modify | Read `CLAUDEHUD_LAYOUT`, parse via `Layout::parse`, pass into `render`. |
| `claudehud/src/time.rs` | No change | `format_duration` still used by incident rendering. `session_duration` lived in `render.rs` (not `time.rs`) and goes away there. |
| `claudehud/src/fmt.rs` | No change | `build_bar(pct, width, out)` already accepts a width parameter. Reused with `width=4` for condensed mini-bars. |
| `README.md` | Modify | Document `CLAUDEHUD_LAYOUT` env var. |

---

## Task 1: Layout enum + parse

**Files:**
- Modify: `claudehud/src/render.rs` (add type, no behavior change yet)

- [ ] **Step 1.1: Write the failing test**

Add to the bottom of the `tests` module in `claudehud/src/render.rs` (right before the closing `}`):

```rust
    #[test]
    fn test_layout_parse() {
        assert_eq!(Layout::parse("comfortable"), Some(Layout::Comfortable));
        assert_eq!(Layout::parse("COMFORTABLE"), Some(Layout::Comfortable));
        assert_eq!(Layout::parse("condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse("Condensed"), Some(Layout::Condensed));
        assert_eq!(Layout::parse(""), None);
        assert_eq!(Layout::parse("compact"), None);
        assert_eq!(Layout::parse("garbage"), None);
    }

    #[test]
    fn test_layout_default_is_comfortable() {
        assert_eq!(Layout::default(), Layout::Comfortable);
    }
```

- [ ] **Step 1.2: Run tests to verify they fail**

Run: `cargo test --package claudehud render::tests::test_layout`
Expected: FAIL — `cannot find type Layout in this scope` / `cannot find function or constant ...::parse`

- [ ] **Step 1.3: Add the enum + parse impl**

Insert below the `RoundingMode` block (around `claudehud/src/render.rs:38`, after the closing `}` of `impl RoundingMode`):

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    #[default]
    Comfortable,
    Condensed,
}

impl Layout {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "comfortable" => Some(Self::Comfortable),
            "condensed" => Some(Self::Condensed),
            _ => None,
        }
    }
}
```

- [ ] **Step 1.4: Run tests to verify they pass**

Run: `cargo test --package claudehud render::tests::test_layout`
Expected: PASS (2 tests)

Also run the full workspace to confirm nothing regressed:
Run: `cargo test --workspace --locked`
Expected: PASS (all existing tests still green)

- [ ] **Step 1.5: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean

- [ ] **Step 1.6: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "feat(claudehud): add Layout enum w/ parse"
```

---

## Task 2: Drop session_duration from comfortable

This is a behavior change visible to existing users. We do it in its own commit so it's easy to revert if anyone misses the stopwatch. After this task, comfortable mode no longer renders `⏱ Xh Ym`.

**Files:**
- Modify: `claudehud/src/render.rs` (remove session-duration block in `render`, remove the now-unused `session_duration` helper, update existing tests if they assert on duration)

- [ ] **Step 2.1: Search for any tests that assert duration is present**

Run: `rg -n "session_duration|⏱|2h16m|2h3m" claudehud/src/`
Expected: matches in `render.rs` only (the rendering block, the helper, and any test references). Note line numbers — you'll touch them next.

- [ ] **Step 2.2: Remove the session-duration block from `render`**

In `claudehud/src/render.rs`, delete lines `90-99` (the entire `// ── Session duration ──` block):

```rust
    // ── Session duration ───────────────────────────────────
    if let Some(dur) = session_duration(input) {
        out.push_str(SEP);
        out.push_str(DIM);
        out.push_str("⏱ ");
        out.push_str(RESET);
        out.push_str(WHITE);
        out.push_str(&dur);
        out.push_str(RESET);
    }
```

Replace with: nothing — the next block (`// ── Incident lines ──`) follows directly.

- [ ] **Step 2.3: Remove the unused `session_duration` helper**

In `claudehud/src/render.rs`, delete lines `204-209`:

```rust
fn session_duration(input: &Input) -> Option<String> {
    let start = input.session.as_ref()?.start_time.as_deref()?;
    let start_epoch = parse_iso8601(start)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format_duration(now.saturating_sub(start_epoch)))
}
```

- [ ] **Step 2.4: Audit imports in render.rs**

After removing the helper, these imports may go unused:
- `use std::time::{SystemTime, UNIX_EPOCH};` — STILL USED by `push_incident_line` at the bottom of the file. Keep.
- `use crate::time::{format_duration, format_reset_time, parse_iso8601, ResetStyle};` — `parse_iso8601` is no longer used. Trim it.

Update the import line at `claudehud/src/render.rs:9`:

Before:
```rust
use crate::time::{format_duration, format_reset_time, parse_iso8601, ResetStyle};
```

After:
```rust
use crate::time::{format_duration, format_reset_time, ResetStyle};
```

- [ ] **Step 2.5: Add a regression test asserting no stopwatch in default render**

Append to the `tests` module in `claudehud/src/render.rs`:

```rust
    #[test]
    fn test_render_no_session_duration() {
        // Build an Input with a session.start_time that would have produced "Xh Ym".
        let json = r#"{
            "session": {"start_time": "2024-01-15T10:30:00Z"}
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor));
        assert!(!plain.contains('⏱'), "stopwatch glyph should be gone");
        // duration strings would look like "Xh Ym" / "Xm" / "Xs" — ensure none of those patterns appear after the model name.
        // A simpler check: the render contains exactly one chunk separated by no SEP after the model.
        assert!(!plain.contains("h0m"), "should not render hour-minute duration");
    }
```

- [ ] **Step 2.6: Run tests**

Run: `cargo test --package claudehud --locked`
Expected: PASS. The new `test_render_no_session_duration` passes; all existing tests (which never asserted duration was present) remain green. Note: `test_render_real_stdin_fixture` does NOT assert on duration, so it stays unchanged.

- [ ] **Step 2.7: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean (the unused-import trim should already have you clippy-clean).

- [ ] **Step 2.8: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "refactor(claudehud): drop session duration from render

The stopwatch (⏱ 2h16m) added a chunk to row 1 that the user reaches
for rarely. Removing it keeps row 1 tighter and is a prerequisite for
the upcoming condensed layout mode where row 1 must absorb both
rate-limit chunks."
```

---

## Task 3: Extract row helpers (refactor only)

Pull the inline blocks inside `render()` into private helpers. No behavior change — every existing test stays green.

**Files:**
- Modify: `claudehud/src/render.rs`

- [ ] **Step 3.1: Snapshot current render output before refactor**

Run: `cargo test --package claudehud --locked`
Expected: PASS — capture the current passing baseline. Don't proceed if anything is red.

- [ ] **Step 3.2: Extract `push_model_full`**

Add this private helper above `fn push_incident_line` (around `claudehud/src/render.rs:138`):

```rust
fn push_model_full(input: &Input, out: &mut String) {
    let model = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    out.push_str(BLUE);
    out.push_str(model);
    out.push_str(RESET);
}
```

In `render()`, replace lines 49-56 (the model block) with:

```rust
    // ── Model ──────────────────────────────────────────────
    push_model_full(input, &mut out);
```

- [ ] **Step 3.3: Extract `push_context`**

Add helper:

```rust
fn push_context(input: &Input, rounding: RoundingMode, out: &mut String) {
    let pct = context_pct(input, rounding);
    out.push_str("✍️ ");
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);
}
```

In `render()`, replace the context block (the lines after `out.push_str(SEP);` that compute and emit `pct`) with:

```rust
    out.push_str(SEP);
    push_context(input, rounding, &mut out);
```

- [ ] **Step 3.4: Extract `push_dir_branch` (with `tight` parameter)**

Add helper:

```rust
fn push_dir_branch(input: &Input, git: Option<&(String, bool)>, tight: bool, out: &mut String) {
    let cwd = input.cwd.as_deref().unwrap_or("");
    let dirname = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd);
    out.push_str(CYAN);
    out.push_str(dirname);
    out.push_str(RESET);
    if let Some((branch, dirty)) = git {
        if !tight {
            out.push(' ');
        }
        out.push_str(GREEN);
        out.push('(');
        out.push_str(branch);
        if *dirty {
            out.push_str(RED);
            out.push('*');
        }
        out.push_str(GREEN);
        out.push(')');
        out.push_str(RESET);
    }
}
```

In `render()`, replace the dir+git block with:

```rust
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), false, &mut out);
```

Note: `git` was `Option<(String, bool)>` (owned). The helper now takes `Option<&(String, bool)>` to avoid moving. Adjust render's signature: `git: Option<(String, bool)>` stays the same — just use `git.as_ref()` at the call site.

- [ ] **Step 3.5: Extract `push_incidents`**

Add helper:

```rust
fn push_incidents(incidents: &[Incident], total_active: u8, out: &mut String) {
    for inc in incidents {
        out.push('\n');
        push_incident_line(inc, out);
    }
    let overflow = total_active.saturating_sub(incidents.len() as u8);
    if overflow > 0 {
        out.push('\n');
        write!(out, "\x1b]8;;https://status.claude.com/\x1b\\").unwrap();
        out.push_str(DIM);
        write!(out, "+{overflow} more").unwrap();
        out.push_str(RESET);
        out.push_str("\x1b]8;;\x1b\\");
    }
}
```

In `render()`, replace the incident block (`for inc in incidents { ... }` and the `let overflow = ...` block) with:

```rust
    push_incidents(incidents, total_active, &mut out);
```

- [ ] **Step 3.6: Run tests**

Run: `cargo test --package claudehud --locked`
Expected: PASS — every existing test stays green. If any test fails, your extraction lost or duplicated a byte. Diff `cargo test -- --nocapture` output against the previous run.

- [ ] **Step 3.7: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 3.8: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "refactor(claudehud): extract render row helpers

Pull the model/context/dir-branch/incidents blocks out of render() into
private helpers. No behavior change — prep for the condensed layout
which will compose the same primitives in a different order."
```

---

## Task 4: Thread `Layout` param into `render()` (dispatcher only)

Add the `layout` parameter, route Comfortable to a dedicated `render_comfortable`. `render_condensed` is added as a stub that returns `String::new()` — implementation lands in Task 5. All existing tests must be updated to pass `Layout::Comfortable`.

**Files:**
- Modify: `claudehud/src/render.rs`
- Modify: `claudehud/src/main.rs`

- [ ] **Step 4.1: Rename current `render` body to `render_comfortable`**

Change the function signature and body in `claudehud/src/render.rs`:

Before:
```rust
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);
    // ... existing body ...
    out
}
```

After (note: this is just a rename — no logic change):
```rust
fn render_comfortable(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);
    // ... existing body ...
    out
}
```

- [ ] **Step 4.2: Add `render_condensed` stub**

Add directly below `render_comfortable`:

```rust
fn render_condensed(
    _input: &Input,
    _git: Option<(String, bool)>,
    _incidents: &[Incident],
    _total_active: u8,
    _rounding: RoundingMode,
) -> String {
    // Implemented in Task 5.
    String::new()
}
```

- [ ] **Step 4.3: Add new `render` dispatcher**

Add this above `render_comfortable`:

```rust
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    layout: Layout,
) -> String {
    match layout {
        Layout::Comfortable => render_comfortable(input, git, incidents, total_active, rounding),
        Layout::Condensed => render_condensed(input, git, incidents, total_active, rounding),
    }
}
```

- [ ] **Step 4.4: Update every call site in tests**

In `claudehud/src/render.rs`'s `tests` module, every existing call to `render(...)` currently has 5 args. Add `Layout::Comfortable` as the 6th. Use `replace_all` only if you can verify each call site visually — there are ~13 calls.

Search: `rg -n "render\(&?input" claudehud/src/render.rs`

Each match like:
```rust
let result = render(&input, None, &[], 0, RoundingMode::Floor);
```

Becomes:
```rust
let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable);
```

Same treatment for the multi-line variants, e.g.:
```rust
let plain = strip_ansi(&render(
    &input,
    Some(("main".to_string(), false)),
    &[],
    0,
    RoundingMode::Floor,
    Layout::Comfortable,
));
```

- [ ] **Step 4.5: Update the production call site in `main.rs`**

In `claudehud/src/main.rs:79`:

Before:
```rust
print!(
    "{}",
    render::render(&input, git, &incidents, total_active, rounding)
);
```

After:
```rust
print!(
    "{}",
    render::render(&input, git, &incidents, total_active, rounding, render::Layout::default())
);
```

(Real env-var wiring lands in Task 6 — for now, the production binary always uses `Comfortable`.)

- [ ] **Step 4.6: Run tests**

Run: `cargo test --workspace --locked`
Expected: PASS — every existing test stays green. The dispatcher is a no-op for Comfortable.

- [ ] **Step 4.7: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean. The `_input` etc. underscored params in `render_condensed` suppress unused-arg warnings.

- [ ] **Step 4.8: Commit**

```bash
git add claudehud/src/render.rs claudehud/src/main.rs
git commit -m "refactor(claudehud): split render into Layout dispatcher

render() now dispatches to render_comfortable or render_condensed via
the new Layout enum. Comfortable is the only implemented path; condensed
is a stub. No behavior change for users."
```

---

## Task 5: Implement `render_condensed`

TDD: write tests for each piece of the condensed layout, then implement helpers + the body.

**Files:**
- Modify: `claudehud/src/render.rs`

- [ ] **Step 5.1: Write the failing test for default model on condensed**

Append to the `tests` module:

```rust
    #[test]
    fn test_render_default_model_condensed() {
        let input = Input::default();
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Claude"), "default model name should render");
    }
```

- [ ] **Step 5.2: Write the failing test for short model name (parenthetical stripped)**

```rust
    #[test]
    fn test_render_model_name_condensed_strips_paren() {
        let json = r#"{"model": {"display_name": "Opus 4.7 (1M context)"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed));
        assert!(plain.contains("Opus 4.7"), "short model name should render");
        assert!(!plain.contains("(1M context)"), "parenthetical suffix should be stripped");
    }
```

- [ ] **Step 5.3: Write the failing test for tight dir/branch spacing**

```rust
    #[test]
    fn test_render_dir_branch_condensed_tight() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(
            &input,
            Some(("main".to_string(), false)),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        ));
        assert!(plain.contains("myproject(main)"), "dir and branch should be tight (no space)");
        assert!(!plain.contains("myproject (main)"), "comfortable spacing should not appear");
    }
```

- [ ] **Step 5.4: Write the failing test for inline rate limits**

```rust
    #[test]
    fn test_render_rate_limits_condensed_inline() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);

        // condensed labels are "5h" and "7d" — not "current"/"weekly"
        assert!(plain.contains("5h"), "5h label should render");
        assert!(plain.contains("7d"), "7d label should render");
        assert!(!plain.contains("current"), "comfortable label should not appear");
        assert!(!plain.contains("weekly"), "comfortable label should not appear");

        // pct numerals
        assert!(plain.contains("9%"), "5h pct should render");
        assert!(plain.contains("12%"), "7d pct should render");

        // mini-bar dots present (4 dots per bar = 8 total when both rate windows shown,
        // plus `○` may also appear in build_bar fallback paths — check ≥ 8)
        let dots = plain.matches('○').count() + plain.matches('●').count();
        assert!(dots >= 8, "expected ≥8 bar dots inline (got {dots})");

        // condensed idle: zero \n
        assert!(!result.contains('\n'), "condensed idle output should be single-line");
    }
```

- [ ] **Step 5.5: Write the failing test for incident on condensed**

```rust
    #[test]
    fn test_render_incident_condensed_keeps_own_line() {
        use common::incidents::{Incident, Severity};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let incident = Incident {
            severity: Severity::Major,
            started_at: now.saturating_sub(12 * 60),
            title: "Elevated API errors".to_string(),
            url: "https://status.claude.com/incidents/abc".to_string(),
        };
        let out = render(
            &Input::default(),
            None,
            &[incident],
            1,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("Elevated API errors"));
        assert!(plain.contains("started 12m ago"));
        // exactly one newline — condensed row 1, then incident row
        assert_eq!(out.matches('\n').count(), 1, "exactly one newline expected");
        assert!(out.contains("\x1b]8;;https://status.claude.com/incidents/abc"));
    }
```

- [ ] **Step 5.6: Write the failing test for context % on condensed**

```rust
    #[test]
    fn test_render_context_pct_condensed() {
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {"input_tokens": 100000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed));
        assert!(plain.contains("50%"));
    }
```

- [ ] **Step 5.7: Write the failing test for missing rate limits (chunk omitted)**

```rust
    #[test]
    fn test_render_condensed_no_rate_limits() {
        // No rate_limits field → no inline rate chunks, but row 1 still renders.
        let input = Input::default();
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Claude"));
        assert!(!plain.contains("5h"));
        assert!(!plain.contains("7d"));
        assert!(!result.contains('\n'));
    }

    #[test]
    fn test_render_condensed_only_5h() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 9.0, "resets_at": 1705316400}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&result);
        assert!(plain.contains("5h"));
        assert!(plain.contains("9%"));
        assert!(!plain.contains("7d"));
    }
```

- [ ] **Step 5.7b: Write the failing test for git dirty marker**

```rust
    #[test]
    fn test_render_git_dirty_condensed() {
        let input = Input::default();
        let out = render(
            &input,
            Some(("main".to_string(), true)),
            &[],
            0,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("(main*)"), "dirty marker should appear inside paren");
    }
```

- [ ] **Step 5.7c: Write the failing test for `+N more` overflow on condensed**

```rust
    #[test]
    fn test_render_incident_plus_n_more_condensed() {
        use common::incidents::{Incident, Severity};
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let incident = Incident {
            severity: Severity::Minor,
            started_at: now,
            title: "Thing A".to_string(),
            url: "https://status.claude.com/incidents/a".to_string(),
        };
        // 1 stored, total=3 → "+2 more"
        let out = render(
            &Input::default(),
            None,
            &[incident],
            3,
            RoundingMode::Floor,
            Layout::Condensed,
        );
        let plain = strip_ansi(&out);
        assert!(plain.contains("+2 more"));
        // condensed row + incident row + overflow row = 2 newlines
        assert_eq!(out.matches('\n').count(), 2);
    }
```

- [ ] **Step 5.7d: Write the failing test for the real stdin fixture on condensed**

```rust
    #[test]
    fn test_render_real_stdin_fixture_condensed() {
        let input: Input = serde_json::from_str(crate::input::REAL_STDIN_FIXTURE).unwrap();
        let out = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed);
        let plain = strip_ansi(&out);
        assert!(plain.contains("Opus 4.7"), "model name should render");
        assert!(plain.contains("22%"), "server-provided used_percentage wins");
        assert!(plain.contains("project"), "cwd dirname should render");
        assert!(plain.contains("5h"), "5h rate label should render");
        assert!(plain.contains("7d"), "7d rate label should render");
        assert!(!plain.contains("current"), "comfortable label should not appear");
        assert!(!plain.contains("weekly"), "comfortable label should not appear");
        assert!(!out.contains('\n'), "fixture has no incidents → single-line output");
    }
```

- [ ] **Step 5.8: Run all the failing tests**

Run: `cargo test --package claudehud render::tests --locked`
Expected: every new `_condensed` test FAILS (the stub returns `""`); all existing comfortable tests still PASS.

- [ ] **Step 5.9: Implement `push_model_short`**

Add this helper near `push_model_full`:

```rust
fn push_model_short(input: &Input, out: &mut String) {
    let raw = input
        .model
        .as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    let short = raw.split_once(" (").map(|(prefix, _)| prefix).unwrap_or(raw);
    out.push_str(BLUE);
    out.push_str(short);
    out.push_str(RESET);
}
```

- [ ] **Step 5.10: Implement `push_rate_inline`**

Add this helper near `push_rate_row`:

```rust
fn push_rate_inline(
    label: &str,
    pct: u8,
    resets_at: Option<u64>,
    style: ResetStyle,
    out: &mut String,
) {
    fmt::build_bar(pct, 4, out);
    out.push(' ');
    out.push_str(WHITE);
    out.push_str(label);
    out.push_str(RESET);
    out.push(' ');
    out.push_str(color_for_pct(pct));
    write!(out, "{pct}%").unwrap();
    out.push_str(RESET);
    if let Some(epoch) = resets_at.filter(|&e| e > 0) {
        out.push(' ');
        out.push_str(DIM);
        out.push_str("⟳ ");
        out.push_str(RESET);
        out.push_str(WHITE);
        out.push_str(&format_reset_time(epoch, style));
        out.push_str(RESET);
    }
}
```

Note: `push_rate_inline` does NOT prepend a separator. The caller (`render_condensed`) is responsible for emitting `SEP` before each rate chunk so layout composition stays explicit.

- [ ] **Step 5.11: Implement `render_condensed` body**

Replace the stub at the bottom of `claudehud/src/render.rs` with:

```rust
fn render_condensed(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
) -> String {
    let mut out = String::with_capacity(512);

    // ── Model (short) ──────────────────────────────────────
    push_model_short(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, &mut out);

    // ── Dir + git (tight) ──────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), true, &mut out);

    // ── Rate limits inline ─────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str(SEP);
                push_rate_inline("5h", pct, fh.resets_at, ResetStyle::Time, &mut out);
            }
        }
        if let Some(sd) = &rl.seven_day {
            if let Some(pct_f) = sd.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str(SEP);
                push_rate_inline("7d", pct, sd.resets_at, ResetStyle::DateTime, &mut out);
            }
        }
    }

    // ── Incidents ──────────────────────────────────────────
    push_incidents(incidents, total_active, &mut out);

    out
}
```

Note: the rate-limit logic is intentionally flatter than comfortable's (which gates 7d on the presence of 5h pct). Condensed treats them independently — if 5h is missing but 7d is present, 7d still renders.

- [ ] **Step 5.12: Run tests**

Run: `cargo test --package claudehud --locked`
Expected: PASS — all 7 new condensed tests + every existing comfortable test green.

- [ ] **Step 5.13: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 5.14: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "feat(claudehud): implement condensed layout

Single-line HUD: model (short) | ctx% | dir(branch*) | ○○○○ 5h N% ⟳ T |
○○○○ 7d N% ⟳ T. Incidents still render on their own line below. Rate
chunks render independently — 5h-only and 7d-only configurations both
work."
```

---

## Task 6: Wire `CLAUDEHUD_LAYOUT` env var

**Files:**
- Modify: `claudehud/src/main.rs`

- [ ] **Step 6.1: Read the existing rounding-mode parse block for reference**

Run: `rg -n "CLAUDEHUD_LOG|usage-rounding-mode" claudehud/src/main.rs`
You'll see: rounding parsed at lines 43-58, `CLAUDEHUD_LOG` env read at line 85. The new layout parse mirrors the rounding pattern but reads from env instead of args.

- [ ] **Step 6.2: Add the env-var parse block**

In `claudehud/src/main.rs`, insert this block immediately after the `let rounding = match args.opt_value_from_str...` block (around line 58, before `let mut raw = String::new();`):

```rust
    let layout = match std::env::var("CLAUDEHUD_LAYOUT") {
        Ok(s) if !s.is_empty() => match render::Layout::parse(&s) {
            Some(l) => l,
            None => {
                eprintln!(
                    "claudehud: unknown CLAUDEHUD_LAYOUT '{s}' (want: comfortable|condensed)"
                );
                render::Layout::default()
            }
        },
        _ => render::Layout::default(),
    };
```

- [ ] **Step 6.3: Pass `layout` into `render::render`**

Replace the call at the bottom of `main()`:

Before (left from Task 4):
```rust
print!(
    "{}",
    render::render(&input, git, &incidents, total_active, rounding, render::Layout::default())
);
```

After:
```rust
print!(
    "{}",
    render::render(&input, git, &incidents, total_active, rounding, layout)
);
```

- [ ] **Step 6.4: Manual smoke test the env var**

Run: `cargo build --release --package claudehud && echo '{"model":{"display_name":"Opus 4.7 (1M context)"},"cwd":"/tmp","rate_limits":{"five_hour":{"used_percentage":9,"resets_at":1705316400},"seven_day":{"used_percentage":12,"resets_at":1705833600}}}' | CLAUDEHUD_LAYOUT=condensed target/release/claudehud`

Expected: a single line containing `Opus 4.7`, `tmp`, `5h 9%`, `7d 12%`, no `current`/`weekly` labels, no blank line.

Run the same without the env var:
`echo '...same json...' | target/release/claudehud`
Expected: comfortable output — model, ctx, dir, blank line, then `current` and `weekly` rows. No `⏱` stopwatch.

Run with garbage:
`echo '...same json...' | CLAUDEHUD_LAYOUT=garbage target/release/claudehud`
Expected: stderr message `claudehud: unknown CLAUDEHUD_LAYOUT 'garbage' (want: comfortable|condensed)`, stdout falls back to comfortable.

- [ ] **Step 6.5: Run the full test suite**

Run: `cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6.6: Lint and format**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: clean.

- [ ] **Step 6.7: Commit**

```bash
git add claudehud/src/main.rs
git commit -m "feat(claudehud): wire CLAUDEHUD_LAYOUT env var

CLAUDEHUD_LAYOUT=condensed flips the HUD to single-line condensed mode.
Unset/empty/'comfortable' keep the default. Unknown values warn to
stderr and fall back to comfortable, matching --usage-rounding-mode."
```

---

## Task 7: README documentation

**Files:**
- Modify: `README.md`

- [ ] **Step 7.1: Find the best insertion point**

Run: `rg -n "CLAUDEHUD_LOG|Configuration|## " README.md`
The README currently has a `## Configuration` section starting around line 98. Add a new subsection `### Layout` under it, before `### Claude Code` or after it — pick the position consistent with how the file already orders by user-facing vs internal config. (A new `### Layout` heading is fine; if the file uses `### Daemon` further down, mirror that style.)

- [ ] **Step 7.2: Add the docs block**

Add the following content as a new `### Layout` subsection under `## Configuration` in `README.md`. (The block below is the literal markdown to paste — note the nested code fences need to be authored with care; if your editor mangles them, write each fenced sub-block separately.)

Section heading:

```
### Layout
```

Section body (verbatim markdown — paste below the heading):

```
Two render layouts: `comfortable` (default) and `condensed`. Set with the
`CLAUDEHUD_LAYOUT` environment variable.

    CLAUDEHUD_LAYOUT=condensed

Comfortable renders the HUD across multiple lines with full bars and a
blank gap before rate limits:

    Opus 4.7 (1M context) │ ✍️ 4% │ claudehud (main*)

    current ○○○○○○○○○○   9% ⟳ 10:50am
    weekly  ●○○○○○○○○○  12% ⟳ apr 25, 7:00pm

Condensed collapses everything onto a single line, with shorter
rate-limit bars (4 dots) inline:

    Opus 4.7 │ ✍️ 4% │ claudehud(main*) │ ○○○○ 5h 9% ⟳ 10:50am │ ○○○○ 7d 12% ⟳ apr 25, 7:00pm

Active incidents still render on their own line below row 1 in both
layouts.

Unknown values (`CLAUDEHUD_LAYOUT=foo`) print a warning to stderr and
fall back to `comfortable`.
```

Indented-4-space code blocks are used here intentionally to sidestep nested triple-backtick issues. If the rest of `README.md` uses fenced code blocks throughout, you may convert these indented blocks to fenced blocks once they're pasted — just keep the fences balanced.

- [ ] **Step 7.3: Commit**

```bash
git add README.md
git commit -m "docs: document CLAUDEHUD_LAYOUT env var"
```

---

## Self-Review Checklist (run after all tasks)

- [ ] Run `cargo test --workspace --locked` from project root — all green
- [ ] Run `cargo fmt --all -- --check` — clean
- [ ] Run `cargo clippy --workspace --all-targets --locked -- -D warnings` — clean
- [ ] Manual smoke test all 3 env-var states (unset, `condensed`, garbage) per Task 6.4
- [ ] Confirm `git log --oneline ^main HEAD` shows ~7 focused commits, each independently revertible
- [ ] Open the diff vs main; confirm:
  - No `⏱` glyph references anywhere
  - No `session_duration` references anywhere
  - `Layout::Comfortable` / `Layout::Condensed` reachable from `claudehud::render::Layout`
  - `CLAUDEHUD_LAYOUT` documented in `README.md`
