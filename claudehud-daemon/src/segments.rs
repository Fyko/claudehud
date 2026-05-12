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
            // Push the char, not just the byte, to preserve valid UTF-8
            if let Some(c) = s[i..].chars().next() {
                out.push(c);
                i += c.len_utf8();
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Run a segment command once: spawn, enforce timeout, capture, strip ANSI, truncate.
/// Returns `None` if the command times out, exits nonzero (when show_on_error=false), or fails to spawn.
pub fn run_segment(cfg: &SegmentConfig) -> Option<String> {
    let parts: Vec<&str> = cfg.cmd.split_ascii_whitespace().collect();
    if parts.is_empty() {
        return None;
    }

    let child = Command::new(parts[0])
        .args(&parts[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    // Timer thread: kill child after timeout
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
                #[cfg(unix)]
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
                #[cfg(not(unix))]
                {
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
    // wait_with_output() drains stdout+stderr to avoid pipe-buffer deadlock.
    let output = child.wait_with_output().ok();
    let _ = kill_tx.send(()); // signal timer thread to stop

    // If we hit (or exceeded) the timeout, treat as timeout regardless of exit status
    if start.elapsed() >= timeout {
        return None;
    }

    let output = output?;

    let raw_bytes = if output.status.success() {
        output.stdout
    } else if cfg.show_on_error {
        output.stderr
    } else {
        return None;
    };

    let text = String::from_utf8_lossy(&raw_bytes).into_owned();
    let cleaned = strip_ansi(&text);
    let trimmed = cleaned.trim_end_matches(['\n', '\r']).trim();
    let truncated = truncate_to_boundary(trimmed, cfg.max_bytes);
    if truncated.is_empty() {
        None
    } else {
        Some(truncated.to_string())
    }
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
    // Safety: freshly opened, sized, exclusive writer — readers use seqlock.
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

        // Sleep the interval, but wake early on shutdown
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

/// Main entry point for the segment scheduler.
/// Loads config, spawns per-segment schedulers, watches config file for hot-reload.
/// Blocks forever.
pub fn start() {
    let cfg_path = config_path();
    let mut handles = match load_config(&cfg_path) {
        Some(configs) => spawn_schedulers(configs),
        None => vec![],
    };

    // Watch the parent directory to catch atomic file replacements (editor rename pattern).
    let parent = cfg_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
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
            // Run without hot-reload — schedulers keep running
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    };

    let _ = watcher.watch(&parent, RecursiveMode::NonRecursive);

    loop {
        if event_rx.recv().is_ok() {
            // Debounce: drain any rapid-fire events from the same edit
            std::thread::sleep(Duration::from_millis(100));
            while event_rx.try_recv().is_ok() {}

            stop_schedulers(handles);
            handles = match load_config(&cfg_path) {
                Some(configs) => spawn_schedulers(configs),
                None => vec![],
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::segments::{Position, SegmentConfig};
    use std::time::Duration;

    fn make_cfg(cmd: &str) -> SegmentConfig {
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

    #[test]
    fn test_run_segment_basic() {
        let cfg = make_cfg("echo hello");
        let result = run_segment(&cfg);
        assert_eq!(result, Some("hello".to_string()));
    }

    #[test]
    fn test_run_segment_timeout() {
        let mut cfg = make_cfg("sleep 10");
        cfg.timeout = Duration::from_millis(200);
        let start = Instant::now();
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
        let cfg = make_cfg("sh -c 'exit 1'");
        assert!(run_segment(&cfg).is_none());
    }

    #[test]
    fn test_run_segment_nonzero_exit_show_on() {
        let mut cfg = make_cfg("sh -c 'echo err-msg >&2; exit 1'");
        cfg.show_on_error = true;
        let result = run_segment(&cfg);
        assert!(result.is_some());
        assert!(result.unwrap().contains("err-msg"));
    }
}
