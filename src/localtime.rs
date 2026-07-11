//! Local-time helpers for the display surfaces (dashboard, `agg history`, status).
//!
//! Every wall-clock timestamp we render comes off a raw UTC epoch, so without a zone conversion
//! a user in CEST (UTC+2) would see times two hours behind their wall clock while the code
//! *labelled* them "local". `jiff` does the conversion, reading the platform's tz database.
//!
//! This module used to hand-roll it: a `struct tm` layout, a `localtime_r` FFI block, and
//! Howard Hinnant's `civil_from_days` for the calendar math — with the offset captured ONCE and
//! cached for the process lifetime. That cache was a latent bug: a long run straddling a DST
//! switch kept the offset it started with and showed wrong times for the rest of the run. It
//! also had no Windows path at all (`localtime_r` is POSIX-only), so Windows silently rendered
//! UTC labelled as local.
//!
//! Both are fixed here. The offset is resolved **per call** against the current instant, so DST
//! transitions are picked up live, and jiff handles Windows properly. There is no cache and no
//! FFI: a timestamp→zone conversion is a lookup, not a syscall, and these are display paths
//! called a handful of times per render.

use jiff::{tz::TimeZone, Timestamp};

/// A UTC epoch as a `Zoned` in the system's local zone. Out-of-range epochs (and a system with
/// no resolvable tz) degrade to the epoch / UTC rather than panicking — this is a display path.
fn local(epoch_utc: u64) -> jiff::Zoned {
    Timestamp::from_second(epoch_utc as i64)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(TimeZone::system())
}

/// Format a UTC epoch as a local `HH:MM:SS` string (no date, no zone suffix).
/// Used for Activity event lines.
pub fn hhmmss(epoch_utc: u64) -> String {
    let (h, m, s) = local_hms(epoch_utc);
    format!("{h:02}:{m:02}:{s:02}")
}

/// Format a UTC epoch as a local `YYYY-MM-DD HH:MM` string. Used by `agg history`, where runs
/// span days so a date is needed (unlike the time-only `hhmmss`). 0 → "—" (no timestamp).
pub fn ymd_hms(epoch_utc: u64) -> String {
    if epoch_utc == 0 {
        return "—".to_string();
    }
    local(epoch_utc).strftime("%Y-%m-%d %H:%M").to_string()
}

/// Local hour/min/sec for a UTC epoch.
pub fn local_hms(epoch_utc: u64) -> (u64, u64, u64) {
    let z = local(epoch_utc);
    (z.hour() as u64, z.minute() as u64, z.second() as u64)
}

/// The CURRENT local offset as a compact label like `UTC+2` / `UTC-5:30` / `UTC`, so the display
/// can name the zone it's showing.
pub fn offset_label() -> String {
    let off = TimeZone::system().to_offset(Timestamp::now()).seconds();
    if off == 0 {
        return "UTC".to_string();
    }
    let sign = if off > 0 { '+' } else { '-' };
    let abs = off.abs();
    let (h, m) = (abs / 3600, (abs % 3600) / 60);
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
        // must stay in range for every epoch, whatever the local offset does to it.
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

    #[test]
    fn ymd_hms_zero_is_dash() {
        assert_eq!(ymd_hms(0), "—");
    }

    #[test]
    fn ymd_hms_is_well_formed() {
        // 1_700_000_000 = 2023-11-14 ~22:13 UTC. Local offset shifts it, but the SHAPE is fixed.
        let s = ymd_hms(1_700_000_000);
        // YYYY-MM-DD HH:MM
        assert_eq!(s.len(), 16, "got {s:?}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], " ");
        assert_eq!(&s[13..14], ":");
    }

    #[test]
    fn renders_a_known_instant_in_a_known_zone() {
        // Pin the zone so this asserts a real VALUE, not just a shape: the old hand-rolled
        // civil_from_days had a dedicated test and its replacement must earn the same trust.
        // 1_700_000_000 = 2023-11-14 22:13:20 UTC → 23:13:20 in Berlin (CET, UTC+1 in November).
        let z = Timestamp::from_second(1_700_000_000)
            .unwrap()
            .to_zoned(TimeZone::get("Europe/Berlin").unwrap());
        assert_eq!(z.strftime("%Y-%m-%d %H:%M:%S").to_string(), "2023-11-14 23:13:20");
        // ...and the same instant in July is CEST (UTC+2) — the DST fold the cached-offset
        // implementation used to get wrong on a long-running loop.
        let summer = Timestamp::from_second(1_688_000_000) // 2023-06-29 00:53:20 UTC
            .unwrap()
            .to_zoned(TimeZone::get("Europe/Berlin").unwrap());
        assert_eq!(summer.strftime("%Y-%m-%d %H:%M:%S").to_string(), "2023-06-29 02:53:20");
    }
}
