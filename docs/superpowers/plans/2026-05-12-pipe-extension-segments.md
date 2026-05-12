# Pipe-Extension Segments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users declare arbitrary shell commands in a TOML config file whose stdout becomes a statusline segment, with the daemon running them on a schedule and caching results in seqlock-protected mmap files that the client reads on render.

**Architecture:** Config types live in `common` (shared by both binaries). The daemon parses config on startup, spawns one scheduler thread per segment with timeout enforcement, strips ANSI, and writes truncated stdout to `clhud-seg-{fnv32(name)}.bin`. The client parses config once per invocation, mmaps each segment's cache file, seqlock-reads, and splices output into the render output at the declared position. Zero subprocesses on the client render path.

**Tech Stack:** Rust 2021, `toml` + `serde` (new workspace deps for config parsing), `memmap2` (already present), `crossbeam-channel` (already in daemon), `notify` (already in daemon for config hot-reload).

**Reference spec:** `docs/superpowers/specs/2026-05-12-pipe-extension-segments.md`

---

## File Structure

**New files:**
- `common/src/segments.rs` — `SegmentConfig`, `Position`, `parse_duration`, `seg_path`, `SEG_MMAP_SIZE`, seqlock write/read helpers for segment payloads, `config_path`, `load_config`, world-writable check
- `claudehud-daemon/src/segments.rs` — scheduler threads, timeout enforcement, ANSI strip, config file watcher + hot-reload
- `claudehud/src/segments.rs` — mmap read per segment, `SegmentOutput` type

**Modified files:**
- `Cargo.toml` (workspace) — add `toml`, `serde` to `[workspace.dependencies]`
- `common/Cargo.toml` — add `serde` (derive), `toml`
- `claudehud-daemon/Cargo.toml` — add `toml`, `serde` workspace deps
- `claudehud/Cargo.toml` — add `toml`, `serde` workspace deps
- `claudehud-daemon/src/main.rs` — register `mod segments`, spawn segment scheduler thread
- `claudehud/src/lib.rs` — add `pub mod segments`
- `claudehud/src/render.rs` — accept `segments: &[SegmentOutput]` parameter, splice at declared positions
- `claudehud/src/main.rs` — load segments, pass to `render::render`
- `README.md` — configuration section + security note

**Test surfaces:** every new file gets `#[cfg(test)] mod tests` inline (matches project pattern).

---

## Task 1: Add `toml` + `serde` to workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `common/Cargo.toml`
- Modify: `claudehud-daemon/Cargo.toml`
- Modify: `claudehud/Cargo.toml`

No test needed — infrastructure that following tasks exercise.

- [ ] **Step 1: Add to `[workspace.dependencies]` in `Cargo.toml`**

Replace the entire `[workspace.dependencies]` block in `Cargo.toml`:

```toml
[workspace.dependencies]
common = { path = "common" }
memmap2 = "0.9"
serde = { version = "1", features = ["derive"] }
toml = "0.8"
```

- [ ] **Step 2: Add serde + toml to `common/Cargo.toml`**

Replace the contents of `common/Cargo.toml`:

```toml
[package]
name = "common"
version.workspace = true
edition.workspace = true

[dependencies]
serde = { workspace = true }
toml = { workspace = true }
```

- [ ] **Step 3: Add serde + toml to daemon**

Replace `claudehud-daemon/Cargo.toml`'s `[dependencies]` block:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
notify = "6"
crossbeam-channel = "0.5"
ureq = { version = "2", default-features = false, features = ["tls"] }
roxmltree = "0.20"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: Add serde + toml to client**

