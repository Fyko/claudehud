# cc-statusline Rust Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite the Claude Code statusline bash script as a Rust binary + mmap-backed daemon for sub-millisecond prompt render times.

**Architecture:** A `cc-statusline` client reads JSON from stdin, queries a per-path mmap file for git status, and renders the statusline. A `cc-statusline-daemon` watches `.git/index` and `.git/HEAD` via FSEvents/inotify and writes git state to per-path mmap files using a seqlock. IPC uses only file I/O — no sockets.

**Tech Stack:** Rust 2021, `serde`/`serde_json`, `memmap2`, `libc`, `notify` (daemon only), `crossbeam-channel` (daemon only)

---

## File Map

| File | Responsibility |
|---|---|
| `Cargo.toml` | Workspace root — replaces existing file |
| `common/Cargo.toml` | Shared lib, no external deps |
| `common/src/lib.rs` | FNV-1a hash, mmap path helpers, seqlock read, `find_git_root` |
| `cc-statusline/Cargo.toml` | Client binary deps |
| `cc-statusline/src/main.rs` | stdin → parse → render → stdout |
| `cc-statusline/src/input.rs` | Serde structs for Claude Code JSON |
| `cc-statusline/src/fmt.rs` | ANSI color consts, token formatter, progress bar |
| `cc-statusline/src/time.rs` | ISO 8601 parser, duration formatter, reset time formatter |
| `cc-statusline/src/render.rs` | Assembles final statusline string |
| `cc-statusline/src/git.rs` | mmap fast path + git subprocess fallback + registration |
| `cc-statusline-daemon/Cargo.toml` | Daemon binary deps |
| `cc-statusline-daemon/src/main.rs` | Spawn registrar thread, run watcher loop |
| `cc-statusline-daemon/src/registrar.rs` | Watch `/tmp/ccsl-watch/` for new path registrations |
| `cc-statusline-daemon/src/watcher.rs` | Watch `.git/index`+`HEAD`, dispatch cache updates |
| `cc-statusline-daemon/src/cache.rs` | Run git, seqlock-write result to mmap file |

---

### Task 1: Workspace scaffold

**Files:**
- Modify: `Cargo.toml` (replace with workspace manifest)
- Create: `common/Cargo.toml`, `common/src/lib.rs` (placeholder)
- Create: `cc-statusline/Cargo.toml`
- Create: `cc-statusline-daemon/Cargo.toml`
- Move: `src/` → `cc-statusline/src/` (keep hello world for now)

- [ ] **Step 1: Replace root Cargo.toml with workspace manifest**

```toml
# Cargo.toml
[workspace]
members = ["common", "cc-statusline", "cc-statusline-daemon"]
resolver = "2"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

- [ ] **Step 2: Create common crate**

```toml
# common/Cargo.toml
[package]
name = "common"
version = "0.1.0"
edition = "2021"
```

```rust
// common/src/lib.rs
// placeholder — filled in Task 2
```

- [ ] **Step 3: Create cc-statusline crate**

Move `src/` to `cc-statusline/src/`:
```bash
mkdir -p cc-statusline/src
mv src/main.rs cc-statusline/src/main.rs
rmdir src
```

```toml
# cc-statusline/Cargo.toml
[package]
name = "cc-statusline"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
memmap2 = "0.9"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
libc = "0.2"
```

- [ ] **Step 4: Create cc-statusline-daemon crate**

```bash
mkdir -p cc-statusline-daemon/src
```

```toml
# cc-statusline-daemon/Cargo.toml
[package]
name = "cc-statusline-daemon"
version = "0.1.0"
edition = "2021"

[dependencies]
common = { path = "../common" }
memmap2 = "0.9"
notify = "6"
crossbeam-channel = "0.5"
libc = "0.2"
```

```rust
// cc-statusline-daemon/src/main.rs
fn main() {}
```

- [ ] **Step 5: Verify workspace compiles**

```bash
cargo check --workspace
```
Expected: warnings about unused code, zero errors.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "chore: scaffold cargo workspace with three crates"
```

---

### Task 2: common — FNV hash, path helpers, seqlock read, find_git_root

**Files:**
- Modify: `common/src/lib.rs`

- [ ] **Step 1: Write failing tests**

```rust
// common/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_hash_path_stable() {
        let a = hash_path(Path::new("/home/user/project"));
        let b = hash_path(Path::new("/home/user/project"));
        assert_eq!(a, b);
    }

    #[test]
    fn test_hash_path_distinct() {
        let a = hash_path(Path::new("/home/user/project"));
        let b = hash_path(Path::new("/home/user/other"));
        assert_ne!(a, b);
    }

    #[test]
    fn test_mmap_path_format() {
        let p = mmap_path(12345);
        assert_eq!(p.to_str().unwrap(), "/tmp/ccsl-12345.bin");
    }

    #[test]
    fn test_watch_path_format() {
        let p = watch_path(12345);
        assert_eq!(p.to_str().unwrap(), "/tmp/ccsl-watch/12345");
    }

    #[test]
    fn test_seqlock_read_stable() {
        let mut buf = [0u8; MMAP_SIZE];
        // seq=2 (even, stable), dirty=1, branch="main"
        buf[0..8].copy_from_slice(&2u64.to_le_bytes());
        buf[8] = 1;
        buf[9] = 4;
        buf[10..14].copy_from_slice(b"main");
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(dirty);
    }

    #[test]
    fn test_find_git_root_found() {
        // Use the repo we're inside
        let cwd = std::env::current_dir().unwrap();
        // Walk up from workspace root — git root must exist
        let root = find_git_root(&cwd);
        assert!(root.is_some());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p common
```
Expected: compile errors — functions not defined yet.

- [ ] **Step 3: Implement common/src/lib.rs**

