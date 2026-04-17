# Status Incidents in Statusline

## Problem

When `status.claude.com` reports an active incident, the user has no in-terminal signal. They discover degraded service through failed requests and have to leave the terminal to check the status page.

## Goal

Surface ongoing `status.claude.com` incidents (and in-progress scheduled maintenance) as a single, clickable line in the `claudehud` statusline output. No action required from the user when there is no incident — the line does not render.

## Non-Goals

- Multi-line display of every concurrent incident.
- A separate CLI subcommand (`claudehud status` etc.).
- Per-user opt-out configuration. A flag can be added later if requested.
- Persisting ETag or last-seen incident state across daemon restarts.
- Covering status pages other than `status.claude.com`.

## High-Level Design

The daemon gains a status-polling thread alongside the existing registrar and watcher. It polls `https://status.claude.com/history.atom` every five minutes using a conditional GET (`If-None-Match` with the last seen `ETag`). When the feed changes, it parses the Atom XML, filters to active incidents and in-progress scheduled maintenance, picks the single most-recent active entry as the "representative" incident, and seqlock-writes that plus the total active count into a new global mmap file at `/tmp/clhud-incidents.bin`.

The client, on every render, attempts to mmap that file. If it exists and `active_count > 0`, it emits a new line directly below line 1 (and above the rate-limit block if present) containing a severity icon, title, "started Xm ago" relative timestamp, and `+N more` suffix when more than one incident is active. The whole line is wrapped in an OSC 8 hyperlink so clicking it opens the incident page.

## Components

### `common/src/incidents.rs` (new)

Shared layout constants, the `Incident` struct, and seqlock read/write helpers that mirror the existing `seqlock_read` in `common/src/lib.rs`.

- `pub const INCIDENTS_MMAP_PATH: &str = "/tmp/clhud-incidents.bin"`
- `pub const INCIDENTS_MMAP_SIZE: usize = 408`
- `pub const TITLE_MAX: usize = 128`
- `pub const URL_MAX: usize = 255`
- `pub enum Severity { None, Minor, Major, Critical, Maintenance }` with `u8` repr.
- `pub struct Incident { severity: Severity, started_at: u64, title: String, url: String, active_count: u8 }`.
- `pub fn seqlock_read_incident(mmap: &[u8]) -> Option<Incident>` — spin on odd counter, read `active_count`, return `None` when zero.
- `pub fn seqlock_write_incident(mmap: &mut [u8], incident: Option<&Incident>)` — daemon side.

### `claudehud-daemon/src/status.rs` (new)

Runs on its own thread spawned from `main.rs`. Owns a `ureq::Agent`, caches the last `ETag` in memory. Each tick:

1. `GET https://status.claude.com/history.atom` with `If-None-Match: <etag>` when present.
2. On `304 Not Modified`: no-op.
3. On `200 OK`: parse body with `roxmltree`, classify entries, pick the representative incident, write to mmap.
4. On network / parse error: log to stderr, leave prior mmap state untouched, retry next cycle.

Poll cadence is a single `const POLL_INTERVAL: Duration = Duration::from_secs(300);`.

### `claudehud-daemon/src/main.rs`

Gains a third `std::thread::spawn` call that invokes `status::start()`. No other changes.

### `claudehud/src/incidents.rs` (new)

Client read path. Mirrors how `git.rs` reads the per-repo mmap:

1. `open(INCIDENTS_MMAP_PATH)` → on `NotFound`, return `None`.
2. `mmap` the file; check length matches `INCIDENTS_MMAP_SIZE`.
3. Call `seqlock_read_incident`.
4. Return `Option<Incident>`.

No fallback, no blocking. If the daemon isn't running, the line simply doesn't appear.

### `claudehud/src/render.rs`

`render` gains an `incident: Option<&Incident>` argument. When `Some`, a new block is emitted between the line 1 block and the rate-limit block:

```
line 1: model · ✍️ ctx% · cwd (branch) · ⏱ duration
line 2: 🟡 Elevated API errors · started 12m ago        [+1 more]   (if incident)
line 3: current  [bar] pct%  ⟳ time                    (if rate limits)
line 4: weekly   [bar] pct%  ⟳ datetime                (if rate limits)
```

## Data Flow

```
status.claude.com/history.atom
            │  5-min poll, If-None-Match
            ▼
  daemon status thread (ureq)
            │  roxmltree parse + classify
            ▼
  /tmp/clhud-incidents.bin  (seqlock-protected)
            │
            ▼
  client mmap read on each render
            │
            ▼
  render line between line 1 and rate limits
```

## Cache File Layout

408 bytes at `/tmp/clhud-incidents.bin`:

