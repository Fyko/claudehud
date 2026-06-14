//! The client's stdin→HUD orchestration as a deep module with a small,
//! injectable interface.
//!
//! The pure HUD assembly lives in [`crate::render::render`]; this module wraps
//! the I/O *around* it — parsing the raw stdin JSON, resolving the git segment
//! (which registers the repo with the daemon), reading the incident cache and
//! the update notice — and returns the rendered **HUD** string.
//!
//! The real entrypoint ([`main`](crate::main)) wires live stdin / cache files /
//! clock to [`run`] via [`SystemEnv`]. Tests inject fakes through the [`Env`]
//! trait so the whole stdin→HUD path can be exercised with no real `/tmp`,
//! network, `git`, or wall-clock dependency.
//!
//! Every step degrades silently per ADR-0001: a missing cache, notice, or git
//! repo just drops the corresponding segment rather than erroring.

use std::path::Path;

use common::incidents::Incident;

use crate::input::Input;
use crate::render::{self, Layout, RoundingMode};

/// The injectable environment the orchestration reads from. Static dispatch
/// (one generic param on [`run`]) keeps the real path zero-cost while letting
/// tests substitute fakes.
pub trait Env {
    /// Resolve the git segment (branch + dirty) for `cwd`, honoring payload
    /// precedence. In the real impl this also writes the **registration
    /// marker** as a side effect of the slow path (see
    /// [`crate::git::ensure_registered_for_watching`]).
    fn resolve_branch(&self, input: &Input, cwd: &Path) -> Option<(String, bool)>;

    /// Read the **incident line** source: stored incidents plus the total
    /// active count. Returns `(vec![], 0)` when the incident cache is absent.
    fn read_incidents(&self) -> (Vec<Incident>, u8);

    /// The active update notice version to advertise, or `None` when there's no
    /// live notice. The clock is the impl's concern (real wall clock vs. an
    /// injected fixed instant) so callers never touch the system clock.
    fn active_notice(&self) -> Option<String>;
}

/// Render options resolved by `main()` from CLI args + env before orchestration.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub rounding: RoundingMode,
    pub layout: Layout,
}

/// Run the stdin→HUD orchestration: raw JSON in, rendered HUD string out.
///
/// `raw` is the exact stdin payload. An empty/blank payload yields the bare
/// `"Claude"` fallback (matching the legacy behavior when stdin carries no
/// JSON). All I/O flows through `env`, so this function is pure given its
/// inputs and the `env` fakes.
pub fn run<E: Env>(raw: &str, env: &E, options: Options) -> String {
    if raw.trim().is_empty() {
        return "Claude".to_string();
    }

    let input: Input = serde_json::from_str(raw).unwrap_or_default();

    // Git segment. Resolving the branch also ensures the repo is registered for
    // watching (the registration-marker side effect lives behind `resolve_branch`).
    let git = input
        .cwd
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|cwd| env.resolve_branch(&input, Path::new(cwd)));

    let (incidents, total_active) = env.read_incidents();
    let update_notice = env.active_notice();

    render::render(
        &input,
        git,
        &incidents,
        total_active,
        update_notice.as_deref(),
        options.rounding,
        options.layout,
    )
}

/// The production [`Env`]: real git cache + registration, real incident mmap,
/// real on-disk notice file read against the real wall clock.
pub struct SystemEnv;

impl Env for SystemEnv {
    fn resolve_branch(&self, input: &Input, cwd: &Path) -> Option<(String, bool)> {
        crate::git::resolve_branch(input, cwd)
    }

    fn read_incidents(&self) -> (Vec<Incident>, u8) {
        crate::incidents::read_incidents()
    }

    fn active_notice(&self) -> Option<String> {
        crate::notice::active_notice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fully-injected environment: no `/tmp`, network, `git`, or wall clock.
    #[derive(Default)]
    struct FakeEnv {
        branch: Option<(String, bool)>,
        incidents: (Vec<Incident>, u8),
        notice: Option<String>,
    }

    impl Env for FakeEnv {
        fn resolve_branch(&self, _input: &Input, _cwd: &Path) -> Option<(String, bool)> {
            self.branch.clone()
        }
        fn read_incidents(&self) -> (Vec<Incident>, u8) {
            self.incidents.clone()
        }
        fn active_notice(&self) -> Option<String> {
            self.notice.clone()
        }
    }

    fn opts() -> Options {
        Options {
            rounding: RoundingMode::Floor,
            layout: Layout::Comfortable,
        }
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('[') => {
                    for c2 in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c2) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            if let Some('\\') = chars.peek() {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn empty_stdin_yields_bare_claude() {
        let out = run("", &FakeEnv::default(), opts());
        assert_eq!(out, "Claude");
    }

    #[test]
    fn blank_stdin_yields_bare_claude() {
        let out = run("   \n\t ", &FakeEnv::default(), opts());
        assert_eq!(out, "Claude");
    }

    #[test]
    fn garbage_json_degrades_to_default_input() {
        // unparseable JSON → Input::default() → still renders (no panic, no error).
        let out = run("not json at all", &FakeEnv::default(), opts());
        assert!(out.contains("Claude"), "fallback model name, got: {out}");
    }

    #[test]
    fn injected_branch_renders_into_git_segment() {
        let env = FakeEnv {
            branch: Some(("feature/seam".to_string(), true)),
            ..Default::default()
        };
        let out = run(crate::input::REAL_STDIN_FIXTURE, &env, opts());
        let plain = strip_ansi(&out);
        assert!(
            plain.contains("feature/seam"),
            "injected branch should appear in HUD, got: {plain}"
        );
    }
}
