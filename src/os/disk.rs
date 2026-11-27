//! Free disk space: the floor below which a run stops, and the measurement it is checked against.
//!
//! A full disk fails things in the wrong place (a verifier that can't write, a scanner that can't read its database)
//! and the cause is lost. The run stops instead, at a boundary, with the cause, and resumes when space is back.

use std::path::Path;

/// 1 GB, decimal (the unit disks are sold in and `df -H` shows).
const GB: f64 = 1_000_000_000.0;

/// The least free space a run needs: a size, a share of the disk, or no check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Floor {
    /// no check (`false`)
    Off,
    /// free bytes (`10GB`)
    Bytes(u64),
    /// free share of the file system, 0 to 100 (`5%`)
    Percent(f64),
}

impl Default for Floor {
    /// 10 GB: an absolute floor, because a share would stop every run on a disk that is mostly full but has room.
    fn default() -> Self {
        Floor::Bytes(10_000_000_000)
    }
}

impl Floor {
    /// `false`, `off`, `none`, a share (`5%`) or a size (`500MB`, `10GB`, `1.5TB`; bare digits are bytes).
    pub fn parse(text: &str) -> Result<Floor, String> {
        let t = text.trim().to_ascii_lowercase();
        if matches!(t.as_str(), "false" | "off" | "none") {
            return Ok(Floor::Off);
        }
        if let Some(p) = t.strip_suffix('%') {
            return match p.trim().parse::<f64>() {
                Ok(v) if v > 0.0 && v <= 100.0 => Ok(Floor::Percent(v)),
                _ => Err(format!("`{text}`: a share is above 0 and at most 100 %")),
            };
        }
        let (number, unit) = t.split_at(t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len()));
        let factor = match unit.trim() {
            "" | "b" => 1.0,
            "kb" => 1e3,
            "mb" => 1e6,
            "gb" => GB,
            "tb" => 1e12,
            other => return Err(format!("`{text}`: unknown unit `{other}` (KB, MB, GB, TB)")),
        };
        match number.trim().parse::<f64>() {
            Ok(v) if v >= 0.0 => Ok(Floor::Bytes((v * factor) as u64)),
            _ => Err(format!("`{text}`: not a size like 10GB, a share like 5%, or false")),
        }
    }

    /// The floor as written back to a person.
    pub fn show(&self) -> String {
        match self {
            Floor::Off => "off".into(),
            Floor::Bytes(b) => format!("{:.1} GB", *b as f64 / GB),
            Floor::Percent(p) => format!("{p} %"),
        }
    }

    /// Whether `free` of `total` bytes is below the floor.
    pub fn below(&self, free: u64, total: u64) -> bool {
        match self {
            Floor::Off => false,
            Floor::Bytes(b) => free < *b,
            Floor::Percent(p) => total > 0 && (free as f64 / total as f64) * 100.0 < *p,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Floor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Flag(false) => Ok(Floor::Off),
            Raw::Flag(true) => Ok(Floor::default()),
            Raw::Text(t) => Floor::parse(&t).map_err(serde::de::Error::custom),
        }
    }
}

impl schemars::JsonSchema for Floor {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DiskFloor".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "the least free disk space a run needs: a size (`10GB`), a share (`5%`) or `false` for no check",
            "anyOf": [{"type": "string"}, {"type": "boolean"}]
        })
    }
}

/// Why a run in `project` must stop for lack of disk space, or `None`: the project's file system and the temp
/// folder's are both held to the floor.
pub fn run_shortfall(floor: Floor, project: &Path) -> Option<String> {
    shortfall(floor, &[project, &std::env::temp_dir()])
}

/// `(free, total)` bytes of the file system holding `path`, or `None` where it can't be asked (and on Windows).
/// `RLT_TEST_FREE_DISK=<free>:<total>` stands in for the measurement, for tests that spawn the binary.
pub fn free(path: &Path) -> Option<(u64, u64)> {
    if let Ok(fake) = std::env::var("RLT_TEST_FREE_DISK") {
        let (f, t) = fake.split_once(':')?;
        return Some((f.parse().ok()?, t.parse().ok()?));
    }
    measured(path)
}

