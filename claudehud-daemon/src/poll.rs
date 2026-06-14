//! One deep poll loop shared by the two daemon pollers (incidents + autoupdate).
//!
//! Both pollers have the same shape: build a client, optionally wait an initial
//! delay, then forever do a conditional GET (with etag handling), and when the
//! body changed, decode it and act. Only the *source* (which URL, how to decode)
//! and the *action* differ. This module owns the shape; the network client and
//! the clock are injected at the [`ConditionalGet`] / [`Clock`] seam so a test
//! can drive a full cycle with no real HTTP and no real sleep.
//!
//! Per ADR-0001 (silent degradation) every step no-ops on failure: a fetch
//! error is logged and the loop simply waits for the next tick; a `NotModified`
//! reply leaves prior state in place.

use std::time::Duration;

/// Outcome of one conditional GET against a polled source.
pub enum FetchOutcome {
    /// Server replied 304 — body unchanged since the supplied etag.
    NotModified,
    /// Fresh body, plus the new etag to send on the next request (if any).
    Body { body: String, etag: Option<String> },
}

/// A source that can be fetched with conditional-GET / etag semantics.
///
/// Implementors map an optional `If-None-Match` etag to either
/// [`FetchOutcome::NotModified`] (304) or a fresh [`FetchOutcome::Body`].
pub trait ConditionalGet {
    fn fetch(&self, etag: Option<&str>) -> Result<FetchOutcome, String>;
}

/// The clock/sleep source driving the loop's cadence.
///
/// Returning `false` from [`Clock::sleep`] stops the loop; the real clock always
/// sleeps and returns `true` (loops forever), while a fake clock can return
/// `false` to let a test exit after a fixed number of cycles.
pub trait Clock {
    /// Sleep for `dur`. Return `true` to keep polling, `false` to stop.
    fn sleep(&self, dur: Duration) -> bool;
}

/// The production clock: a real `thread::sleep` that never stops the loop.
pub struct RealClock;

impl Clock for RealClock {
    fn sleep(&self, dur: Duration) -> bool {
        std::thread::sleep(dur);
        true
    }
}

/// Poll `source` on `clock`'s cadence, calling `on_body` with each fresh body.
///
/// Runs `first_delay` (if any) once up front, then loops: fetch → on a fresh
/// body, hand it to `on_body`; on 304, do nothing; on fetch error, log and carry
/// on → sleep `interval`. The loop ends only when the clock's `sleep` returns
/// `false` (production never does; tests do, to terminate the cycle).
///
/// `label` prefixes the silent-degradation warnings so the two pollers stay
/// distinguishable in logs.
pub fn run_poll_loop<S, C, F>(
    source: &S,
    clock: &C,
    label: &str,
    first_delay: Option<Duration>,
    interval: Duration,
    mut on_body: F,
) where
    S: ConditionalGet,
    C: Clock,
    F: FnMut(&str),
{
    let mut etag: Option<String> = None;
    if let Some(delay) = first_delay {
        if !clock.sleep(delay) {
            return;
        }
    }
    loop {
        match source.fetch(etag.as_deref()) {
            Ok(FetchOutcome::NotModified) => {}
            Ok(FetchOutcome::Body {
                body,
                etag: new_etag,
            }) => {
                etag = new_etag;
                on_body(&body);
            }
            Err(e) => eprintln!("WARN {label} fetch: {e}"),
        }
        if !clock.sleep(interval) {
            return;
        }
    }
}

/// Adapter: a [`ConditionalGet`] backed by a real `ureq` agent fetching one URL.
pub struct UreqSource {
    agent: ureq::Agent,
    url: &'static str,
}

impl UreqSource {
    pub fn new(agent: ureq::Agent, url: &'static str) -> Self {
        Self { agent, url }
    }
}

