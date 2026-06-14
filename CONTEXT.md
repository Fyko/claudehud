# claudehud

A Rust statusline for Claude Code: a `claudehud` client reads the harness's JSON payload from stdin and renders a HUD, backed by a daemon that caches per-repo git status in mmap files.

## Language

**HUD**:
The rendered visual output — the model/context/git block, rate-limit bars, and any incident line.
_Avoid_: statusline (means the integration seam, not the output), status bar

**Statusline**:
Claude Code's integration seam — the `statusLine` key in `settings.json` and the `render` subcommand that feeds it. Not a synonym for the HUD.
_Avoid_: using to mean the rendered output

**Client**:
The `claudehud` binary — reads JSON from stdin, reads the git cache, writes the HUD to stdout.
_Avoid_: "the binary" (ambiguous; there are two)

**Daemon**:
The `claudehud-daemon` binary — watches git repos, polls status.claude.com, self-updates, and writes caches the client reads.

**Segment**:
A single `│`-delimited piece of the HUD's first line — e.g. the model segment, context segment, git segment, cost segment.
_Avoid_: field, part

**Rate-limit row**:
A rate-limit bar shown on its own line in the comfortable layout (`current`, `weekly`). Collapses to an inline **rate-limit segment** in the condensed layout.

**Incident line**:
The hyperlinked line rendered below line 1 when status.claude.com reports an active incident or in-progress maintenance. Not a segment.

**Layout**:
The render mode — `comfortable` (multi-line, default) or `condensed` (single line). Set via `CLAUDEHUD_LAYOUT`.

**Registration marker**:
The file the client writes under `/tmp/clhud-watch/{hash}` to register a repo for watching; the daemon watches that directory and picks it up. To "register" a repo is to write this marker.
_Avoid_: marker file, watch marker

**Cache file**:
The per-repo mmap-backed file (`/tmp/clhud-{hash}.bin`) holding git status (seqlock counter, dirty flag, branch name). The daemon writes it; the client reads it directly.

## Relationships

- The **Client** renders the **HUD**; Claude Code invokes the client through the **Statusline** seam.
- The **Daemon** writes the **Cache file**; the **Client** reads it directly — the client never talks to the daemon over a socket.
- The **Client** registers a repo by writing a **Registration marker**; the **Daemon** then writes that repo's **Cache file**.