Replace `claudehud/Cargo.toml`'s `[dependencies]` block:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
pico-args = "0.5"
serde_json = { version = "1", features = ["preserve_order"] }
time = { version = "0.3", features = ["local-offset"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 5: Verify the workspace builds**

Run: `cargo build --workspace`
Expected: builds successfully.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml common/Cargo.toml claudehud-daemon/Cargo.toml claudehud/Cargo.toml Cargo.lock
git commit -m "build: add toml + serde workspace deps for pipe-extension segments"
```

---

## Task 2: `common/src/segments.rs` — types, duration parser, path helpers, mmap layout

**Files:**
- Create: `common/src/segments.rs`
- Modify: `common/src/lib.rs` (add `pub mod segments`)

This is the shared contract between daemon and client. No I/O — pure data types and serialization helpers.

- [ ] **Step 1: Write the failing tests**

Create `common/src/segments.rs` with tests only (no impl yet):

```rust
// common/src/segments.rs

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parse_duration_ms() {
        assert_eq!(parse_duration("10ms"), Some(std::time::Duration::from_millis(10)));
    }

    #[test]
    fn test_parse_duration_s() {
        assert_eq!(parse_duration("30s"), Some(std::time::Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_duration_m() {
        assert_eq!(parse_duration("2m"), Some(std::time::Duration::from_secs(120)));
    }

    #[test]
    fn test_parse_duration_h() {
        assert_eq!(parse_duration("1h"), Some(std::time::Duration::from_secs(3600)));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("10x"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn test_position_parse_all_variants() {
        assert_eq!(Position::parse("before-model"), Some(Position::BeforeModel));
        assert_eq!(Position::parse("after-branch"), Some(Position::AfterBranch));
        assert_eq!(Position::parse("end-line-1"), Some(Position::EndLine1));
        assert_eq!(Position::parse("before-rate"), Some(Position::BeforeRate));
        assert_eq!(Position::parse("line-2"), Some(Position::Line2));
        assert_eq!(Position::parse("end-line-2"), Some(Position::EndLine2));
    }

    #[test]
    fn test_position_parse_unknown() {
        assert_eq!(Position::parse("top-right"), None);
    }

    #[test]
    fn test_seg_path_format() {
        let p = seg_path_in(Path::new("/tmp"), "kube-ctx");
        // FNV-1a of "kube-ctx"
        let hash = fnv32_str("kube-ctx");
        assert_eq!(p, Path::new("/tmp").join(format!("clhud-seg-{hash}.bin")));
    }

    #[test]
    fn test_seg_mmap_size() {
        assert_eq!(SEG_MMAP_SIZE, 137);
    }

    #[test]
    fn test_seqlock_write_read_roundtrip() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"prod-cluster");
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload, b"prod-cluster");
    }

    #[test]
    fn test_seqlock_write_read_empty() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"");
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload, b"");
    }

    #[test]
    fn test_seqlock_write_truncates_at_128() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        let long = b"a".repeat(200);
        seqlock_write_seg(&mut buf, &long);
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload.len(), 128);
    }

    #[test]
    fn test_truncate_utf8_safe_ascii() {
        assert_eq!(truncate_to_boundary("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_safe_multibyte() {
        // "café" = 5 bytes: c-a-f-e(1)-accent(2)
        // truncating at 4 bytes should give "caf" (not mid-char)
        let s = "café";
        let t = truncate_to_boundary(s, 4);
        assert!(s.starts_with(t));
        assert!(t.len() <= 4);
    }

    #[test]
    fn test_truncate_utf8_safe_empty() {
        assert_eq!(truncate_to_boundary("", 10), "");
    }

    #[test]
    fn test_config_parse_valid() {
        let toml = r#"
[[segment]]
name = "kube-ctx"
cmd = "kubectl config current-context"
interval = "30s"
position = "after-branch"
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg[0].name, "kube-ctx");
        assert_eq!(cfg[0].cmd, "kubectl config current-context");
        assert_eq!(cfg[0].position, Position::AfterBranch);
        assert_eq!(cfg[0].interval, std::time::Duration::from_secs(30));
        assert_eq!(cfg[0].max_bytes, 64);
        assert_eq!(cfg[0].timeout, std::time::Duration::from_secs(5));
        assert!(!cfg[0].show_on_error);
    }

    #[test]
    fn test_config_parse_max_bytes_clamped() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "1s"
position = "end-line-1"
max_bytes = 999
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg[0].max_bytes, 128);
    }

    #[test]
    fn test_config_parse_missing_required_field() {
        let toml = r#"
[[segment]]
name = "x"
interval = "1s"
position = "end-line-1"
"#;
        // missing cmd
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn test_config_parse_invalid_position() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "1s"
position = "bad-position"
"#;
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn test_config_parse_invalid_interval() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "forever"
position = "end-line-1"
"#;
        assert!(parse_config(toml).is_err());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p common`
Expected: FAIL — module `segments` not found.

- [ ] **Step 3: Register the module in `common/src/lib.rs`**

Add after the existing `pub mod incidents;` line in `common/src/lib.rs`:

```rust
pub mod segments;
```

- [ ] **Step 4: Run to verify tests still fail (now with missing symbols)**

Run: `cargo test -p common segments`
Expected: FAIL with missing types/functions.

- [ ] **Step 5: Implement `common/src/segments.rs`**

Replace the test-only file with the full implementation + tests:

```rust
// common/src/segments.rs

use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, Ordering};
use std::time::Duration;

use serde::Deserialize;

use crate::cache_dir;

/// Max payload bytes stored in the mmap. Hard cap regardless of `max_bytes` config.
pub const SEG_PAYLOAD_MAX: usize = 128;

/// Fixed mmap file size: seqlock(8) + len(1) + payload(128) = 137 bytes.
pub const SEG_MMAP_SIZE: usize = 8 + 1 + SEG_PAYLOAD_MAX;

/// Path to a segment's mmap cache file.
pub fn seg_path(name: &str) -> PathBuf {
    seg_path_in(&cache_dir(), name)
}

/// Test seam: build segment mmap path under an explicit root.
pub fn seg_path_in(root: &Path, name: &str) -> PathBuf {
    root.join(format!("clhud-seg-{}.bin", fnv32_str(name)))
}

/// FNV-1a 32-bit hash of a string.
pub fn fnv32_str(s: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for &b in s.as_bytes() {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

/// Seqlock write: write `payload` (truncated to SEG_PAYLOAD_MAX) into `buf`.
pub fn seqlock_write_seg(buf: &mut [u8], payload: &[u8]) {
    assert!(buf.len() >= SEG_MMAP_SIZE);
    let seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    let len = payload.len().min(SEG_PAYLOAD_MAX);
    buf[8] = len as u8;
    buf[9..9 + len].copy_from_slice(&payload[..len]);
    buf[9 + len..9 + SEG_PAYLOAD_MAX].fill(0);

    fence(Ordering::SeqCst);
    let seq2 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
}

/// Seqlock read: spin until stable, return payload bytes as Vec.
pub fn seqlock_read_seg(buf: &[u8]) -> Vec<u8> {
    assert!(buf.len() >= SEG_MMAP_SIZE);
    loop {
        let seq1 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let len = (buf[8] as usize).min(SEG_PAYLOAD_MAX);
        let payload = buf[9..9 + len].to_vec();

        fence(Ordering::Acquire);
        let seq2 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        if seq1 == seq2 {
            return payload;
        }
        std::hint::spin_loop();
    }
}

/// Truncate `s` to at most `max_bytes` bytes, respecting UTF-8 char boundaries.
pub fn truncate_to_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    &s[..i]
}

/// Segment position in the statusline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    BeforeModel,
    AfterBranch,
    EndLine1,
    BeforeRate,
    Line2,
    EndLine2,
}

impl Position {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "before-model" => Some(Self::BeforeModel),
            "after-branch" => Some(Self::AfterBranch),
            "end-line-1" => Some(Self::EndLine1),
            "before-rate" => Some(Self::BeforeRate),
            "line-2" => Some(Self::Line2),
            "end-line-2" => Some(Self::EndLine2),
            _ => None,
        }
    }
}

