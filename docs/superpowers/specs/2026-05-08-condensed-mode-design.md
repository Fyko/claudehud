# Condensed Layout Mode

## Problem

`claudehud`'s default render produces up to 5 visible lines plus a blank gap when both rate-limit rows and an incident are present:

```
Opus 4.7 (1M context) │ ✍️ 4% │ claudehud (main*) │ ⏱ 2h16m
Elevated errors on Claude Opus 4.7 · started 2h16m ago

current ○○○○○○○○○○   9% ⟳ 10:50am
weekly  ●○○○○○○○○○  12% ⟳ apr 25, 7:00pm
```

In narrow splits, on small terminals, or simply for users who prefer a tighter HUD, this dominates vertical real estate. Idle (no incident) is still 4 lines + a blank gap.

## Goal

Add a `condensed` layout that renders the same information set on **1 line idle / 2 lines with active incident**, controlled by an environment variable. `comfortable` remains the default and is unchanged in semantics; the only behavioral drift in the default mode is that session duration disappears from both layouts (it is not the kind of signal the user reaches for, and removing it lets the comfortable header fit on a single visual chunk consistently).

## Non-Goals

- A fully customizable layout (token-stream config, user-defined columns). Two named modes only.
- A CLI flag (`--layout condensed`). The toggle is env-only, matching the existing `CLAUDEHUD_LOG` convention used at `claudehud/src/main.rs:85`.
- Per-mode color theming. Color logic (`color_for_pct`, `color_for_severity`) is identical across both modes.
- Truncation logic for very narrow terminals. Condensed will be wider than comfortable's first line; users on terminals too narrow to fit it can stay on comfortable.
- Live mode switching (e.g. shrink automatically when terminal width drops). The mode is resolved once at process start.

## Final Layouts

### Comfortable (default; current minus stopwatch)

Idle:

```
Opus 4.7 (1M context) │ ✍️ 4% │ claudehud (main*)

current ○○○○○○○○○○   9% ⟳ 10:50am
weekly  ●○○○○○○○○○  12% ⟳ apr 25, 7:00pm
```

With incident:

```
Opus 4.7 (1M context) │ ✍️ 4% │ claudehud (main*)
Elevated errors on Claude Opus 4.7 · started 2h16m ago

current ○○○○○○○○○○   9% ⟳ 10:50am
weekly  ●○○○○○○○○○  12% ⟳ apr 25, 7:00pm
```

Identical to today except `⏱ 2h16m` is removed from line 1.

### Condensed (`CLAUDEHUD_LAYOUT=condensed`)

Idle:

```
Opus 4.7 │ ✍️ 4% │ claudehud(main*) │ ○○○○ 5h 9% ⟳ 10:50am │ ○○○○ 7d 12% ⟳ apr 25, 7:00pm
```

With incident:

```
Opus 4.7 │ ✍️ 4% │ claudehud(main*) │ ○○○○ 5h 9% ⟳ 10:50am │ ○○○○ 7d 12% ⟳ apr 25, 7:00pm
Elevated errors on Claude Opus 4.7 · started 2h16m ago
```

## Layout Differences (Comfortable → Condensed)

| Element | Comfortable | Condensed |
|---|---|---|
| Model name | `display_name` verbatim (e.g. `Opus 4.7 (1M context)`) | parenthetical suffix stripped (`Opus 4.7`) |
| Context % | `✍️ N%` | `✍️ N%` (unchanged) |
| Dir + branch spacing | `dirname (branch*)` | `dirname(branch*)` (no space before paren) |
| Session duration | removed | removed |
| Rate-limit position | own row, after a blank line | inline on row 1, separated by `│` |
| Rate-limit bar width | 10 dots | 4 dots |
| Reset time format | `10:50am` (5h) / `apr 25, 7:00pm` (7d) | identical |
| Incident line | own line below row 1 | own line below row 1 |
| `+N more` overflow | own line below incidents | identical |
| Blank gap before rate limits | yes (`\n\n`) | n/a (rate limits are inline) |

Rate-limit rendering rules:
- 5h and 7d are independently optional. If `five_hour.used_percentage` is missing, neither inline rate chunk renders. If 5h renders and 7d is missing, only the 5h chunk renders. This mirrors today's nested `if let` structure at `render.rs:117-133`.
- Reset clock omitted (per chunk) when `resets_at` is absent or zero, mirroring `push_rate_row`.

Incident rendering rules: byte-for-byte identical to comfortable, including hyperlinks, severity color, `· started Xm ago` suffix, and `+N more` link.

## Configuration

`CLAUDEHUD_LAYOUT` environment variable. Recognized values (case-insensitive):

| Value | Effect |
|---|---|
| unset / empty | `Comfortable` (default) |
| `comfortable` | `Comfortable` |
| `condensed` | `Condensed` |
| anything else | warn to stderr (`claudehud: unknown CLAUDEHUD_LAYOUT '<x>' (want: comfortable\|condensed)`), fall back to `Comfortable` |

This mirrors how `--usage-rounding-mode` parses at `claudehud/src/main.rs:43-52`. The variable is read once in `main` and threaded into `render` as a `Layout` enum value.

## Architecture

### Types

In `claudehud/src/render.rs`:

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
            "condensed"   => Some(Self::Condensed),
            _ => None,
        }
    }
}
```

### Render dispatch

`render()` becomes a thin dispatcher:

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
        Layout::Condensed   => render_condensed(input, git, incidents, total_active, rounding),
    }
}
```

