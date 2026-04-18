# `claudehud install` Subcommand

## Problem

`install.sh` edits `~/.claude/settings.json` with `sed`. Two latent bugs live in that path:

1. The sed expression uses `/` as its delimiter but interpolates `$INSTALL_DIR` unescaped. Any path containing `/` (i.e. every path) breaks out of the replacement, parsing path segments as sed flags. A user with `INSTALL_DIR=/Users/alex/.local/bin` gets `sed: bad flag 'U'`.
2. The replacement regex `s/}[[:space:]]*$/.../` matches *every* closing brace at end of line, not just the outermost one. On any `settings.json` containing nested objects (e.g. `hooks`), sed injects a bogus `statusLine` block inside each nested object, producing invalid JSON — silently, with exit 0.

The prior iteration of this work moved json editing to a `jq`-then-`python3`-then-warn ladder inside `install.sh`. It works, but it duplicates logic that the `claudehud` Rust binary is already better suited to do: it already loads and parses Claude Code settings JSON, already depends on `serde_json`, and is already being placed on disk one step earlier in the install flow.

## Goal

Replace all JSON editing inside `install.sh` with a single invocation of `claudehud install`. The Rust binary becomes the canonical tool for wiring itself into Claude Code's configuration; the shell script goes back to doing what shell does well (platform detection, binary download, daemon registration).

## Non-Goals

- Moving daemon registration (launchd plists / systemd units) into the Rust binary. Static here-docs per platform are fine in shell; no parsing, no bug surface.
- Moving binary download / version detection into Rust. Bootstrap chicken-and-egg: the binary has to exist before it can be invoked.
- Uninstall support (`claudehud uninstall`). Out of scope; can be added later if asked.
- Writing Claude Code project-level `.claude/settings.json` files. Only the user-level config is configured.
- Preserving comments in `settings.json`. JSON has no comments; Claude Code's own settings loader rejects JSONC.

## High-Level Design

A new subcommand `claudehud install` resolves the Claude Code settings path, loads the existing JSON (if any), sets `.statusLine.command` to the current binary's absolute path, and writes the result atomically. On collision with an existing `statusLine`, it prompts when stdin is a TTY and errors out when it isn't — unless `--force` is passed.

`install.sh` drops its `configure_claude` / `set_statusline` / `warn` helpers entirely and replaces the old sed block with a single line invoking `claudehud install`, threading `CLAUDEHUD_FORCE_CONFIG` through as `--force` and respecting `CLAUDEHUD_SKIP_CONFIG` by skipping the invocation.

## Command Shape

```
claudehud install [--force] [--settings <PATH>] [--dry-run]
```

- No flags: discover settings path, install `statusLine`, prompt on collision if TTY.
- `--force`: overwrite an existing `statusLine` without prompting.
- `--settings <PATH>`: explicit path override. Wins over `$CLAUDE_CONFIG_DIR` and the default.
- `--dry-run`: print the resulting JSON to stdout; do not write. Useful for debugging and as a manual-paste escape hatch when filesystem writes fail.

### Settings Path Discovery

In order of precedence:

1. `--settings <PATH>` if supplied.
2. `$CLAUDE_CONFIG_DIR/settings.json` if `CLAUDE_CONFIG_DIR` is set. Matches Claude Code's own discovery rule, so a user who moved their config also moves ours.
3. `$HOME/.claude/settings.json` otherwise.

### Behavior Matrix

| State | No flag | `--force` |
|---|---|---|
| Parent directory of settings path missing | silent skip, exit 0 | silent skip, exit 0 |
| `settings.json` missing | create with `statusLine` | create with `statusLine` |
| `settings.json` exists, no `statusLine` | add `statusLine` | add `statusLine` |
| `settings.json` exists, has `statusLine`, stdin is TTY | prompt `[y/N]` | overwrite |
| `settings.json` exists, has `statusLine`, stdin is non-TTY | exit 1 with hint | overwrite |
| `settings.json` exists but is invalid JSON | exit 2 with error | exit 2 with error |

"Parent directory" means the `dirname` of the resolved settings path (`$HOME/.claude`, `$CLAUDE_CONFIG_DIR`, or `dirname $(--settings)`). If the user passed an explicit `--settings` pointing into a nonexistent directory, we still silent-skip rather than creating the tree — that was their mistake to make, and creating arbitrary directories on their behalf is worse than a no-op.

Silent-skip on missing parent directory preserves current behavior: a user who installed `claudehud` before ever running `claude` gets no error; the next Claude Code run creates `~/.claude` and the next `install.sh` run (upgrade) wires up the `statusLine`.

Non-TTY + collision + no `--force` fails loud instead of silently skipping or corrupting stdin. This is the key UX improvement over `install.sh`'s current `read -r response` under `curl | sh`, which reads garbage or EOFs.

Invalid JSON fails loud. The current shell path silently makes it worse.

## Components

### `claudehud/src/main.rs`

Route on the first positional argument. If it is literally `install`, dispatch to `install::run(args)` with the remaining arguments. Otherwise fall through to the existing statusline-render path. The hot path (statusline rendering on every keystroke in Claude Code) is unchanged — no added work, no added deps loaded.

```rust
fn main() -> ExitCode {
    let mut args = pico_args::Arguments::from_env();
    if args.subcommand().ok().flatten().as_deref() == Some("install") {
        return install::run(args);
    }
    // ... existing render path
}
```

`pico_args::Arguments::subcommand()` pops the first free-standing argument if it doesn't start with `-`; exactly what we want.