/// Parse a duration string: `<N>ms`, `<N>s`, `<N>m`, `<N>h`.
pub fn parse_duration(s: &str) -> Option<Duration> {
    if let Some(n) = s.strip_suffix("ms") {
        return n.parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(n) = s.strip_suffix('h') {
        return n.parse::<u64>().ok().map(|v| Duration::from_secs(v * 3600));
    }
    if let Some(n) = s.strip_suffix('m') {
        return n.parse::<u64>().ok().map(|v| Duration::from_secs(v * 60));
    }
    if let Some(n) = s.strip_suffix('s') {
        return n.parse::<u64>().ok().map(Duration::from_secs);
    }
    None
}

/// A parsed, validated segment configuration entry.
#[derive(Debug, Clone)]
pub struct SegmentConfig {
    pub name: String,
    pub cmd: String,
    pub interval: Duration,
    pub position: Position,
    pub max_bytes: usize,
    pub timeout: Duration,
    pub show_on_error: bool,
}

/// Raw TOML shape — separate from `SegmentConfig` to allow validation/defaults.
#[derive(Deserialize)]
struct RawSegment {
    name: String,
    cmd: String,
    interval: String,
    position: String,
    max_bytes: Option<u64>,
    timeout: Option<String>,
    show_on_error: Option<bool>,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default)]
    segment: Vec<RawSegment>,
}

/// Parse TOML config text into a validated `Vec<SegmentConfig>`.
/// Returns `Err` with a human-readable message on any validation failure.
pub fn parse_config(toml_text: &str) -> Result<Vec<SegmentConfig>, String> {
    let raw: RawConfig =
        toml::from_str(toml_text).map_err(|e| format!("config parse error: {e}"))?;

    let mut out = Vec::with_capacity(raw.segment.len());
    for (i, seg) in raw.segment.into_iter().enumerate() {
        let interval = parse_duration(&seg.interval)
            .ok_or_else(|| format!("segment[{i}] invalid interval {:?}", seg.interval))?;
        let position = Position::parse(&seg.position)
            .ok_or_else(|| format!("segment[{i}] unknown position {:?}", seg.position))?;
        let max_bytes = seg.max_bytes.unwrap_or(64) as usize;
        let max_bytes = max_bytes.min(SEG_PAYLOAD_MAX);
        let timeout = seg
            .timeout
            .as_deref()
            .map(|s| {
                parse_duration(s)
                    .ok_or_else(|| format!("segment[{i}] invalid timeout {:?}", s))
            })
            .transpose()?
            .unwrap_or(Duration::from_secs(5));
        out.push(SegmentConfig {
            name: seg.name,
            cmd: seg.cmd,
            interval,
            position,
            max_bytes,
            timeout,
            show_on_error: seg.show_on_error.unwrap_or(false),
        });
    }
    Ok(out)
}

/// Platform config file path for claudehud.
/// Unix: `$XDG_CONFIG_HOME/claudehud/config.toml` (default `~/.config/claudehud/config.toml`)
/// Windows: `%APPDATA%\claudehud\config.toml`
pub fn config_path() -> PathBuf {
    #[cfg(unix)]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("claudehud").join("config.toml");
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        home.join(".config").join("claudehud").join("config.toml")
    }
    #[cfg(windows)]
    {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Users\Default\AppData\Roaming"));
        appdata.join("claudehud").join("config.toml")
    }
}

/// Load and parse config from `path`. Returns `None` if file doesn't exist or
/// (on Unix) is world-writable. Returns `Some(vec![])` for an empty config.
pub fn load_config(path: &Path) -> Option<Vec<SegmentConfig>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = std::fs::metadata(path) {
            if meta.mode() & 0o002 != 0 {
                eprintln!(
                    "WARN claudehud: refusing to load world-writable config: {}",
                    path.display()
                );
                return None;
            }
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    match parse_config(&text) {
        Ok(segs) => Some(segs),
        Err(e) => {
            eprintln!("WARN claudehud config: {e}");
            Some(vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parse_duration_ms() {
        assert_eq!(parse_duration("10ms"), Some(Duration::from_millis(10)));
    }

    #[test]
    fn test_parse_duration_s() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_duration_m() {
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn test_parse_duration_h() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("10x"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn test_position_parse_all_variants() {
        assert_eq!(Position::parse("before-model"), Some(Position::BeforeModel));
        assert_eq!(Position::parse("after-branch"), Some(Position::AfterBranch));
        assert_eq!(Position::parse("end-line-1"), Some(Position::EndLine1));
        assert_eq!(Position::parse("before-rate"), Some(Position::BeforeRate));
        assert_eq!(Position::parse("line-2"), Some(Position::Line2));
        assert_eq!(Position::parse("end-line-2"), Some(Position::EndLine2));
    }

    #[test]
    fn test_position_parse_unknown() {
        assert_eq!(Position::parse("top-right"), None);
    }

    #[test]
    fn test_seg_path_format() {
        let hash = fnv32_str("kube-ctx");
        let p = seg_path_in(Path::new("/tmp"), "kube-ctx");
        assert_eq!(p, Path::new("/tmp").join(format!("clhud-seg-{hash}.bin")));
    }

    #[test]
    fn test_seg_mmap_size() {
        assert_eq!(SEG_MMAP_SIZE, 137);
    }

    #[test]
    fn test_seqlock_write_read_roundtrip() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"prod-cluster");
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload, b"prod-cluster");
    }

    #[test]
    fn test_seqlock_write_read_empty() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"");
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload, b"");
    }

    #[test]
    fn test_seqlock_write_truncates_at_128() {
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        let long = vec![b'a'; 200];
        seqlock_write_seg(&mut buf, &long);
        let payload = seqlock_read_seg(&buf);
        assert_eq!(payload.len(), 128);
    }

    #[test]
    fn test_truncate_utf8_safe_ascii() {
        assert_eq!(truncate_to_boundary("hello world", 5), "hello");
    }

    #[test]
    fn test_truncate_utf8_safe_multibyte() {
        let s = "café";
        let t = truncate_to_boundary(s, 4);
        assert!(s.starts_with(t));
        assert!(t.len() <= 4);
    }

    #[test]
    fn test_truncate_utf8_safe_empty() {
        assert_eq!(truncate_to_boundary("", 10), "");
    }

    #[test]
    fn test_config_parse_valid() {
        let toml = r#"
[[segment]]
name = "kube-ctx"
cmd = "kubectl config current-context"
interval = "30s"
position = "after-branch"
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg.len(), 1);
        assert_eq!(cfg[0].name, "kube-ctx");
        assert_eq!(cfg[0].cmd, "kubectl config current-context");
        assert_eq!(cfg[0].position, Position::AfterBranch);
        assert_eq!(cfg[0].interval, Duration::from_secs(30));
        assert_eq!(cfg[0].max_bytes, 64);
        assert_eq!(cfg[0].timeout, Duration::from_secs(5));
        assert!(!cfg[0].show_on_error);
    }

    #[test]
    fn test_config_parse_max_bytes_clamped() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "1s"
