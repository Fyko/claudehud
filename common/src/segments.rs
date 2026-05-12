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

/// Raw TOML shape — separate from `SegmentConfig` to allow validation and defaults.
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
        let max_bytes = (seg.max_bytes.unwrap_or(64) as usize).min(SEG_PAYLOAD_MAX);
        let timeout = seg
            .timeout
            .as_deref()
            .map(|s| {
                parse_duration(s)
                    .ok_or_else(|| format!("segment[{i}] invalid timeout {s:?}"))
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

/// Load and parse config from `path`. Returns `None` if the file doesn't exist or
/// (on Unix) is world-writable. Returns `Some(vec![])` for an empty/valid config with no segments.
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
        // "café" is 5 bytes: c(1) a(1) f(1) é(2)
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
        // missing cmd
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