| Offset | Size | Field |
|--------|------|-------|
| 0 | 8 | `u64` seqlock counter (even = stable, odd = write in progress) |
| 8 | 1 | `u8` `active_count` (0 = no active incidents) |
| 9 | 1 | `u8` severity (0=none, 1=minor, 2=major, 3=critical, 4=maintenance) |
| 10 | 8 | `u64` `started_at` Unix epoch (LE) |
| 18 | 1 | `u8` title length |
| 19 | 128 | title bytes (UTF-8, zero-padded) |
| 147 | 1 | `u8` url length (max 255) |
| 148 | 255 | url bytes (ASCII, zero-padded) |
| 403 | 5 | padding |

Seqlock semantics match the existing `common::seqlock_read` pattern: writer increments counter to odd before writing, increments to even after; reader spins until it gets two matching even reads bracketing the payload.

## Atom Classification

Statuspage emits one Atom entry per incident (or maintenance). The most recent status phase is embedded in `<title>` with a prefix like `"Investigating - ..."`, `"Identified - ..."`, `"Monitoring - ..."`, `"Resolved - ..."`, `"Scheduled - ..."`, `"In progress - ..."`, `"Completed - ..."`, `"Postmortem - ..."`. Severity appears as `<category term="minor|major|critical|maintenance"/>`.

An entry is **active** iff its title prefix is in the set `{Investigating, Identified, Monitoring, Verifying, Update, In progress}`. The set `{Resolved, Completed, Postmortem, Scheduled}` is **not** active — note that "Scheduled" (announced but not yet started) is deliberately excluded per the Q2 decision.

For each active entry: `started_at` = parsed `<published>`, `url` = `<link href="...">`, `title` = stripped `<title>` (prefix removed). Representative entry = the one with the most recent `<updated>`. `active_count` = total matching entries.

## Render Details

- Severity → icon: `Minor` → `🟡`, `Major` → `🟠`, `Critical` → `🔴`, `Maintenance` → `🔧`.
- Title truncation: fits within remaining terminal budget after icon, `started Xm ago` suffix, and `+N more` (if applicable). Hard-cap at the mmap's 128-byte title storage.
- Relative time: reuses `claudehud/src/time.rs::format_duration` against `SystemTime::now() - started_at`.
- Hyperlink: whole line wrapped in OSC 8 escape: `\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\`. The `+N more` suffix links to `https://status.claude.com/` (the overview page), not the representative incident.

## Error Handling

| Failure | Behavior |
|---------|----------|
| DNS / TCP error | daemon logs `WARN status fetch: <err>` to stderr, retries in 5 min |
| HTTP 5xx | same as above |
| HTTP 304 | no-op, seq unchanged |
| XML parse error | log, retain prior mmap state, retry next cycle |
| mmap file missing on client | skip incident line, silent |
| mmap file truncated / wrong size | skip incident line, silent |
| Daemon not running | no incident line ever appears |

No mechanism alerts the user that the fetch pipeline is broken. The statusline already degrades silently when the daemon is absent (falls back to direct git); this is consistent.

## Dependency Additions

| Crate | Target | Purpose | Approx. Size |
|-------|--------|---------|--------------|
| `ureq` (w/ `rustls` feature) | `claudehud-daemon` | HTTPS client | ~800 KB–1 MB |
| `roxmltree` | `claudehud-daemon` | Atom parse | ~50 KB, zero transitive deps |

Client (`claudehud`) gains no new dependencies. `common` gains no new dependencies.

## Testing

### `common`

- `seqlock_read_incident` / `seqlock_write_incident` round-trip for each severity variant.
- Title / URL truncation at cap boundary writes correct length byte.
- Reading from an all-zero buffer returns `None`.

### `claudehud-daemon`

- Atom parse fixtures (inline `&str` constants in test modules, consistent with existing test style):
  - `active_incident.atom` → returns one active incident, expected severity / title / url.
  - `resolved_incident.atom` → returns `None` (no actives).
  - `in_progress_maint.atom` → returns one active maintenance entry.
  - `scheduled_maint.atom` → returns `None` (scheduled but not started).
  - `multiple_actives.atom` → returns the most-recent-updated entry, `active_count == N`.
  - `empty_feed.atom` → returns `None`.
- ETag conditional GET handling is mocked at the `ureq::Agent` level — omit if mocking proves invasive; minimum bar is the classification logic under test.

### `claudehud`

- Render with `Some(Incident)` for each severity emits icon + title + `started Xm ago`.
- Render with `active_count > 1` emits `+N more` suffix.
- Render with `None` matches existing output byte-for-byte.
- OSC 8 escape is present around the line when `Some`.

## Migration & Compatibility

- The mmap file is new; no schema migration needed.
- Older daemon builds won't write the file; older client builds ignore it. Version skew in either direction degrades gracefully to "no incident line".
- README gets an "Incidents" section documenting the new line and the `/tmp/clhud-incidents.bin` file.

## Open Questions

None — all design forks resolved during brainstorming.