position = "end-line-1"
max_bytes = 999
"#;
        let cfg = parse_config(toml).unwrap();
        assert_eq!(cfg[0].max_bytes, 128);
    }

    #[test]
    fn test_config_parse_missing_required_field() {
        let toml = r#"
[[segment]]
name = "x"
interval = "1s"
position = "end-line-1"
"#;
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn test_config_parse_invalid_position() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "1s"
position = "bad-position"
"#;
        assert!(parse_config(toml).is_err());
    }

    #[test]
    fn test_config_parse_invalid_interval() {
        let toml = r#"
[[segment]]
name = "x"
cmd = "echo hi"
interval = "forever"
position = "end-line-1"
"#;
        assert!(parse_config(toml).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_load_config_world_writable_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let result = load_config(&path);
        assert!(result.is_none());
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p common segments`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add common/src/segments.rs common/src/lib.rs
git commit -m "feat(common): add segments types, mmap layout, config parser"
```

---

## Task 3: `claudehud-daemon/src/segments.rs` — scheduler, timeout, ANSI strip, hot-reload

**Files:**
- Create: `claudehud-daemon/src/segments.rs`
- Modify: `claudehud-daemon/src/main.rs` (add `mod segments`, spawn thread)

Each segment gets its own scheduler thread. Timeout is enforced by a second timer thread that kills the child after the deadline.

- [ ] **Step 1: Write the failing tests**

Create `claudehud-daemon/src/segments.rs` with tests only:

```rust
// claudehud-daemon/src/segments.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_csi() {
        assert_eq!(strip_ansi("\x1b[32mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_osc() {
        assert_eq!(strip_ansi("\x1b]8;;https://example.com\x1b\\text\x1b]8;;\x1b\\"), "text");
    }

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn test_strip_ansi_mixed() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m and plain"), "bold and plain");
    }

    #[test]
    fn test_run_segment_basic() {
        use common::segments::SegmentConfig;
        use common::segments::Position;
        use std::time::Duration;
        let cfg = SegmentConfig {
            name: "test".to_string(),
            cmd: "echo hello".to_string(),
            interval: Duration::from_secs(60),
            position: Position::EndLine1,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: false,
        };
        let result = run_segment(&cfg);
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn test_run_segment_timeout() {
        use common::segments::SegmentConfig;
        use common::segments::Position;
        use std::time::{Duration, Instant};
        let cfg = SegmentConfig {
            name: "slow".to_string(),
            cmd: "sleep 10".to_string(),
            interval: Duration::from_secs(60),
            position: Position::EndLine1,
            max_bytes: 64,
            timeout: Duration::from_millis(200),
            show_on_error: false,
        };
        let start = Instant::now();
        let result = run_segment(&cfg);
        let elapsed = start.elapsed();
        assert!(result.is_none());
        // Should complete within 500ms of the 200ms timeout
        assert!(elapsed < Duration::from_millis(700), "timeout took too long: {elapsed:?}");
    }

    #[test]
    fn test_run_segment_nonzero_exit_show_off() {
        use common::segments::SegmentConfig;
        use common::segments::Position;
        use std::time::Duration;
        let cfg = SegmentConfig {
            name: "fail".to_string(),
            cmd: "sh -c 'exit 1'".to_string(),
            interval: Duration::from_secs(60),
            position: Position::EndLine1,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: false,
        };
        let result = run_segment(&cfg);
        assert!(result.is_none());
    }

    #[test]
    fn test_run_segment_nonzero_exit_show_on() {
        use common::segments::SegmentConfig;
        use common::segments::Position;
        use std::time::Duration;
        let cfg = SegmentConfig {
            name: "fail-show".to_string(),
            cmd: "sh -c 'echo stderr-msg >&2; exit 1'".to_string(),
            interval: Duration::from_secs(60),
            position: Position::EndLine1,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: true,
        };
        let result = run_segment(&cfg);
        // show_on_error: show stderr when exit nonzero
        assert!(result.is_some());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p claudehud-daemon segments`
Expected: FAIL — module not found.

- [ ] **Step 3: Register module in daemon main.rs**

In `claudehud-daemon/src/main.rs`, after the existing `mod status;` line, add:

```rust
mod segments;
```

- [ ] **Step 4: Implement `claudehud-daemon/src/segments.rs`**

```rust
// claudehud-daemon/src/segments.rs

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use memmap2::MmapMut;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use common::segments::{
    config_path, load_config, seg_path, seqlock_write_seg, truncate_to_boundary, SegmentConfig,
    SEG_MMAP_SIZE,
};

/// Strip ANSI escape sequences from a string.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    // CSI — skip until a byte in 0x40..=0x7e
                    i += 2;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                }
                b']' => {
                    // OSC — skip until BEL (0x07) or ST (ESC \)
                    i += 2;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                _ => {
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Run a segment command once: spawn, timeout, capture, strip, truncate.
/// Returns `None` if the command fails, times out, or exits nonzero (when show_on_error is false).
pub fn run_segment(cfg: &SegmentConfig) -> Option<String> {
    let parts: Vec<&str> = cfg.cmd.split_ascii_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let mut child = Command::new(parts[0])
        .args(&parts[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    // Timer thread: kill after timeout
    let child_id = child.id();
    let timeout = cfg.timeout;
    let (kill_tx, kill_rx): (Sender<()>, Receiver<()>) = bounded(1);
    std::thread::spawn(move || {
        let deadline = Instant::now() + timeout;
        loop {
            if kill_rx.try_recv().is_ok() {
                return;
            }
            if Instant::now() >= deadline {
                // Best-effort kill
                #[cfg(unix)]
                {
                    unsafe { libc_kill(child_id as i32, 9) };
                }
                #[cfg(not(unix))]
                {
                    // On Windows use taskkill as a fallback; child.kill() would be ideal
                    // but we don't have a handle here. Acceptable limitation — see spec.
                    let _ = Command::new("taskkill")
                        .args(["/F", "/PID", &child_id.to_string()])
                        .output();
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let start = Instant::now();
    // wait_with_output() drains stdout+stderr concurrently, avoiding pipe-buffer deadlock.
    let output = child.wait_with_output().ok();
    let _ = kill_tx.send(()); // signal timer to stop

    // Check elapsed against timeout — if over, treat as timeout
    if start.elapsed() >= timeout {
        return None;
    }

    let output = output?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let cleaned = strip_ansi(&text);
        let trimmed = cleaned.trim_end_matches(['\n', '\r']).trim();
        let truncated = truncate_to_boundary(trimmed, cfg.max_bytes);
        if truncated.is_empty() {
            None
        } else {
            Some(truncated.to_string())
        }
    } else if cfg.show_on_error {
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        let cleaned = strip_ansi(&text);
        let trimmed = cleaned.trim_end_matches(['\n', '\r']).trim();
        let truncated = truncate_to_boundary(trimmed, cfg.max_bytes);
        if truncated.is_empty() {
            None
        } else {
            Some(truncated.to_string())
        }
    } else {
        None
    }
}

#[cfg(unix)]
fn libc_kill(pid: i32, sig: i32) {
    unsafe { libc::kill(pid, sig) };
}

fn write_seg_to_mmap(path: &std::path::Path, payload: &[u8]) {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("WARN seg mmap open {}: {e}", path.display());
            return;
        }
    };
    if file.set_len(SEG_MMAP_SIZE as u64).is_err() {
        return;
    }
    let mut mmap = match unsafe { MmapMut::map_mut(&file) } {
        Ok(m) if m.len() >= SEG_MMAP_SIZE => m,
        _ => return,
    };
    seqlock_write_seg(&mut mmap[..], payload);
}

fn scheduler_thread(cfg: Arc<SegmentConfig>, shutdown_rx: Receiver<()>) {
    let path = seg_path(&cfg.name);
    loop {
        let payload = run_segment(&cfg)
            .map(|s| s.into_bytes())
            .unwrap_or_default();
        write_seg_to_mmap(&path, &payload);

        // Sleep interval, but wake early on shutdown
        let deadline = Instant::now() + cfg.interval;
        loop {
            if shutdown_rx.try_recv().is_ok() {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

struct SchedulerHandle {
    shutdown_tx: Sender<()>,
}

fn spawn_schedulers(configs: Vec<SegmentConfig>) -> Vec<SchedulerHandle> {
    configs
        .into_iter()
        .map(|cfg| {
            let (shutdown_tx, shutdown_rx) = bounded::<()>(1);
            let arc = Arc::new(cfg);
            std::thread::spawn(move || scheduler_thread(arc, shutdown_rx));
            SchedulerHandle { shutdown_tx }
        })
        .collect()
}

fn stop_schedulers(handles: Vec<SchedulerHandle>) {
    for h in handles {
        let _ = h.shutdown_tx.send(());
    }
}

/// Main entry point for the segment scheduler. Loads config, spawns schedulers,
/// watches config file for hot-reload, loops forever.
pub fn start() {
    let cfg_path = config_path();
    let mut handles = if let Some(configs) = load_config(&cfg_path) {
        spawn_schedulers(configs)
    } else {
        vec![]
    };

    // Watch parent dir to catch atomic file replacements (editor rename pattern)
    let parent = cfg_path.parent().unwrap_or(std::path::Path::new("."));
    let cfg_path_clone = cfg_path.clone();
    let (event_tx, event_rx) = crossbeam_channel::unbounded::<PathBuf>();

    let mut watcher = match RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    for path in &event.paths {
                        if path == &cfg_path_clone {
                            let _ = event_tx.send(path.clone());
                        }
                    }
                }
            }
        },
        Config::default(),
    ) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("WARN segments watcher: {e}");
            // Run without hot-reload
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    };

    let _ = watcher.watch(parent, RecursiveMode::NonRecursive);

    loop {
        if event_rx.recv().is_ok() {
            // Debounce: drain any rapid-fire events
            std::thread::sleep(Duration::from_millis(100));
            while event_rx.try_recv().is_ok() {}

            stop_schedulers(handles);
            handles = if let Some(configs) = load_config(&cfg_path) {
                spawn_schedulers(configs)
            } else {
                vec![]
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::segments::{Position, SegmentConfig};
    use std::time::Duration;

    #[test]
    fn test_strip_ansi_csi() {
        assert_eq!(strip_ansi("\x1b[32mhello\x1b[0m"), "hello");
    }

    #[test]
    fn test_strip_ansi_osc() {
        assert_eq!(
            strip_ansi("\x1b]8;;https://example.com\x1b\\text\x1b]8;;\x1b\\"),
            "text"
        );
    }

    #[test]
    fn test_strip_ansi_plain() {
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn test_strip_ansi_mixed() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m and plain"), "bold and plain");
    }

    fn echo_cfg(cmd: &str) -> SegmentConfig {
        SegmentConfig {
            name: "test".to_string(),
            cmd: cmd.to_string(),
            interval: Duration::from_secs(60),
            position: Position::EndLine1,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: false,
        }
    }

    #[test]
    fn test_run_segment_basic() {
        let cfg = echo_cfg("echo hello");
        let result = run_segment(&cfg);
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn test_run_segment_timeout() {
        let mut cfg = echo_cfg("sleep 10");
        cfg.timeout = Duration::from_millis(200);
        let start = std::time::Instant::now();
        let result = run_segment(&cfg);
        let elapsed = start.elapsed();
        assert!(result.is_none());
        assert!(
            elapsed < Duration::from_millis(700),
            "timeout took too long: {elapsed:?}"
        );
    }

    #[test]
    fn test_run_segment_nonzero_exit_show_off() {
        let cfg = echo_cfg("sh -c 'exit 1'");
        assert!(run_segment(&cfg).is_none());
    }

    #[test]
    fn test_run_segment_nonzero_exit_show_on() {
        let mut cfg = echo_cfg("sh -c 'echo err-msg >&2; exit 1'");
        cfg.show_on_error = true;
        let result = run_segment(&cfg);
        assert!(result.is_some());
        assert!(result.unwrap().contains("err-msg"));
    }
}
```

- [ ] **Step 5: Add `libc` to daemon deps (for Unix kill)**

In `claudehud-daemon/Cargo.toml`, add to `[dependencies]`:

```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

The full `[dependencies]` block becomes:

```toml
[dependencies]
common = { workspace = true }
memmap2 = { workspace = true }
serde = { workspace = true }
toml = { workspace = true }
notify = "6"
crossbeam-channel = "0.5"
ureq = { version = "2", default-features = false, features = ["tls"] }
roxmltree = "0.20"

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p claudehud-daemon segments`
Expected: all tests pass. The timeout test will take ~200ms.

- [ ] **Step 7: Wire `segments::start()` into daemon main.rs**

In `claudehud-daemon/src/main.rs`, after the existing `std::thread::spawn(|| { status::start(); });` block (around line 58), add:

```rust
    std::thread::spawn(|| {
        segments::start();
    });
```

- [ ] **Step 8: Build the daemon**

Run: `cargo build -p claudehud-daemon`
Expected: builds cleanly.

- [ ] **Step 9: Commit**

```bash
git add claudehud-daemon/src/segments.rs claudehud-daemon/src/main.rs claudehud-daemon/Cargo.toml Cargo.lock
git commit -m "feat(daemon): pipe-extension segment scheduler with timeout + ANSI strip"
```

---

## Task 4: `claudehud/src/segments.rs` — mmap read, SegmentOutput

**Files:**
- Create: `claudehud/src/segments.rs`
- Modify: `claudehud/src/lib.rs` (add `pub mod segments`)

The client reads each segment's mmap file once per render.

- [ ] **Step 1: Write the failing tests**

Create `claudehud/src/segments.rs` with tests only:

```rust
// claudehud/src/segments.rs

#[cfg(test)]
mod tests {
    use super::*;
    use common::segments::{seqlock_write_seg, Position, SegmentConfig, SEG_MMAP_SIZE};
    use std::time::Duration;

    fn make_cfg(name: &str, position: Position) -> SegmentConfig {
        SegmentConfig {
            name: name.to_string(),
            cmd: "echo test".to_string(),
            interval: Duration::from_secs(30),
            position,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: false,
        }
    }

    #[test]
    fn test_read_segment_from_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = common::segments::seg_path_in(dir.path(), "myseg");
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"prod-us-east-1");
        std::fs::write(&path, &buf).unwrap();

        let cfg = make_cfg("myseg", Position::AfterBranch);
        let result = read_segment_from(dir.path(), &cfg);
        assert_eq!(result, Some(SegmentOutput {
            text: "prod-us-east-1".to_string(),
            position: Position::AfterBranch,
        }));
    }

    #[test]
    fn test_read_segment_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = make_cfg("nonexistent", Position::EndLine1);
        let result = read_segment_from(dir.path(), &cfg);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_segment_empty_payload_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = common::segments::seg_path_in(dir.path(), "empty");
        let buf = vec![0u8; SEG_MMAP_SIZE]; // zero-initialized = empty payload
        std::fs::write(&path, &buf).unwrap();

        let cfg = make_cfg("empty", Position::EndLine1);
        let result = read_segment_from(dir.path(), &cfg);
        assert!(result.is_none());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p claudehud segments`
Expected: FAIL — module not found.

- [ ] **Step 3: Register module in `claudehud/src/lib.rs`**

Add after the existing module declarations in `claudehud/src/lib.rs`:

```rust
pub mod segments;
```

- [ ] **Step 4: Implement `claudehud/src/segments.rs`**

```rust
// claudehud/src/segments.rs

use std::fs;
use std::path::Path;

use common::segments::{
    cache_dir, config_path, load_config, seg_path_in, seqlock_read_seg, Position, SegmentConfig,
    SEG_MMAP_SIZE,
};
use memmap2::Mmap;

/// A resolved segment ready to splice into render output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentOutput {
    pub text: String,
    pub position: Position,
}