#[cfg(unix)]
fn measured(path: &Path) -> Option<(u64, u64)> {
    let s = nix::sys::statvfs::statvfs(path).ok()?;
    let unit = s.fragment_size() as u64;
    Some((s.blocks_available() as u64 * unit, s.blocks() as u64 * unit))
}

#[cfg(not(unix))]
fn measured(_: &Path) -> Option<(u64, u64)> {
    None
}

/// Why the run must stop: the first of `paths` whose file system has less free space than the floor; `None` when all
/// have enough, when the floor is off, or when the space can't be measured.
pub fn shortfall(floor: Floor, paths: &[&Path]) -> Option<String> {
    paths.iter().find_map(|p| {
        let (free, total) = free(p)?;
        floor.below(free, total).then(|| {
            let pct = if total > 0 { free as f64 / total as f64 * 100.0 } else { 0.0 };
            format!(
                "free disk on {} is {:.1} GB ({:.0} %), below the floor {}: free space, then resume (`rlt … --resume`)",
                p.display(),
                free as f64 / GB,
                pct,
                floor.show()
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A size, a share, off, and what is refused.
    #[test]
    fn a_floor_parses_a_size_a_share_or_off() {
        assert_eq!(Floor::parse("10GB"), Ok(Floor::Bytes(10_000_000_000)));
        assert_eq!(Floor::parse("500 mb"), Ok(Floor::Bytes(500_000_000)));
        assert_eq!(Floor::parse("1.5TB"), Ok(Floor::Bytes(1_500_000_000_000)));
        assert_eq!(Floor::parse("2048"), Ok(Floor::Bytes(2048)));
        assert_eq!(Floor::parse("5%"), Ok(Floor::Percent(5.0)));
        for off in ["false", "OFF", "none"] {
            assert_eq!(Floor::parse(off), Ok(Floor::Off));
        }
        for bad in ["0%", "101%", "ten", "5XB", "-3GB"] {
            assert!(Floor::parse(bad).is_err(), "{bad}");
        }
    }

    /// Below the floor is below by size or by share; at the floor is not below; off never is.
    #[test]
    fn below_compares_by_size_or_by_share() {
        assert!(Floor::Bytes(10).below(9, 100));
        assert!(!Floor::Bytes(10).below(10, 100));
        assert!(Floor::Percent(5.0).below(4, 100));
        assert!(!Floor::Percent(5.0).below(5, 100));
        assert!(!Floor::Percent(5.0).below(0, 0), "an unknown total is no evidence");
        assert!(!Floor::Off.below(0, 100));
    }

    /// YAML takes a size, a share, `false` and `true` (the default).
    #[test]
    fn yaml_reads_a_size_a_share_false_and_true() {
        let f = |s: &str| serde_yaml::from_str::<Floor>(s);
        assert_eq!(f("10GB").unwrap(), Floor::Bytes(10_000_000_000));
        assert_eq!(f("'5%'").unwrap(), Floor::Percent(5.0));
        assert_eq!(f("false").unwrap(), Floor::Off);
        assert_eq!(f("true").unwrap(), Floor::default());
        assert!(f("nope").is_err());
    }

    /// The message names the path, the free space and the floor, and says how to go on.
    #[test]
    fn the_shortfall_message_says_what_to_do() {
        // a real, existing path: its free space is more than 1 byte, so only the floor of "all of it" is missed
        let dir = std::env::temp_dir();
        let msg = shortfall(Floor::Percent(100.0), &[&dir]);
        let msg = msg.expect("100 % free is never met");
        assert!(msg.contains("below the floor 100 %") && msg.contains("resume"), "{msg}");
        assert!(shortfall(Floor::Off, &[&dir]).is_none());
        assert!(shortfall(Floor::Bytes(1), &[&dir]).is_none());
    }
}