```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{fence, Ordering};

pub const MMAP_SIZE: usize = 138;
pub const BRANCH_MAX: usize = 128;

// Layout:
// [0..8]   u64 seqlock counter (even=stable, odd=write in progress)
// [8]      u8 dirty flag
// [9]      u8 branch name length
// [10..138] [u8;128] branch name bytes (zero-padded)

/// FNV-1a 32-bit hash of a path's bytes. No external deps.
pub fn hash_path(path: &Path) -> u32 {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut hash: u32 = 2_166_136_261;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

pub fn mmap_path(hash: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/ccsl-{hash}.bin"))
}

pub fn watch_path(hash: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/ccsl-watch/{hash}"))
}

/// Seqlock read: spin until we get a consistent even-seq snapshot.
pub fn seqlock_read(mmap: &[u8]) -> (String, bool) {
    loop {
        let seq1 = read_u64_le(mmap, 0);
        if seq1 & 1 == 1 {
            std::hint::spin_loop();
            continue;
        }
        fence(Ordering::Acquire);

        let dirty = mmap[8] != 0;
        let branch_len = (mmap[9] as usize).min(BRANCH_MAX);
        let branch = String::from_utf8_lossy(&mmap[10..10 + branch_len]).into_owned();

        fence(Ordering::Acquire);
        let seq2 = read_u64_le(mmap, 0);
        if seq1 == seq2 {
            return (branch, dirty);
        }
        std::hint::spin_loop();
    }
}

fn read_u64_le(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(buf[offset..offset + 8].try_into().unwrap())
}

/// Walk up from `path` looking for a `.git/` directory. Returns the repo root.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.join(".git").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p common
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add common/
git commit -m "feat(common): fnv hash, mmap paths, seqlock read, find_git_root"
```

---

### Task 3: cc-statusline — fmt.rs

**Files:**
- Create: `cc-statusline/src/fmt.rs`

- [ ] **Step 1: Write failing tests**

Create `cc-statusline/src/fmt.rs` with tests only:

```rust
// cc-statusline/src/fmt.rs

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_k() {
        assert_eq!(format_tokens(1_500), "2k");
        assert_eq!(format_tokens(1_000), "1k");
        assert_eq!(format_tokens(999_999), "1000k");
    }

    #[test]
    fn test_format_tokens_m() {
        assert_eq!(format_tokens(1_000_000), "1.0m");
        assert_eq!(format_tokens(1_500_000), "1.5m");
    }

    #[test]
    fn test_color_for_pct() {
        assert_eq!(color_for_pct(0), GREEN);
        assert_eq!(color_for_pct(49), GREEN);
        assert_eq!(color_for_pct(50), ORANGE);
        assert_eq!(color_for_pct(69), ORANGE);
        assert_eq!(color_for_pct(70), YELLOW);
        assert_eq!(color_for_pct(89), YELLOW);
        assert_eq!(color_for_pct(90), RED);
        assert_eq!(color_for_pct(100), RED);
    }

    #[test]
    fn test_build_bar_half() {
        let mut s = String::new();
        build_bar(50, 10, &mut s);
        // Should contain 5 filled and 5 empty dots (ignoring color codes)
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●○○○○○");
    }

    #[test]
    fn test_build_bar_full() {
        let mut s = String::new();
        build_bar(100, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●●●●●●");
    }

    #[test]
    fn test_build_bar_empty() {
        let mut s = String::new();
        build_bar(0, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "○○○○○○○○○○");
    }
}
```

Add `mod fmt;` to `cc-statusline/src/main.rs` temporarily so it compiles.

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline
```
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement fmt.rs**

```rust
// cc-statusline/src/fmt.rs
pub const BLUE:    &str = "\x1b[38;2;0;153;255m";
pub const ORANGE:  &str = "\x1b[38;2;255;176;85m";
pub const GREEN:   &str = "\x1b[38;2;0;175;80m";
pub const CYAN:    &str = "\x1b[38;2;86;182;194m";
pub const RED:     &str = "\x1b[38;2;255;85;85m";
pub const YELLOW:  &str = "\x1b[38;2;230;200;0m";
pub const WHITE:   &str = "\x1b[38;2;220;220;220m";
pub const DIM:     &str = "\x1b[2m";
pub const RESET:   &str = "\x1b[0m";
pub const SEP:     &str = " \x1b[2m│\x1b[0m ";

pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

pub fn color_for_pct(pct: u8) -> &'static str {
    if pct >= 90      { RED }
    else if pct >= 70 { YELLOW }
    else if pct >= 50 { ORANGE }
    else              { GREEN }
}

