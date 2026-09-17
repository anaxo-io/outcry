//! Shared by both benchmarks: environment parsing and core pinning.
//!
//! Pinning is what turns a benchmark on a multi-complex CPU from a coin flip into an
//! experiment. Without it, the scheduler decides each run whether writer and reader share
//! an L3, and the numbers differ by 5x between the two outcomes. `OUTCRY_PIN=0,1,2` pins
//! the writer to core 0 and readers to 1 and 2; unset, threads go wherever they land.

#![allow(dead_code)]

pub fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// A comma-separated list from the environment; unset or empty is `None`.
pub fn env_list(key: &str) -> Option<Vec<usize>> {
    std::env::var(key)
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .filter(|v: &Vec<usize>| !v.is_empty())
}

/// The core list from `OUTCRY_PIN`, checked to be long enough for a writer plus `readers`.
pub fn pin_list(readers: usize) -> Option<Vec<usize>> {
    let pins = env_list("OUTCRY_PIN")?;
    assert!(
        pins.len() > readers,
        "OUTCRY_PIN names {} cores; need {} (one writer + {readers} readers)",
        pins.len(),
        readers + 1
    );
    Some(pins)
}

/// Pin the calling thread to one core. Linux only; a no-op elsewhere so the benchmark
/// still runs, just unpinned.
pub fn pin_to(core: usize) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: cpu_set_t is plain data zero-initialised as libc documents; the macros
        // and the call only read and write that local.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(core, &mut set);
            let rc = libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
            assert_eq!(
                rc,
                0,
                "sched_setaffinity to core {core} failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = core;
}

pub fn describe_pinning(pins: &Option<Vec<usize>>) -> String {
    match pins {
        Some(p) => format!("pinned to cores {p:?}"),
        None => "unpinned".to_string(),
    }
}