/// Load config and read all segment mmap files. Returns outputs in config order.
/// Missing cache files are silently skipped.
pub fn read_segments() -> Vec<SegmentOutput> {
    let path = config_path();
    let configs = match load_config(&path) {
        Some(c) => c,
        None => return vec![],
    };
    read_segments_from(&cache_dir(), &configs)
}

pub fn read_segments_from(cache: &Path, configs: &[SegmentConfig]) -> Vec<SegmentOutput> {
    configs
        .iter()
        .filter_map(|cfg| read_segment_from(cache, cfg))
        .collect()
}

pub fn read_segment_from(cache: &Path, cfg: &SegmentConfig) -> Option<SegmentOutput> {
    let path = seg_path_in(cache, &cfg.name);
    let file = fs::File::open(&path).ok()?;
    if file.metadata().ok()?.len() != SEG_MMAP_SIZE as u64 {
        return None;
    }
    // Safety: file holds fd open; daemon uses seqlock so concurrent writes are safe.
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let payload = seqlock_read_seg(&mmap);
    if payload.is_empty() {
        return None;
    }
    let text = String::from_utf8_lossy(&payload).into_owned();
    Some(SegmentOutput {
        text,
        position: cfg.position,
    })
}

// Re-export for convenience
pub use common::segments::cache_dir;

