// claudehud/src/segments.rs

use std::fs;
use std::path::Path;

use common::segments::{
    config_path, load_config, seg_path_in, seqlock_read_seg, Position, SegmentConfig, SEG_MMAP_SIZE,
};
use memmap2::Mmap;

/// A resolved segment ready to splice into render output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentOutput {
    pub text: String,
    pub position: Position,
}

/// Load config and read all segment mmap files. Returns outputs in config order.
/// Missing or empty cache files are silently skipped.
pub fn read_segments() -> Vec<SegmentOutput> {
    let path = config_path();
    let configs = match load_config(&path) {
        Some(c) => c,
        None => return vec![],
    };
    read_segments_from(&common::cache_dir(), &configs)
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
        let buf = vec![0u8; SEG_MMAP_SIZE]; // zero-initialized = empty payload
        std::fs::write(&path, &buf).unwrap();

        let cfg = make_cfg("empty", Position::EndLine1);
        let result = read_segment_from(dir.path(), &cfg);
        assert!(result.is_none());
    }
}