/// Write a color-coded seqlock bar into `out`. width=10 is standard.
pub fn build_bar(pct: u8, width: usize, out: &mut String) {
    let pct = pct.min(100) as usize;
    let filled = pct * width / 100;
    let empty = width - filled;
    out.push_str(color_for_pct(pct as u8));
    for _ in 0..filled { out.push('●'); }
    out.push_str(DIM);
    for _ in 0..empty  { out.push('○'); }
    out.push_str(RESET);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_k() {
        assert_eq!(format_tokens(1_500), "2k");
        assert_eq!(format_tokens(1_000), "1k");
    }

    #[test]
    fn test_format_tokens_m() {
        assert_eq!(format_tokens(1_000_000), "1.0m");
        assert_eq!(format_tokens(1_500_000), "1.5m");
    }

    #[test]
    fn test_color_for_pct() {
        assert_eq!(color_for_pct(0), GREEN);
        assert_eq!(color_for_pct(49), GREEN);
        assert_eq!(color_for_pct(50), ORANGE);
        assert_eq!(color_for_pct(69), ORANGE);
        assert_eq!(color_for_pct(70), YELLOW);
        assert_eq!(color_for_pct(89), YELLOW);
        assert_eq!(color_for_pct(90), RED);
        assert_eq!(color_for_pct(100), RED);
    }

    #[test]
    fn test_build_bar_half() {
        let mut s = String::new();
        build_bar(50, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●○○○○○");
    }

    #[test]
    fn test_build_bar_full() {
        let mut s = String::new();
        build_bar(100, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "●●●●●●●●●●");
    }

    #[test]
    fn test_build_bar_empty() {
        let mut s = String::new();
        build_bar(0, 10, &mut s);
        let plain: String = s.chars().filter(|&c| c == '●' || c == '○').collect();
        assert_eq!(plain, "○○○○○○○○○○");
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p cc-statusline fmt
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline/src/fmt.rs cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): fmt module — colors, token formatter, progress bar"
```

---

### Task 4: cc-statusline — time.rs

**Files:**
- Create: `cc-statusline/src/time.rs`

- [ ] **Step 1: Write failing tests**

```rust
// cc-statusline/src/time.rs (tests first)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso8601_utc_z() {
        // 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(parse_iso8601("2024-01-15T10:30:00Z"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_with_fractional() {
        assert_eq!(parse_iso8601("2024-01-15T10:30:00.000Z"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_offset_plus() {
        // +05:30 means UTC-5:30 offset, so subtract 5h30m from the given time
        // 2024-01-15T16:00:00+05:30 = 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(parse_iso8601("2024-01-15T16:00:00+05:30"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_offset_minus() {
        // 2024-01-15T05:30:00-05:00 = 2024-01-15T10:30:00Z = 1705314600
        assert_eq!(parse_iso8601("2024-01-15T05:30:00-05:00"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_invalid() {
        assert_eq!(parse_iso8601("not-a-date"), None);
        assert_eq!(parse_iso8601(""), None);
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(90), "1m");
        assert_eq!(format_duration(3599), "59m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h0m");
        assert_eq!(format_duration(3661), "1h1m");
        assert_eq!(format_duration(7384), "2h3m");
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline time
```
Expected: compile error — functions not defined.

- [ ] **Step 3: Implement time.rs**

```rust
// cc-statusline/src/time.rs

pub enum ResetStyle {
    Time,
    DateTime,
}

/// Parse ISO 8601 datetime string to Unix epoch seconds. No external deps.
/// Handles: YYYY-MM-DDTHH:MM:SS[.fff][Z|+HH:MM|-HH:MM]
pub fn parse_iso8601(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 19 { return None; }

    let year:  i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day:   i64 = s.get(8..10)?.parse().ok()?;
    let hour:  i64 = s.get(11..13)?.parse().ok()?;
    let min:   i64 = s.get(14..16)?.parse().ok()?;
    let sec:   i64 = s.get(17..19)?.parse().ok()?;

    // Find timezone marker after optional fractional seconds
    let after = &s[19..];
    let tz = after
        .find(|c: char| c == 'Z' || c == '+' || c == '-')
        .map(|i| &after[i..])
        .unwrap_or("Z");

    let tz_offset: i64 = if tz.starts_with('Z') {
        0
    } else {
        let sign: i64 = if tz.starts_with('+') { 1 } else { -1 };
        let t = &tz[1..];
        let h: i64 = t.get(0..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: i64 = t.get(3..5).and_then(|s| s.parse().ok()).unwrap_or(0);
        sign * (h * 3600 + m * 60)
    };

    let days = days_since_epoch(year, month, day)?;
    let epoch = days * 86400 + hour * 3600 + min * 60 + sec - tz_offset;
    if epoch < 0 { return None; }
    Some(epoch as u64)
}

/// Days since 1970-01-01 using proleptic Gregorian calendar.
fn days_since_epoch(year: i64, month: i64, day: i64) -> Option<i64> {
    if month < 1 || month > 12 || day < 1 || day > 31 { return None; }
    let y = year - (month <= 2) as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format epoch as local time. Uses libc::localtime_r — no chrono.
pub fn format_reset_time(epoch: u64, style: ResetStyle) -> String {
    use libc::{localtime_r, time_t, tm};
    unsafe {
        let t = epoch as time_t;
        let mut local: tm = std::mem::zeroed();
        localtime_r(&t, &mut local);

        let hour = local.tm_hour;
        let min  = local.tm_min;
        let ampm = if hour >= 12 { "pm" } else { "am" };
        let h12  = match hour % 12 { 0 => 12, h => h };

        match style {
            ResetStyle::Time => format!("{h12}:{min:02}{ampm}"),
            ResetStyle::DateTime => {
                const MONTHS: [&str; 12] = [
                    "jan","feb","mar","apr","may","jun",
                    "jul","aug","sep","oct","nov","dec",
                ];
                let month = MONTHS[local.tm_mon as usize];
                let day   = local.tm_mday;
                format!("{month} {day}, {h12}:{min:02}{ampm}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iso8601_utc_z() {
        assert_eq!(parse_iso8601("2024-01-15T10:30:00Z"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_with_fractional() {
        assert_eq!(parse_iso8601("2024-01-15T10:30:00.000Z"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_offset_plus() {
        assert_eq!(parse_iso8601("2024-01-15T16:00:00+05:30"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_offset_minus() {
        assert_eq!(parse_iso8601("2024-01-15T05:30:00-05:00"), Some(1_705_314_600));
    }

    #[test]
    fn test_parse_iso8601_invalid() {
        assert_eq!(parse_iso8601("not-a-date"), None);
        assert_eq!(parse_iso8601(""), None);
    }

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(59), "59s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(90), "1m");
        assert_eq!(format_duration(3599), "59m");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(3600), "1h0m");
        assert_eq!(format_duration(3661), "1h1m");
        assert_eq!(format_duration(7384), "2h3m");
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cc-statusline time
```
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline/src/time.rs cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): time module — ISO 8601 parser, duration, reset formatter"
```

---

### Task 5: cc-statusline — input.rs

**Files:**
- Create: `cc-statusline/src/input.rs`

- [ ] **Step 1: Write failing test**

```rust
// cc-statusline/src/input.rs (tests first)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal() {
        let json = r#"{}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(input.model.is_none());
        assert!(input.cwd.is_none());
    }

    #[test]
    fn test_deserialize_full() {
        let json = r#"{
            "model": {"display_name": "Claude Sonnet 4.5"},
            "session_id": "abc123",
            "cwd": "/home/user/project",
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {
                    "input_tokens": 1000,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 200
                }
            },
            "session": {"start_time": "2024-01-15T10:00:00Z"},
            "rate_limits": {
                "five_hour": {"used_percentage": 45.5, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(input.model.unwrap().display_name.unwrap(), "Claude Sonnet 4.5");
        assert_eq!(input.session_id.unwrap(), "abc123");
        let cw = input.context_window.unwrap();
        assert_eq!(cw.context_window_size.unwrap(), 200_000);
        let usage = cw.current_usage.unwrap();
        assert_eq!(usage.input_tokens.unwrap(), 1000);
        let rl = input.rate_limits.unwrap();
        assert!((rl.five_hour.unwrap().used_percentage.unwrap() - 45.5).abs() < 0.01);
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline input
```
Expected: compile error.

- [ ] **Step 3: Implement input.rs**

```rust
// cc-statusline/src/input.rs
use serde::Deserialize;

#[derive(Deserialize, Default)]
pub struct Input {
    pub model: Option<Model>,
    pub session_id: Option<String>,
    pub context_window: Option<ContextWindow>,
    pub cwd: Option<String>,
    pub session: Option<Session>,
    pub rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
pub struct Model {
    pub display_name: Option<String>,
}

#[derive(Deserialize)]
pub struct ContextWindow {
    pub context_window_size: Option<u64>,
    pub current_usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
pub struct Session {
    pub start_time: Option<String>,
}

#[derive(Deserialize)]
pub struct RateLimits {
    pub five_hour: Option<RateWindow>,
    pub seven_day: Option<RateWindow>,
}

#[derive(Deserialize)]
pub struct RateWindow {
    pub used_percentage: Option<f64>,
    pub resets_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal() {
        let input: Input = serde_json::from_str("{}").unwrap();
        assert!(input.model.is_none());
        assert!(input.cwd.is_none());
    }

    #[test]
    fn test_deserialize_full() {
        let json = r#"{
            "model": {"display_name": "Claude Sonnet 4.5"},
            "session_id": "abc123",
            "cwd": "/home/user/project",
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {
                    "input_tokens": 1000,
                    "cache_creation_input_tokens": 500,
                    "cache_read_input_tokens": 200
                }
            },
            "session": {"start_time": "2024-01-15T10:00:00Z"},
            "rate_limits": {
                "five_hour": {"used_percentage": 45.5, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert_eq!(input.model.unwrap().display_name.unwrap(), "Claude Sonnet 4.5");
        let rl = input.rate_limits.unwrap();
        assert!((rl.five_hour.unwrap().used_percentage.unwrap() - 45.5).abs() < 0.01);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cc-statusline input
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline/src/input.rs cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): input module — serde structs for Claude Code JSON"
```

---

### Task 6: cc-statusline — render.rs

**Files:**
- Create: `cc-statusline/src/render.rs`

- [ ] **Step 1: Write failing tests**

```rust
// cc-statusline/src/render.rs (tests first)

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Input;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' { in_escape = true; continue; }
            if in_escape { if c == 'm' { in_escape = false; } continue; }
            out.push(c);
        }
        out
    }

    #[test]
    fn test_render_default_model() {
        let input = Input::default();
        let result = render(&input, None);
        let plain = strip_ansi(&result);
        assert!(plain.contains("Claude"), "should contain default model name");
    }

    #[test]
    fn test_render_model_name() {
        let json = r#"{"model": {"display_name": "claude-sonnet-4-5"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None));
        assert!(plain.contains("claude-sonnet-4-5"));
    }

    #[test]
    fn test_render_context_pct() {
        let json = r#"{
            "context_window": {
                "context_window_size": 200000,
                "current_usage": {"input_tokens": 100000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None));
        assert!(plain.contains("50%"));
    }

    #[test]
    fn test_render_git_branch() {
        let input = Input::default();
        let plain = strip_ansi(&render(&input, Some(("main".to_string(), false))));
        assert!(plain.contains("(main)"));
    }

    #[test]
    fn test_render_git_dirty() {
        let input = Input::default();
        let plain = strip_ansi(&render(&input, Some(("main".to_string(), true))));
        assert!(plain.contains("(main*") || plain.contains("main") && plain.contains('*'));
    }

    #[test]
    fn test_render_dirname() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let plain = strip_ansi(&render(&input, None));
        assert!(plain.contains("myproject"));
    }

    #[test]
    fn test_render_rate_limits_present() {
        let json = r#"{
            "rate_limits": {
                "five_hour": {"used_percentage": 45.0, "resets_at": 1705316400},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}
            }
        }"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None);
        assert!(result.contains('\n'), "should have newlines for rate limits");
        let plain = strip_ansi(&result);
        assert!(plain.contains("current"));
        assert!(plain.contains("weekly"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline render
```
Expected: compile error.

- [ ] **Step 3: Implement render.rs**

```rust
// cc-statusline/src/render.rs
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fmt::{self, *};
use crate::input::Input;
use crate::time::{format_duration, format_reset_time, parse_iso8601, ResetStyle};

pub fn render(input: &Input, git: Option<(String, bool)>) -> String {
    let mut out = String::with_capacity(512);

    // ── Model ──────────────────────────────────────────────
    let model = input.model.as_ref()
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or("Claude");
    out.push_str(BLUE);
    out.push_str(model);
    out.push_str(RESET);

    // ── Context usage ──────────────────────────────────────
    out.push_str(SEP);
    let pct = context_pct(input);
    out.push_str("✍️ ");
    out.push_str(color_for_pct(pct));
    out.push_str(&format!("{pct}%"));
    out.push_str(RESET);

    // ── Dir + git ──────────────────────────────────────────
    out.push_str(SEP);
    let cwd = input.cwd.as_deref().unwrap_or("");
    let dirname = Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(cwd);
    out.push_str(CYAN);
    out.push_str(dirname);
    out.push_str(RESET);
    if let Some((branch, dirty)) = &git {
        out.push(' ');
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

    // ── Rate limits ────────────────────────────────────────
    if let Some(rl) = &input.rate_limits {
        if let Some(fh) = &rl.five_hour {
            if let Some(pct_f) = fh.used_percentage {
                let pct = pct_f.round().clamp(0.0, 100.0) as u8;
                out.push_str("\n\n");
                out.push_str(WHITE);
                out.push_str("current");
                out.push_str(RESET);
                out.push(' ');
                fmt::build_bar(pct, 10, &mut out);
                out.push(' ');
                out.push_str(color_for_pct(pct));
                out.push_str(&format!("{pct:2}%"));
                out.push_str(RESET);
                if let Some(epoch) = fh.resets_at.filter(|&e| e > 0) {
                    out.push_str(&format!(
                        " {DIM} ⟳ {RESET}{WHITE}{}{RESET}",
                        format_reset_time(epoch, ResetStyle::Time)
                    ));
                }

                if let Some(sd) = &rl.seven_day {
                    if let Some(pct_f) = sd.used_percentage {
                        let pct = pct_f.round().clamp(0.0, 100.0) as u8;
                        out.push('\n');
                        out.push_str(WHITE);
                        out.push_str("weekly ");
                        out.push_str(RESET);
                        out.push(' ');
                        fmt::build_bar(pct, 10, &mut out);
                        out.push(' ');
                        out.push_str(color_for_pct(pct));
                        out.push_str(&format!("{pct:2}%"));
                        out.push_str(RESET);
                        if let Some(epoch) = sd.resets_at.filter(|&e| e > 0) {
                            out.push_str(&format!(
                                " {DIM} ⟳ {RESET}{WHITE}{}{RESET}",
                                format_reset_time(epoch, ResetStyle::DateTime)
                            ));
                        }
                    }
                }
            }
        }
    }

    out
}

fn context_pct(input: &Input) -> u8 {
    let size = input.context_window.as_ref()
        .and_then(|cw| cw.context_window_size)
        .filter(|&s| s > 0)
        .unwrap_or(200_000);
    let current = input.context_window.as_ref()
        .and_then(|cw| cw.current_usage.as_ref())
        .map(|u| {
            u.input_tokens.unwrap_or(0)
                + u.cache_creation_input_tokens.unwrap_or(0)
                + u.cache_read_input_tokens.unwrap_or(0)
        })
        .unwrap_or(0);
    ((current * 100) / size).min(100) as u8
}

fn session_duration(input: &Input) -> Option<String> {
    let start = input.session.as_ref()?.start_time.as_deref()?;
    let start_epoch = parse_iso8601(start)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format_duration(now.saturating_sub(start_epoch)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Input;

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for c in s.chars() {
            if c == '\x1b' { in_escape = true; continue; }
            if in_escape { if c == 'm' { in_escape = false; } continue; }
            out.push(c);
        }
        out
    }

    #[test]
    fn test_render_default_model() {
        let plain = strip_ansi(&render(&Input::default(), None));
        assert!(plain.contains("Claude"));
    }

    #[test]
    fn test_render_model_name() {
        let json = r#"{"model": {"display_name": "claude-sonnet-4-5"}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(strip_ansi(&render(&input, None)).contains("claude-sonnet-4-5"));
    }

    #[test]
    fn test_render_context_pct() {
        let json = r#"{"context_window": {"context_window_size": 200000, "current_usage": {"input_tokens": 100000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(strip_ansi(&render(&input, None)).contains("50%"));
    }

    #[test]
    fn test_render_git_branch() {
        let plain = strip_ansi(&render(&Input::default(), Some(("main".into(), false))));
        assert!(plain.contains("(main)"));
    }

    #[test]
    fn test_render_git_dirty() {
        let plain = strip_ansi(&render(&Input::default(), Some(("main".into(), true))));
        assert!(plain.contains("main") && plain.contains('*'));
    }

    #[test]
    fn test_render_dirname() {
        let json = r#"{"cwd": "/home/user/myproject"}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        assert!(strip_ansi(&render(&input, None)).contains("myproject"));
    }

    #[test]
    fn test_render_rate_limits_present() {
        let json = r#"{"rate_limits": {"five_hour": {"used_percentage": 45.0, "resets_at": 1705316400}, "seven_day": {"used_percentage": 12.0, "resets_at": 1705833600}}}"#;
        let input: Input = serde_json::from_str(json).unwrap();
        let result = render(&input, None);
        assert!(result.contains('\n'));
        let plain = strip_ansi(&result);
        assert!(plain.contains("current"));
        assert!(plain.contains("weekly"));
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cc-statusline render
```
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline/src/render.rs cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): render module — assembles statusline string"
```

---

### Task 7: cc-statusline — git.rs

**Files:**
- Create: `cc-statusline/src/git.rs`

- [ ] **Step 1: Write failing tests**

```rust
// cc-statusline/src/git.rs (tests first)

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_branch_and_dirty_in_git_repo() {
        // This test runs inside the cc-statusline repo
        let cwd = std::env::current_dir().unwrap();
        // Should not panic and should return Some if we're in a git repo
        let result = branch_and_dirty(&cwd);
        // We're in a git repo so we expect Some
        assert!(result.is_some(), "expected git info for current dir");
        let (branch, _dirty) = result.unwrap();
        assert!(!branch.is_empty(), "branch should not be empty");
    }

    #[test]
    fn test_branch_and_dirty_not_git() {
        // /tmp is not a git repo
        let result = branch_and_dirty(Path::new("/tmp"));
        assert!(result.is_none());
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline git
```
Expected: compile error.

- [ ] **Step 3: Implement git.rs**

```rust
// cc-statusline/src/git.rs
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use common::{hash_path, mmap_path, seqlock_read, watch_path, MMAP_SIZE};
use memmap2::Mmap;

/// Returns (branch, is_dirty) for the git repo containing `cwd`.
/// Fast path: reads from daemon mmap file (~10µs).
/// Slow path (first render or daemon not running): registers path + runs git.
pub fn branch_and_dirty(cwd: &Path) -> Option<(String, bool)> {
    let hash = hash_path(cwd);

    // ── Fast path: mmap ──────────────────────────────────
    if let Some(result) = try_mmap_read(hash) {
        return Some(result);
    }

    // ── Slow path: register + direct git ─────────────────
    register(cwd, hash);
    git_fallback(cwd)
}

fn try_mmap_read(hash: u32) -> Option<(String, bool)> {
    let file = fs::File::open(mmap_path(hash)).ok()?;
    if file.metadata().ok()?.len() != MMAP_SIZE as u64 {
        return None;
    }
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let (branch, dirty) = seqlock_read(&mmap);
    if branch.is_empty() { None } else { Some((branch, dirty)) }
}

fn register(cwd: &Path, hash: u32) {
    let watch_dir = Path::new("/tmp/ccsl-watch");
    let _ = fs::create_dir_all(watch_dir);
    if let Ok(mut f) = fs::File::create(watch_path(hash)) {
        let _ = f.write_all(cwd.as_os_str().as_encoded_bytes());
    }
}

fn git_fallback(cwd: &Path) -> Option<(String, bool)> {
    let git_root = common::find_git_root(cwd)?;
    let head = fs::read_to_string(git_root.join(".git/HEAD")).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_branch_and_dirty_in_git_repo() {
        let cwd = std::env::current_dir().unwrap();
        let result = branch_and_dirty(&cwd);
        assert!(result.is_some());
        let (branch, _) = result.unwrap();
        assert!(!branch.is_empty());
    }

    #[test]
    fn test_branch_and_dirty_not_git() {
        assert!(branch_and_dirty(Path::new("/tmp")).is_none());
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cc-statusline git
```
Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline/src/git.rs cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): git module — mmap fast path + git fallback"
```

---

### Task 8: cc-statusline — main.rs + end-to-end test

**Files:**
- Modify: `cc-statusline/src/main.rs`

- [ ] **Step 1: Implement main.rs**

```rust
// cc-statusline/src/main.rs
mod fmt;
mod git;
mod input;
mod render;
mod time;

use std::io::{self, Read};
use std::path::Path;

fn main() {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw).unwrap_or(0);

    if raw.trim().is_empty() {
        print!("Claude");
        return;
    }

    let input: input::Input = serde_json::from_str(&raw).unwrap_or_default();
    let cwd = input.cwd.as_deref().unwrap_or("").to_owned();
    let git = if cwd.is_empty() {
        None
    } else {
        git::branch_and_dirty(Path::new(&cwd))
    };

    print!("{}", render::render(&input, git));
}
```

- [ ] **Step 2: Build release binary**

```bash
cargo build --release -p cc-statusline 2>&1
```
Expected: compiles with zero errors, zero warnings.

- [ ] **Step 3: Test empty stdin**

```bash
echo -n "" | ./target/release/cc-statusline
```
Expected output: `Claude` (exactly, no newline).

- [ ] **Step 4: Test minimal JSON**

```bash
echo '{}' | ./target/release/cc-statusline
```
Expected: `Claude │ ✍️ 0%` followed by a directory and no rate limits.

- [ ] **Step 5: Test full JSON**

```bash
echo '{
  "model": {"display_name": "claude-sonnet-4-5"},
  "cwd": "/Users/carterhimmel/code/fyko/cc-statusline",
  "context_window": {
    "context_window_size": 200000,
    "current_usage": {"input_tokens": 50000, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
  },
  "session": {"start_time": "2026-04-14T10:00:00Z"},
  "rate_limits": {
    "five_hour": {"used_percentage": 30.0, "resets_at": 1744660000},
    "seven_day": {"used_percentage": 8.0, "resets_at": 1745000000}
  }
}' | ./target/release/cc-statusline
```
Expected: 3-line output with model, context %, branch `(main)` or similar, session time, and current/weekly bars.

- [ ] **Step 6: Commit**

```bash
git add cc-statusline/src/main.rs
git commit -m "feat(cc-statusline): wire main.rs — client binary complete"
```

---

### Task 9: daemon — cache.rs

**Files:**
- Create: `cc-statusline-daemon/src/cache.rs`

- [ ] **Step 1: Write failing test**

```rust
// cc-statusline-daemon/src/cache.rs (tests first)

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_seqlock_write_readable() {
        use common::{seqlock_read, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "feature-branch", true);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "feature-branch");
        assert!(dirty);
    }

    #[test]
    fn test_seqlock_write_clean() {
        use common::{seqlock_read, MMAP_SIZE};
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "main", false);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(!dirty);
    }
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p cc-statusline-daemon cache
```
Expected: compile error.

- [ ] **Step 3: Implement cache.rs**

```rust
// cc-statusline-daemon/src/cache.rs
use std::fs::OpenOptions;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{fence, Ordering};

use common::{find_git_root, hash_path, mmap_path, BRANCH_MAX, MMAP_SIZE};
use memmap2::MmapMut;

/// Re-run git status for `cwd` and write result to the mmap cache file.
pub fn update(cwd: &Path) {
    let Some((branch, dirty)) = git_status(cwd) else { return };
    let hash = hash_path(cwd);
    let path = mmap_path(hash);

    let file = match OpenOptions::new().read(true).write(true).create(true).open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = file.set_len(MMAP_SIZE as u64);
    let mut mmap = match unsafe { MmapMut::map_mut(&file) } {
        Ok(m) => m,
        Err(_) => return,
    };
    seqlock_write(&mut mmap[..], &branch, dirty);
}

/// Write branch + dirty to a raw byte slice using a seqlock protocol.
/// Exported for testing with plain Vec<u8>.
pub fn seqlock_write(buf: &mut [u8], branch: &str, dirty: bool) {
    let seq = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    // Increment to odd → write in progress
    buf[0..8].copy_from_slice(&seq.wrapping_add(1).to_le_bytes());
    fence(Ordering::SeqCst);

    buf[8] = dirty as u8;
    let bytes = branch.as_bytes();
    let len = bytes.len().min(BRANCH_MAX);
    buf[9] = len as u8;
    buf[10..10 + len].copy_from_slice(&bytes[..len]);
    for b in &mut buf[10 + len..10 + BRANCH_MAX] {
        *b = 0;
    }

    fence(Ordering::SeqCst);
    // Increment to even → write complete
    let seq2 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    buf[0..8].copy_from_slice(&seq2.wrapping_add(1).to_le_bytes());
}

fn git_status(cwd: &Path) -> Option<(String, bool)> {
    let git_root = find_git_root(cwd)?;
    let head = std::fs::read_to_string(git_root.join(".git/HEAD")).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::{seqlock_read, MMAP_SIZE};

    #[test]
    fn test_seqlock_write_readable() {
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "feature-branch", true);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "feature-branch");
        assert!(dirty);
    }

    #[test]
    fn test_seqlock_write_clean() {
        let mut buf = vec![0u8; MMAP_SIZE];
        seqlock_write(&mut buf, "main", false);
        let (branch, dirty) = seqlock_read(&buf);
        assert_eq!(branch, "main");
        assert!(!dirty);
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p cc-statusline-daemon cache
```
Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add cc-statusline-daemon/src/cache.rs cc-statusline-daemon/src/main.rs
git commit -m "feat(daemon): cache module — seqlock write to mmap files"
```

---

### Task 10: daemon — registrar.rs

**Files:**
- Create: `cc-statusline-daemon/src/registrar.rs`

- [ ] **Step 1: Implement registrar.rs**

No isolated unit test possible (requires FS + threads), so implement directly and verify via integration in Task 12.

```rust
// cc-statusline-daemon/src/registrar.rs
use std::fs;
use std::path::PathBuf;

use crossbeam_channel::Sender;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub const WATCH_DIR: &str = "/tmp/ccsl-watch";

/// Watch /tmp/ccsl-watch/ for new marker files. Each file contains an absolute
/// path as UTF-8 bytes. Sends each path to `tx` for the watcher to pick up.
/// Also drains any existing marker files on startup (handles daemon restarts).
pub fn start(tx: Sender<PathBuf>) {
    let watch_dir = std::path::Path::new(WATCH_DIR);
    fs::create_dir_all(watch_dir).expect("failed to create /tmp/ccsl-watch");

    // Drain existing markers (daemon may have been restarted)
    if let Ok(entries) = fs::read_dir(watch_dir) {
        for entry in entries.flatten() {
            send_path_from_marker(&entry.path(), &tx);
        }
    }

    let tx2 = tx.clone();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if matches!(event.kind, EventKind::Create(_)) {
                    for path in &event.paths {
                        send_path_from_marker(path, &tx2);
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create notify watcher");

    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .expect("failed to watch /tmp/ccsl-watch");

    // Park this thread — `watcher` must stay alive to keep watching.
    let _watcher = watcher;
    std::thread::park();
}

fn send_path_from_marker(marker: &std::path::Path, tx: &Sender<PathBuf>) {
    if let Ok(bytes) = fs::read(marker) {
        if let Ok(s) = std::str::from_utf8(&bytes) {
            let path = PathBuf::from(s.trim());
            if path.is_absolute() {
                let _ = tx.send(path);
            }
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p cc-statusline-daemon
```
Expected: zero errors.

- [ ] **Step 3: Commit**

```bash
git add cc-statusline-daemon/src/registrar.rs
git commit -m "feat(daemon): registrar — watch /tmp/ccsl-watch for new path registrations"
```

---

### Task 11: daemon — watcher.rs

**Files:**
- Create: `cc-statusline-daemon/src/watcher.rs`

- [ ] **Step 1: Implement watcher.rs**

```rust
// cc-statusline-daemon/src/watcher.rs
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::cache;
use common::find_git_root;

/// Receive new cwd paths from the registrar, find their git roots, watch
/// .git/index + .git/HEAD, and call cache::update on every FS change.
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
                        // path = {git_root}/.git/index  →  parent = .git/  →  parent = git_root
                        if let Some(git_root) = path.parent().and_then(|p| p.parent()) {
                            let _ = event_tx.send(git_root.to_path_buf());
                        }
                    }
                }
            }
        },
        Config::default(),
    )
    .expect("failed to create FS watcher");

    // git_root → all registered cwds within that repo
    let mut repo_cwds: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut watched: HashSet<PathBuf> = HashSet::new();

    loop {
        crossbeam_channel::select! {
            recv(rx) -> msg => {
                let Ok(cwd) = msg else { break };
                if let Some(git_root) = find_git_root(&cwd) {
                    repo_cwds.entry(git_root.clone()).or_default().push(cwd.clone());
                    if watched.insert(git_root.clone()) {
                        let _ = watcher.watch(
                            &git_root.join(".git/index"),
                            RecursiveMode::NonRecursive,
                        );
                        let _ = watcher.watch(
                            &git_root.join(".git/HEAD"),
                            RecursiveMode::NonRecursive,
                        );
                    }
                    cache::update(&cwd);
                }
            }
            recv(event_rx) -> msg => {
                let Ok(git_root) = msg else { break };
                if let Some(cwds) = repo_cwds.get(&git_root) {
                    for cwd in cwds {
                        cache::update(cwd);
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p cc-statusline-daemon
```
Expected: zero errors.

- [ ] **Step 3: Commit**

```bash
git add cc-statusline-daemon/src/watcher.rs
git commit -m "feat(daemon): watcher — FS event dispatch to cache updates"
```

---

### Task 12: daemon — main.rs + integration test

**Files:**
- Modify: `cc-statusline-daemon/src/main.rs`

- [ ] **Step 1: Implement main.rs**

```rust
// cc-statusline-daemon/src/main.rs
mod cache;
mod registrar;
mod watcher;

use std::path::PathBuf;

fn main() {
    let (tx, rx) = crossbeam_channel::unbounded::<PathBuf>();
    let tx2 = tx.clone();

    std::thread::spawn(move || {
        registrar::start(tx2);
    });

    // watcher::start runs the main event loop — blocks until channel closes
    watcher::start(rx);
}
```

- [ ] **Step 2: Build both release binaries**

```bash
cargo build --release 2>&1
```
Expected: zero errors, both `target/release/cc-statusline` and `target/release/cc-statusline-daemon` produced.

- [ ] **Step 3: Integration test — daemon populates mmap, client reads it**

In one terminal:
```bash
./target/release/cc-statusline-daemon &
DAEMON_PID=$!
sleep 0.5
```

In the same terminal, run client pointing at this repo:
```bash
echo "{\"model\":{\"display_name\":\"test\"},\"cwd\":\"$(pwd)\"}" \
  | ./target/release/cc-statusline
```
Expected: first run shows branch name (from git fallback). Wait 1s, run again:
```bash
echo "{\"model\":{\"display_name\":\"test\"},\"cwd\":\"$(pwd)\"}" \
  | ./target/release/cc-statusline
```
Expected: second run also shows correct branch (from mmap fast path — verify daemon created `/tmp/ccsl-*.bin`):
```bash
ls /tmp/ccsl-*.bin
```

- [ ] **Step 4: Kill daemon**

```bash
kill $DAEMON_PID
```

- [ ] **Step 5: Benchmark**

Install `hyperfine` if needed (`brew install hyperfine`), then:
```bash
# Start daemon first
./target/release/cc-statusline-daemon &
DAEMON_PID=$!
sleep 1

SAMPLE="{\"model\":{\"display_name\":\"test\"},\"cwd\":\"$(pwd)\"}"

# Warm up mmap (one render to register)
echo "$SAMPLE" | ./target/release/cc-statusline > /dev/null

hyperfine \
  --warmup 5 \
  "echo '$SAMPLE' | ./target/release/cc-statusline" \
  "echo '$SAMPLE' | bash ~/.claude/statusline.sh"

kill $DAEMON_PID
```
Expected: Rust binary with warm mmap should be **10-50x faster** than bash.

- [ ] **Step 6: Commit**

```bash
git add cc-statusline-daemon/src/main.rs
git commit -m "feat(daemon): main.rs — wire registrar + watcher threads"
```

---

### Task 13: Install + wire into Claude Code

**Files:**
- Modify: `~/.claude/settings.json`
- Create: `~/.local/bin/` symlinks or copies

- [ ] **Step 1: Install binaries**

```bash
mkdir -p ~/.local/bin
cp target/release/cc-statusline ~/.local/bin/cc-statusline
cp target/release/cc-statusline-daemon ~/.local/bin/cc-statusline-daemon
```

Verify:
```bash
~/.local/bin/cc-statusline --help 2>&1 || echo '{}' | ~/.local/bin/cc-statusline
```
Expected: prints `Claude` or statusline output.

- [ ] **Step 2: Update ~/.claude/settings.json**

Change the `statusLine` entry from:
```json
"statusLine": {
  "type": "command",
  "command": "bash \"$HOME/.claude/statusline.sh\""
}
```
to:
```json
"statusLine": {
  "type": "command",
  "command": "$HOME/.local/bin/cc-statusline"
}
```

- [ ] **Step 3: Create launchd plist for daemon auto-start (macOS)**

Create `~/Library/LaunchAgents/com.cc-statusline.daemon.plist`:
```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.cc-statusline.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/carterhimmel/.local/bin/cc-statusline-daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardErrorPath</key>
  <string>/tmp/cc-statusline-daemon.log</string>
</dict>
</plist>
```

Load it:
```bash
launchctl load ~/Library/LaunchAgents/com.cc-statusline.daemon.plist
```

Verify running:
```bash
launchctl list | grep cc-statusline
```
Expected: shows the daemon PID.

- [ ] **Step 4: Verify in Claude Code**

Open a new Claude Code session. The statusline should display:
- Model name in blue
- Context % with color-coded indicator
- Current directory + git branch + dirty flag if applicable
- Session duration
- Rate limit bars if rate limit data is available

- [ ] **Step 5: Final commit**

```bash
git add docs/
git commit -m "docs: add implementation plan"
```

---

## Verification Summary

| Check | Command | Expected |
|---|---|---|
| All tests pass | `cargo test --workspace` | zero failures |
| Zero warnings | `cargo build --release 2>&1 \| grep warning` | no output |
| Empty stdin | `echo -n "" \| ./target/release/cc-statusline` | `Claude` |
| Minimal JSON | `echo '{}' \| ./target/release/cc-statusline` | valid output |
| Daemon populates mmap | see Task 12 step 3 | `/tmp/ccsl-*.bin` exists |
| Speedup | `hyperfine` in Task 12 step 5 | >10x vs bash |
| Claude Code renders | open new session | statusline visible |