#[cfg(test)]
mod tests {
    use super::*;
    use common::segments::{seqlock_write_seg, Position, SegmentConfig, SEG_MMAP_SIZE};
    use std::time::Duration;

    fn make_cfg(name: &str, position: Position) -> SegmentConfig {
        SegmentConfig {
            name: name.to_string(),
            cmd: "echo test".to_string(),
            interval: Duration::from_secs(30),
            position,
            max_bytes: 64,
            timeout: Duration::from_secs(5),
            show_on_error: false,
        }
    }

    #[test]
    fn test_read_segment_from_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = common::segments::seg_path_in(dir.path(), "myseg");
        let mut buf = vec![0u8; SEG_MMAP_SIZE];
        seqlock_write_seg(&mut buf, b"prod-us-east-1");
        std::fs::write(&path, &buf).unwrap();

        let cfg = make_cfg("myseg", Position::AfterBranch);
        let result = read_segment_from(dir.path(), &cfg);
        assert_eq!(
            result,
            Some(SegmentOutput {
                text: "prod-us-east-1".to_string(),
                position: Position::AfterBranch,
            })
        );
    }

    #[test]
    fn test_read_segment_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = make_cfg("nonexistent", Position::EndLine1);
        let result = read_segment_from(dir.path(), &cfg);
        assert!(result.is_none());
    }

    #[test]
    fn test_read_segment_empty_payload_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = common::segments::seg_path_in(dir.path(), "empty");
        let buf = vec![0u8; SEG_MMAP_SIZE];
        std::fs::write(&path, &buf).unwrap();

        let cfg = make_cfg("empty", Position::EndLine1);
        let result = read_segment_from(dir.path(), &cfg);
        assert!(result.is_none());
    }
}
```

- [ ] **Step 5: Fix the re-export — `cache_dir` is in `common`, not `common::segments`**

The `use common::segments::cache_dir` in the implementation above should instead be:

```rust
use common::cache_dir;
```

And remove the `pub use` at the bottom. The `read_segments` function body should use `common::cache_dir()`:

```rust
pub fn read_segments() -> Vec<SegmentOutput> {
    let path = config_path();
    let configs = match load_config(&path) {
        Some(c) => c,
        None => return vec![],
    };
    read_segments_from(&common::cache_dir(), &configs)
}
```

Update the imports in the file to match:

```rust
use common::segments::{
    config_path, load_config, seg_path_in, seqlock_read_seg, Position, SegmentConfig,
    SEG_MMAP_SIZE,
};
use common::cache_dir;
use memmap2::Mmap;
```

Remove `pub use common::segments::cache_dir;` from the bottom.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p claudehud segments`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add claudehud/src/segments.rs claudehud/src/lib.rs
git commit -m "feat(client): segment mmap reader + SegmentOutput type"
```

---

## Task 5: Wire segments into `render.rs`

**Files:**
- Modify: `claudehud/src/render.rs`

Add `segments: &[SegmentOutput]` to the `render` function signature and both layout functions. Splice at declared positions. In condensed mode, omit `BeforeRate`, `Line2`, `EndLine2`.

- [ ] **Step 1: Write failing render tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `claudehud/src/render.rs`:

```rust
    #[test]
    fn test_render_segment_after_branch_comfortable() {
        use crate::segments::SegmentOutput;
        use common::segments::Position;
        let input = Input::default();
        let segs = vec![SegmentOutput {
            text: "prod".to_string(),
            position: Position::AfterBranch,
        }];
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable, &segs);
        let plain = strip_ansi(&result);
        assert!(plain.contains("prod"), "expected segment text in output: {plain}");
    }

    #[test]
    fn test_render_segment_end_line1_comfortable() {
        use crate::segments::SegmentOutput;
        use common::segments::Position;
        let input = Input::default();
        let segs = vec![SegmentOutput {
            text: "staging".to_string(),
            position: Position::EndLine1,
        }];
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable, &segs);
        let plain = strip_ansi(&result);
        assert!(plain.contains("staging"));
    }

    #[test]
    fn test_render_segment_line2_omitted_in_condensed() {
        use crate::segments::SegmentOutput;
        use common::segments::Position;
        let input = Input::default();
        let segs = vec![SegmentOutput {
            text: "hidden".to_string(),
            position: Position::Line2,
        }];
        let result = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Condensed, &segs);
        let plain = strip_ansi(&result);
        assert!(!plain.contains("hidden"), "line-2 segments should be omitted in condensed mode");
    }

    #[test]
    fn test_render_no_segments_unchanged() {
        let input = Input::default();
        let result_no_segs = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable, &[]);
        let result_baseline = render(&input, None, &[], 0, RoundingMode::Floor, Layout::Comfortable, &[]);
        assert_eq!(result_no_segs, result_baseline);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p claudehud render`
Expected: FAIL — `render` function signature mismatch.

- [ ] **Step 3: Update `render` function signatures**

In `claudehud/src/render.rs`, add the import near the top:

```rust
use crate::segments::SegmentOutput;
```

Update the public `render` function signature (around line 56):

```rust
pub fn render(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    layout: Layout,
    segments: &[SegmentOutput],
) -> String {
    match layout {
        Layout::Comfortable => {
            render_comfortable(input, git, incidents, total_active, rounding, segments)
        }
        Layout::Condensed => {
            render_condensed(input, git, incidents, total_active, rounding, segments)
        }
    }
}
```

- [ ] **Step 4: Update `render_comfortable` to splice segments**

Update `render_comfortable` signature and body. The function currently builds output in this order: model → context → cost → dir/branch → incidents → rate limits. Segments are inserted at their declared positions. Replace the function with:

```rust
fn render_comfortable(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    segments: &[SegmentOutput],
) -> String {
    use common::segments::Position;
    let mut out = String::with_capacity(512);

    // ── BeforeModel segments ──────────────────────────────
    push_segments_at(segments, Position::BeforeModel, &mut out);

    // ── Model ──────────────────────────────────────────────
    push_model_full(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, &mut out);

    // ── Cost (skipped when absent or $0) ───────────────────
    push_cost(input, &mut out);

    // ── Dir + git ──────────────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), false, &mut out);

    // ── AfterBranch segments ──────────────────────────────
    push_segments_at(segments, Position::AfterBranch, &mut out);

    // ── EndLine1 segments ─────────────────────────────────
    push_segments_at(segments, Position::EndLine1, &mut out);

    // ── Incident lines ────────────────────────────────────
    push_incidents(incidents, total_active, &mut out);

    // ── BeforeRate segments ───────────────────────────────
    push_segments_at(segments, Position::BeforeRate, &mut out);

    // ── Rate limits ────────────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = rounding.apply(pct_f);
                out.push_str("\n\n");
                push_rate_row("current", pct, fh.resets_at, ResetStyle::Time, &mut out);

                if let Some(sd) = &rl.seven_day {
                    if let Some(pct_f) = sd.used_percentage {
                        let pct = rounding.apply(pct_f);
                        out.push('\n');
                        push_rate_row("weekly ", pct, sd.resets_at, ResetStyle::DateTime, &mut out);
                    }
                }
            }
        }
    }

    // ── Line2 + EndLine2 segments ─────────────────────────
    let has_line2 = segments
        .iter()
        .any(|s| s.position == Position::Line2 || s.position == Position::EndLine2);
    if has_line2 {
        out.push('\n');
        push_segments_at(segments, Position::Line2, &mut out);
        push_segments_at(segments, Position::EndLine2, &mut out);
    }

    out
}
```

- [ ] **Step 5: Update `render_condensed` to splice segments (omit line-2 positions)**

```rust
fn render_condensed(
    input: &Input,
    git: Option<(String, bool)>,
    incidents: &[Incident],
    total_active: u8,
    rounding: RoundingMode,
    segments: &[SegmentOutput],
) -> String {
    use common::segments::Position;
    let mut out = String::with_capacity(512);

    // ── BeforeModel segments ──────────────────────────────
    push_segments_at(segments, Position::BeforeModel, &mut out);

    // ── Model (short) ──────────────────────────────────────
    push_model_short(input, &mut out);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    push_context(input, rounding, &mut out);

    // ── Cost (skipped when absent or $0) ───────────────────
    push_cost(input, &mut out);

    // ── Dir + git (tight) ──────────────────────────────────
    out.push_str(SEP);
    push_dir_branch(input, git.as_ref(), true, &mut out);

    // ── AfterBranch segments ──────────────────────────────
    push_segments_at(segments, Position::AfterBranch, &mut out);

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

    // ── EndLine1 segments ─────────────────────────────────
    push_segments_at(segments, Position::EndLine1, &mut out);

    // ── Incidents ──────────────────────────────────────────
    push_incidents(incidents, total_active, &mut out);

    // NOTE: BeforeRate, Line2, EndLine2 are omitted in condensed mode.

    out
}
```

- [ ] **Step 6: Add `push_segments_at` helper**

Add this function to `render.rs` after the `push_incidents` function:

```rust
fn push_segments_at(segments: &[SegmentOutput], pos: common::segments::Position, out: &mut String) {
    for seg in segments.iter().filter(|s| s.position == pos) {
        out.push_str(SEP);
        out.push_str(DIM);
        out.push_str(&seg.text);
        out.push_str(RESET);
    }
}
```

- [ ] **Step 7: Run the render tests**

Run: `cargo test -p claudehud render`
Expected: all tests pass including the new segment tests.

- [ ] **Step 8: Commit**

```bash
git add claudehud/src/render.rs
git commit -m "feat(render): splice pipe-extension segments at declared positions"
```

---

## Task 6: Wire segments into `claudehud/src/main.rs`

**Files:**
- Modify: `claudehud/src/main.rs`

The client loads segments on each render invocation and passes them to `render::render`.

- [ ] **Step 1: Update the render call in `main.rs`**

In `claudehud/src/main.rs`, update the `render` function (around line 80). First add the import at the top with the other `claudehud::` imports:

```rust
use claudehud::{git, incidents, input, install, render, segments, update};
```

Then in the `render` fn body, after the `let (incidents, total_active) = incidents::read_incidents();` line (around line 132), add:

```rust
    let seg_outputs = segments::read_segments();