### `claudehud/src/install.rs` (new)

One public function, `pub fn run(args: pico_args::Arguments) -> ExitCode`, plus private helpers.

Responsibilities:

- Parse `--force`, `--settings <PATH>`, `--dry-run`, `-h`/`--help`. Reject unknown args with a nonzero exit.
- Resolve the settings path per the precedence above.
- Resolve the absolute path of the currently-running binary via `std::env::current_exe()`. This is what we write into `.statusLine.command` — correctly reflecting wherever the user placed the binary (`~/.local/bin/claudehud`, `/opt/homebrew/bin/claudehud`, a nix store path, whatever).
- Load-or-initialize the JSON. Missing file → start with an empty `serde_json::Value::Object`. Invalid JSON → fail.
- Decide: prompt / overwrite / skip / error, per the behavior matrix.
- On write: serialize with 2-space indentation and a trailing newline, matching the style Claude Code itself writes. Write to `<settings-path>.claudehud-tmp` in the same directory, then `rename` onto the final path (atomic on the same filesystem). Clean up the tempfile on any error path.
- Print one human-readable status line on stdout, matching `install.sh`'s `say` output style (`==> added statusLine to /Users/alex/.claude/settings.json`).

#### JSON key-order preservation

`serde_json`'s default HashMap-based object representation does not preserve key insertion order, so round-tripping a user's `settings.json` would reorder their keys. Enable `serde_json`'s `preserve_order` feature in `claudehud/Cargo.toml`:

```toml
serde_json = { version = "1", features = ["preserve_order"] }
```

This pulls in `indexmap` transitively (already a tiny, no-op-at-runtime dep) and makes `Value::Object` an order-preserving map. Required; a user's custom settings file would otherwise come back scrambled.

#### TTY detection

`std::io::IsTerminal` (stable since 1.70) on `io::stdin()`. No external dep.

#### Prompt

Write `/Users/alex/.claude/settings.json already has a statusLine. Overwrite? [y/N] ` to stderr, read one line from stdin, accept `y` / `Y` / `yes` / `YES` as yes, everything else as no. Matches current shell prompt wording.

### `claudehud/Cargo.toml`

Add `features = ["preserve_order"]` to the `serde_json` dep. No other deps needed; `pico-args` handles subcommand dispatch, `std::io::IsTerminal` and `std::env::current_exe` cover TTY + self-path.

### `install.sh`

Delete `configure_claude`, `set_statusline`, and `warn`. Replace the existing call-site after binary placement with:

```sh
if [ -z "${CLAUDEHUD_SKIP_CONFIG:-}" ]; then
    install_args=""
    [ -n "${CLAUDEHUD_FORCE_CONFIG:-}" ] && install_args="--force"
    "$INSTALL_DIR/claudehud" install $install_args || true
fi
```

The `|| true` ensures a failed `install` (non-TTY collision without `--force`) doesn't abort the rest of the install flow — daemon registration still happens, user still gets the binary on disk, they just need to re-run with `--force` or manually edit. An explicit error message from the subcommand already told them what to do.

Net delta: ~60 lines of shell removed, ~1 invocation added. The `jq`/`python3` dependency trees vanish from the install path.

## Error Handling

- Invalid JSON input: print `claudehud install: <path> is not valid JSON: <serde error>` to stderr, exit 2.
- Permission denied on write: print `claudehud install: cannot write <path>: <io error>` to stderr, exit 1. Tempfile cleaned up.
- `current_exe()` failure (vanishingly rare; needs `/proc` on Linux or a working dyld on macOS): exit 1 with error. In practice this only happens in pathological environments.
- Unknown flags: print usage, exit 2.
- Prompt on non-TTY with no `--force`: exit 1 with `hint: pass --force to overwrite, or set statusLine.command manually to <path>`.

## Testing

Unit tests in `claudehud/src/install.rs` using `tempfile` for scratch dirs. `tempfile` is dev-only; no production dep impact.

Cases to cover:

- **Missing parent dir**: `install` against `<tmp>/nonexistent/settings.json` → exit 0, no file created.
- **Missing settings file**: parent dir exists, file does not → file created, contains only `{"statusLine": {...}}`, parent file mode preserved (or default), trailing newline present.
- **Empty JSON object**: `{}` → `{"statusLine": {"command": "..."}}`.
- **Populated JSON, no statusLine**: `{"theme":"dark","hooks":{...}}` → `statusLine` appended, existing keys preserved in original order, nested `hooks` structure untouched.
- **Populated JSON, has statusLine, `--force`**: overwritten, other keys preserved.
- **Populated JSON, has statusLine, no `--force`, non-TTY**: exit 1, file unchanged.
- **Invalid JSON input**: exit 2, file unchanged, no tempfile left behind.
- **`--dry-run`**: expected JSON printed to stdout, file unchanged, no tempfile created.
- **`--settings` precedence**: explicit `--settings` wins over `CLAUDE_CONFIG_DIR` env var.

TTY-path + interactive prompt isn't unit-testable without a PTY harness; skip in the unit layer, validate by hand before merge.

## Migration

No user action required. Running `install.sh` (fresh install or in-place upgrade) at or after the release that includes this change automatically uses the new code path. Existing `settings.json` files that were successfully edited by the prior shell-based path continue to work unchanged; the new code is idempotent against them (same `.statusLine.command` value → no-op if the binary path hasn't moved, prompt/`--force` if it has).
