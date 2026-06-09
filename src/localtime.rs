//! Dependency-free local-time helpers.
//!
//! The dashboard previously rendered all wall-clock timestamps (the Info block's
//! "started" anchor and every Activity event line) straight off the raw UTC
//! epoch — so a user in CEST (UTC+2) saw times two hours behind their wall
//! clock, even though the code *labelled* them "local". We don't want to pull in
//! `chrono`/`time` just for an offset, so we read the platform's local UTC
//! offset (`struct tm`'s `tm_gmtoff`, seconds east of UTC) once via a minimal
//! `localtime_r` FFI and cache it for the process lifetime.
//!
//! `tm_gmtoff` already folds in DST for the current instant, which is exactly
//! what a "what time is it on my wall" display wants. The offset is captured at
//! first use; a process that straddles a DST boundary keeps the offset it
//! started with (acceptable for a dashboard — it never silently shows UTC).

use std::sync::OnceLock;

/// `time_t` / `long` on the LP64 Unix targets we ship for
/// (aarch64/x86_64 macOS + linux-gnu). 64-bit on all of them.
#[allow(non_camel_case_types)]
type time_t = i64;

// `struct tm` layout per POSIX. We only read `tm_gmtoff`; the rest are present
// so the struct size matches what libc writes into. `tm_zone` is a trailing
// pointer on glibc/macOS.
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

extern "C" {
    // localtime_r(const time_t *timep, struct tm *result) -> struct tm *
    fn localtime_r(timep: *const time_t, result: *mut Tm) -> *mut Tm;
}

/// Local UTC offset in seconds (east of UTC, so CEST = +7200). Computed once
/// from the current instant and cached. Falls back to 0 (i.e. UTC, the old
/// behaviour) only if `localtime_r` ever fails — never panics on the display
/// path.
fn local_offset_secs() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as time_t)
            .unwrap_or(0);
        // SAFETY: `now` is a valid time_t; we hand libc a properly-sized,
        // zero-initialised `Tm` to fill and only read scalar fields back.
        let mut tm: Tm = unsafe { std::mem::zeroed() };
        let ret = unsafe { localtime_r(&now, &mut tm) };
        if ret.is_null() {
            0
        } else {
            tm.tm_gmtoff
        }
    })
}

/// Format a UTC epoch as a local `HH:MM:SS` string (no date, no zone suffix).
/// Used for Activity event lines.
pub fn hhmmss(epoch_utc: u64) -> String {
    let (h, m, s) = local_hms(epoch_utc);
    format!("{h:02}:{m:02}:{s:02}")
}

/// Local hour/min/sec for a UTC epoch, applying the cached offset.
pub fn local_hms(epoch_utc: u64) -> (u64, u64, u64) {
    // shift into local seconds, wrapping safely across the day boundary.
    let local = (epoch_utc as i64 + local_offset_secs()).rem_euclid(86_400) as u64;
    (local / 3600, (local / 60) % 60, local % 60)
}

/// The cached offset as a compact label like `UTC+2` / `UTC-5` / `UTC` so the
/// display can name the zone it's showing.
pub fn offset_label() -> String {
    let off = local_offset_secs();
    if off == 0 {
        return "UTC".to_string();
    }
    let sign = if off > 0 { '+' } else { '-' };
    let abs = off.abs();
    let h = abs / 3600;
    let m = (abs % 3600) / 60;
    if m == 0 {
        format!("UTC{sign}{h}")
    } else {
        format!("UTC{sign}{h}:{m:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_wraps_across_local_midnight() {
        // Pure arithmetic check on local_hms: it must stay in range and wrap.
        for epoch in [0u64, 1, 3600, 86_399, 86_400, 1_780_956_498] {
            let (h, m, s) = local_hms(epoch);
            assert!(h < 24 && m < 60 && s < 60);
        }
    }

    #[test]
    fn offset_label_is_well_formed() {
        let l = offset_label();
        assert!(l.starts_with("UTC"));
    }
}
