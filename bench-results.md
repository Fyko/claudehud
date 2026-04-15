# benchmark results

machine: apple m-series mac  
date: 2026-04-15  
tool: hyperfine 1.20.0 (500 runs, 20 warmup)  
binary: `target/release/claudehud` (release profile: opt-level=3, lto, strip)

payload: full JSON with model, cwd, context_window, session, rate_limits, pointing at this repo

## results

| scenario | tz impl | mean | min | max |
|---|---|---|---|---|
| cold (no daemon, git subprocess fallback) | `libc` | 9.0ms ± 1.0ms | 7.1ms | 14.9ms |
| warm (daemon running, mmap cache hit) | `libc` | 3.5ms ± 0.9ms | 2.2ms | 14.4ms |
| warm (daemon running, mmap cache hit) | `time` crate | **2.6ms ± 0.7ms** | **1.4ms** | 10.9ms |

switching from `libc::localtime_r` to `time` crate (`local-offset` feature) shaved ~0.9ms mean / 0.8ms min off warm path.
hypothesis: `time` crate reads `/etc/localtime` (or `$TZ`) in pure rust more efficiently than C stdlib's TZ machinery invoked per-call via `localtime_r`.

## notes

- cold path: daemon not running, no `/tmp/clhud-*.bin` files → every invocation spawns `git` subprocess
- warm path: daemon was running, cache file existed at `/tmp/clhud-{fnv32}.bin` (138 bytes), client reads via mmap + seqlock
- hyperfine warns about shell startup overhead at sub-5ms; use `--shell=none` with a wrapper for tighter numbers
- current impl uses `time` crate — `libc` dep removed