### Shared private helpers

To prevent the two paths from drifting, the row-building primitives become small private functions both modes consume:

| Helper | Used by | Notes |
|---|---|---|
| `push_model_full(input, out)` | comfortable | full `display_name`, fallback `"Claude"` |
| `push_model_short(input, out)` | condensed | strip `" (...)"` parenthetical suffix from `display_name` |
| `push_context(input, rounding, out)` | both | `✍️ N%` chunk |
| `push_dir_branch(input, git, tight, out)` | both | `tight=true` omits the space before `(branch)` |
| `push_incidents(incidents, total_active, out)` | both | extracted from current `render`. Always prepends `\n` per incident. |
| `push_rate_row(label, pct, resets_at, style, out)` | comfortable | existing 10-dot row, unchanged |
| `push_rate_inline(label, pct, resets_at, style, out)` | condensed | new — emits `○○○○ <label> N% ⟳ <reset>` with no leading separator (caller adds `SEP`) |

Bar width is selected by reusing `fmt::build_bar(pct, width, out)`: `width=10` for comfortable rows, `width=4` for condensed inline.

### Model-name shortening

`push_model_short` truncates at the first ` (` substring:

```rust
let raw = input.model.as_ref().and_then(|m| m.display_name.as_deref()).unwrap_or("Claude");
let short = raw.split_once(" (").map(|(prefix, _)| prefix).unwrap_or(raw);
```

Falls through unchanged for names without a parenthetical (`"Claude"`, `"claude-sonnet-4-5"`, etc.).

### Wire-up in `main.rs`

After the existing rounding-mode parsing block, parse `CLAUDEHUD_LAYOUT`:

```rust
let layout = match std::env::var("CLAUDEHUD_LAYOUT") {
    Ok(s) if !s.is_empty() => match Layout::parse(&s) {
        Some(l) => l,
        None => {
            eprintln!("claudehud: unknown CLAUDEHUD_LAYOUT '{s}' (want: comfortable|condensed)");
            Layout::default()
        }
    },
    _ => Layout::default(),
};
```

Passed as the new sixth arg to `render::render`.

### Files touched

| File | Change |
|---|---|
| `claudehud/src/render.rs` | Add `Layout` enum + parse. Split `render` into dispatcher + `render_comfortable` + `render_condensed`. Extract row helpers. Drop the session-duration block from comfortable. |
| `claudehud/src/main.rs` | Read & parse `CLAUDEHUD_LAYOUT`. Pass `Layout` to `render`. |
| `claudehud/src/time.rs` | Remove `session_duration` helper if it has no remaining callers. |
| `claudehud/src/input.rs` | No change. |
| `claudehud/src/fmt.rs` | No change. `build_bar` already accepts `width`. |
| `README.md` | Document `CLAUDEHUD_LAYOUT` env var alongside `CLAUDEHUD_LOG`. |

## Testing

Add `_condensed` twins for every existing render assertion in `claudehud/src/render.rs`'s `tests` module. Each verifies:

- **`test_render_default_model_condensed`** — default `Claude` model name still appears.
- **`test_render_model_name_condensed`** — `display_name` renders, parenthetical suffix stripped (input `"Opus 4.7 (1M context)"` → output contains `Opus 4.7` but not `(1M context)`).
- **`test_render_context_pct_condensed`** — `50%` renders.
- **`test_render_git_branch_condensed`** — `(main)` renders.
- **`test_render_git_dirty_condensed`** — `*` renders inside the paren.
- **`test_render_dirname_condensed`** — directory basename renders.
- **`test_render_rate_limits_present_condensed`** — `5h` and `7d` labels present, `current`/`weekly` labels absent (those belong to comfortable). Reset times present. Output contains **zero** `\n` (idle condensed is strictly single-line; no blank gap, no rate row split).
- **`test_render_incident_present_major_condensed`** — incident on its own line below row 1; row 1 still contains rate-limit chunks.
- **`test_render_incident_with_plus_n_more_condensed`** — `+N more` link still works.
- **`test_render_no_incident_unchanged_shape_condensed`** — output contains zero `\n` (true single-line output).
- **`test_render_real_stdin_fixture_condensed`** — feeds the same fixture as the existing test, asserts model/ctx/dir/rate-limit chunks all render on a single line.
- **`test_layout_parse`** — covers `"comfortable"`, `"condensed"`, casing variants, garbage strings.

Comfortable-side regression coverage:
- **`test_render_no_session_duration_in_comfortable`** — the existing `test_render_real_stdin_fixture` should be updated to additionally assert the stopwatch glyph (`⏱`) and the duration string never appear.

The `strip_ansi` helper at `render.rs:216` works for both layouts unchanged.

## Risks & Open Questions

- **Line width**: condensed's row 1 with both rate limits, resets, model, dir, branch can exceed 120 columns when `display_name`, `dirname`, or `branch` are long. There is no truncation logic; users on narrow terminals see line wrap. Acceptable for v1 — flagged for follow-up if reported.
- **Stopwatch removal in comfortable**: this is technically a behavior change for default users, not strictly part of "add a new mode." Documented prominently in the changelog / commit message.
- **Reset-style consistency**: 7d still uses `apr 25, 7:00pm` (DateTime). On condensed, this contributes ~12 chars to row 1. Considered shortening to date-only for condensed; rejected to keep mode behavior easy to predict (resets render the same regardless of mode).