impl ConditionalGet for UreqSource {
    fn fetch(&self, etag: Option<&str>) -> Result<FetchOutcome, String> {
        let mut req = self.agent.get(self.url);
        if let Some(tag) = etag {
            req = req.set("If-None-Match", tag);
        }
        match req.call() {
            Ok(resp) => {
                let new_etag = resp.header("ETag").map(str::to_string);
                let body = resp.into_string().map_err(|e| e.to_string())?;
                Ok(FetchOutcome::Body {
                    body,
                    etag: new_etag,
                })
            }
            Err(ureq::Error::Status(304, _)) => Ok(FetchOutcome::NotModified),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Test-only fakes shared by this module's tests and the two adapter modules
/// (`status`, `update`), so each adapter can drive a full poll cycle with no
/// real HTTP and no real sleep.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::cell::RefCell;

    /// A fake source that replays a scripted sequence of fetch outcomes and
    /// records the etag it was asked to send on each call.
    pub(crate) struct FakeSource {
        scripted: RefCell<std::collections::VecDeque<Result<FetchOutcome, String>>>,
        pub(crate) seen_etags: RefCell<Vec<Option<String>>>,
    }

    impl FakeSource {
        pub(crate) fn new(scripted: Vec<Result<FetchOutcome, String>>) -> Self {
            Self {
                scripted: RefCell::new(scripted.into_iter().collect()),
                seen_etags: RefCell::new(Vec::new()),
            }
        }
    }

    impl ConditionalGet for FakeSource {
        fn fetch(&self, etag: Option<&str>) -> Result<FetchOutcome, String> {
            self.seen_etags.borrow_mut().push(etag.map(str::to_string));
            self.scripted
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(FetchOutcome::NotModified))
        }
    }

    /// A fake clock that records every sleep and stops the loop once it has
    /// returned `true` (kept polling) `keep_for` times — no real time passes.
    pub(crate) struct FakeClock {
        keep_for: RefCell<usize>,
        pub(crate) slept: RefCell<Vec<Duration>>,
    }

    impl FakeClock {
        /// Keep the loop alive across `n` sleeps, then stop on the next one.
        pub(crate) fn keep_for(n: usize) -> Self {
            Self {
                keep_for: RefCell::new(n),
                slept: RefCell::new(Vec::new()),
            }
        }
    }

    impl Clock for FakeClock {
        fn sleep(&self, dur: Duration) -> bool {
            self.slept.borrow_mut().push(dur);
            let mut left = self.keep_for.borrow_mut();
            if *left == 0 {
                return false;
            }
            *left -= 1;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeClock, FakeSource};
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn full_cycle_fetch_etag_decide_act_no_real_io() {
        // Cycle 1: fresh body with an etag → on_body called, etag captured.
        // Cycle 2: 304 Not Modified → on_body NOT called, but the captured etag
        //          is sent back as If-None-Match.
        let source = FakeSource::new(vec![
            Ok(FetchOutcome::Body {
                body: "first".to_string(),
                etag: Some("etag-1".to_string()),
            }),
            Ok(FetchOutcome::NotModified),
        ]);
        let clock = FakeClock::keep_for(2);
        let acted: RefCell<Vec<String>> = RefCell::new(Vec::new());

        run_poll_loop(
            &source,
            &clock,
            "test",
            Some(Duration::from_secs(7)),
            Duration::from_secs(300),
            |body| acted.borrow_mut().push(body.to_string()),
        );

        // Acted exactly once, on the fresh body only.
        assert_eq!(acted.borrow().as_slice(), &["first".to_string()]);
        // First request sent no etag; second sent the etag from cycle 1.
        assert_eq!(
            source.seen_etags.borrow().as_slice(),
            &[None, Some("etag-1".to_string())]
        );
        // The first-delay was honored before the interval sleeps.
        assert_eq!(
            clock.slept.borrow().as_slice(),
            &[
                Duration::from_secs(7),
                Duration::from_secs(300),
                Duration::from_secs(300)
            ]
        );
    }

    #[test]
    fn fetch_error_no_ops_and_keeps_polling() {
        // A fetch error must not act and must not abort the loop (ADR-0001).
        let source = FakeSource::new(vec![
            Err("boom".to_string()),
            Ok(FetchOutcome::Body {
                body: "recovered".to_string(),
                etag: None,
            }),
        ]);
        let clock = FakeClock::keep_for(1);
        let acted: RefCell<Vec<String>> = RefCell::new(Vec::new());

        run_poll_loop(
            &source,
            &clock,
            "test",
            None,
            Duration::from_secs(60),
            |body| acted.borrow_mut().push(body.to_string()),
        );

        // The error cycle acted on nothing; the loop survived to the next tick.
        assert_eq!(acted.borrow().as_slice(), &["recovered".to_string()]);
    }
}
