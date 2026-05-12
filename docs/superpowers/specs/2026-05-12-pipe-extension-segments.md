# Pipe-Extension Segments

## Problem

claudehud renders a fixed set of segments. Users cannot inject context-specific
information — kubernetes context, AWS profile, active tmux session, custom
project metadata — without forking the binary. The shell can answer these
questions trivially, but the 2.6ms render path forbids subprocess spawns.

## Goal

Let users declare arbitrary shell commands whose stdout becomes a statusline
segment, with the daemon running them on a schedule and caching results in mmap.
The client reads from mmap on each render — identical access pattern to git
status and incidents. The render path stays subprocess-free.

## Non-Goals

- **Live output.** Segments are polled, not streaming.
- **Shell metacharacter support.** Commands are split on whitespace and passed as
  argv. Users who need pipes/redirects write `bash -c '...'` explicitly.
- **Per-directory segment scoping.** v1 segments are global.
- **Colorized output pass-through.** ANSI escapes in command stdout are stripped.
  Segments are plain text; the renderer controls color.
- **`claudehud config validate` subcommand.** Nice-to-have; deferred.

## Design Decisions

### Config drift between client and daemon

Both processes need the config: the daemon to know what to run; the client to
know what to render and where. Duplication is avoided by putting the config
types in `common`. Both crates depend on `common` already. Client parses config
once per invocation (cheap — small TOML, no subprocess). Daemon parses on
startup and re-parses on config file change.

### ANSI escapes

Stripped before writing to mmap. Terminal output of arbitrary commands can
contain ANSI that would corrupt the surrounding statusline. Plain text only;
segment rendered with a fixed dim color. This is conservative and avoids
terminal corruption.

The strip is a byte-level pass: skip bytes from `ESC [` to first `m`, and from
`ESC ]` to `BEL` or `ST`. Simple enough to implement without pulling in a crate.

### Timeout / stall

Each segment runs in its own thread. Timeout is enforced by spawning the child,
parking the scheduler thread in `child.wait()`, and using a secondary timer
thread that sends `SIGKILL` (Unix) / `TerminateProcess` (Windows) after the
deadline. Standard `std::process::Child::kill()` handles both. If the kill
itself fails (process already exited), that is a no-op. The scheduler thread
never stalls permanently.

### UTF-8 truncation

Truncation to `max_bytes` finds the last char boundary at or before the limit
via `floor_char_boundary` (nightly) — we replicate the logic in stable Rust:
walk backward from the limit until `s.is_char_boundary(i)`. Output is already
valid UTF-8 after ANSI stripping (we only strip ASCII escape sequences and
pass UTF-8 content bytes through unchanged).

### World-writable config

On Unix, the daemon refuses to load a config file where `mode & 0o002 != 0`
(world-writable). Printed warning; no segments run. Same check on the parent
directory (`~/.config/claudehud/`) is a nice-to-have for v1 — skipped.

### Hot-reload

`notify` watches the config file path. On `Modify` event: stop all segment
scheduler threads (signal via a `shutdown_tx` crossbeam sender), wait for them
to drain, re-parse config, start new scheduler threads. Threads check the
shutdown signal between ticks.

**Known limitation (v1):** if the config file is replaced atomically (many
editors write to a temp file then rename), `notify` on some platforms fires
`Remove` + `Create` rather than `Modify`. The daemon watches the parent
directory for config changes to cover both cases.

## Positions Enum

Six positions — sufficient to cover the two-line comfortable layout and the
one-line condensed layout:

| Value | Comfortable | Condensed |
|-------|-------------|-----------|
| `before-model` | start of line 1, before model name | start of line 1 |
| `after-branch` | after dir/branch, same line | after dir/branch |
| `end-line-1` | end of line 1 | end of line 1 |
| `before-rate` | before rate-limit bars (comfortable only) | omitted |
| `line-2` | second newline block | omitted |
| `end-line-2` | end of second newline block | omitted |

In condensed mode, `before-rate`, `line-2`, and `end-line-2` segments are
silently omitted (condensed layout has no second-line block and no separate
rate-limit row).

## Mmap Layout

File: `{cache_dir()}/clhud-seg-{fnv32(name)}.bin`

```
[0..8]   u64  seqlock counter
[8]      u8   payload length (0–max_bytes, max 128)
[9..9+N] u8[] payload bytes (UTF-8, ANSI-stripped, newlines stripped)
```

Fixed file size: `8 + 1 + 128 = 137 bytes`. The `max_bytes` field in config
(default 64, max 128) controls how many bytes are written to the payload field;
the rest is zero-padded. File is always exactly 137 bytes regardless of
`max_bytes`.

## Config Schema

Location:
- Unix: `$XDG_CONFIG_HOME/claudehud/config.toml` (default
  `~/.config/claudehud/config.toml`)
- Windows: `%APPDATA%\claudehud\config.toml`

```toml
[[segment]]
name     = "kube-ctx"
cmd      = "kubectl config current-context"
interval = "30s"
position = "after-branch"
max_bytes = 64          # default, max 128
timeout   = "5s"        # default
show_on_error = false   # default: empty segment on nonzero exit
```

`interval` and `timeout` are parsed from duration strings: `<N>ms`, `<N>s`,
`<N>m`, `<N>h`. No external dependency — hand-rolled parser is ~30 lines.

## File Structure

**New files:**
- `common/src/segments.rs` — `SegmentConfig`, `Position`, `parse_duration`,
  `seg_path`, `seg_mmap_size`, seqlock write/read helpers for segments
- `claudehud-daemon/src/segments.rs` — scheduler, timeout enforcement, ANSI
  strip, config watcher
- `claudehud/src/segments.rs` — config load + mmap read, `push_segments` helper

**Modified files:**
- `Cargo.toml` (workspace) — add `toml` + `serde` to workspace deps
- `common/Cargo.toml` — add `serde` (derive), `toml`
- `claudehud-daemon/src/main.rs` — spawn segment scheduler thread
- `claudehud/src/render.rs` — accept `&[SegmentOutput]` slice, splice at
  declared positions
- `claudehud/src/main.rs` — load segments, pass to render
- `claudehud/src/lib.rs` — re-export `segments`
- `README.md` — configuration section + security note

## Security

- Daemon refuses config file that is world-writable (Unix only — Windows has no
  equivalent simple check; documented).
- Commands are never passed through a shell. `cmd` is split on ASCII whitespace;
  `Command::new(parts[0]).args(&parts[1..])` is the only invocation form.
- Stdout truncated hard at `max_bytes` (128 max) on a char boundary.
- ANSI escapes stripped before writing to mmap.
- Users are warned loudly in README that segments run with daemon privileges.

## Testing

- TOML parsing: valid config, missing required fields, invalid position, invalid
  duration, `max_bytes` > 128 clamped
- Duration parser: `10ms`, `30s`, `2m`, `1h`, invalid string
- Mmap encode/decode roundtrip (write → seqlock read → assert payload)
- Position parsing: all 6 values, unknown string returns error
- UTF-8-safe truncation: multi-byte char at boundary, purely ASCII, empty
- ANSI strip: CSI sequence, OSC sequence, mixed content
- World-writable refusal (Unix only): tempfile with mode 0o666, expect `None`
  from config loader
- Timeout enforcement: command that sleeps longer than timeout, expect empty/
  error result within deadline + epsilon
- Config hot-reload: write config, signal watcher, verify new segment active