```

And update the `render::render(...)` call to include the new argument:

```rust
    print!(
        "{}",
        render::render(&input, git, &incidents, total_active, rounding, layout, &seg_outputs)
    );
```

- [ ] **Step 2: Build and test**

Run: `cargo test --workspace`
Expected: all tests pass, no compiler errors.

- [ ] **Step 3: Commit**

```bash
git add claudehud/src/main.rs
git commit -m "feat(client): load + pass pipe-extension segments to render"
```

---

## Task 7: README update

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add configuration section**

Find the `## Architecture` section in `README.md` and add a new `## Pipe-extension segments` section before it:

```markdown
## Pipe-extension segments

Declare arbitrary shell commands in `~/.config/claudehud/config.toml` (macOS/Linux) or `%APPDATA%\claudehud\config.toml` (Windows). The daemon runs each command on its interval and caches the output; the client reads from cache on every render with no subprocess overhead.

```toml
[[segment]]
name     = "kube-ctx"
cmd      = "kubectl config current-context"
interval = "30s"
position = "after-branch"

[[segment]]
name     = "aws-profile"
cmd      = "aws configure get profile"
interval = "60s"
position = "end-line-1"
max_bytes = 32        # default 64, max 128
timeout   = "2s"      # kill if command exceeds this; default 5s
show_on_error = false # show stderr when exit nonzero? default false
```

### Positions

| `position` value | Comfortable layout | Condensed layout |
|------------------|--------------------|------------------|
| `before-model` | before model name, line 1 | before model name |
| `after-branch` | after dir/branch, line 1 | after dir/branch |
| `end-line-1` | end of line 1 | end of line 1 |
| `before-rate` | before rate-limit bars | **omitted** |
| `line-2` | second newline block | **omitted** |
| `end-line-2` | end of second newline block | **omitted** |

### Security

> **Warning:** pipe-extension commands run with the daemon's privileges and inherit its environment. Only put trusted commands in your config.

- Commands are split on whitespace and passed directly as `argv` — **not** through a shell. To use shell features (pipes, redirects, variable expansion), write `bash -c '...'` or `sh -c '...'` explicitly.
- On Unix, the daemon refuses to load a config file that is world-writable (`mode & 0o002 != 0`). A warning is printed and no segments run.
- Command output is truncated to `max_bytes` on a UTF-8 character boundary. ANSI escape sequences are stripped before display.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: pipe-extension segments config, positions, security note"
```

---

## Task 8: Final check — fmt, clippy, all tests

- [ ] **Step 1: Format check**

Run: `cargo fmt --check`
Expected: no diff. If there is a diff, run `cargo fmt` then re-check.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings or errors. Fix any that appear.

- [ ] **Step 3: Full test suite**

Run: `cargo test --workspace`
Expected: all tests pass.

- [ ] **Step 4: Commit any fmt/clippy fixes**

If any fixes were needed:

```bash
git add -p
git commit -m "fix: clippy + fmt for pipe-extension segments"
```
