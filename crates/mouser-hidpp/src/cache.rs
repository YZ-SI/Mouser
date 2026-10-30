//! Last-known-good device cache (`last_device.json`). Ports `_cache_dir`, `_cache_path`,
//! `_load_last_device_cache`, `_save_last_device_cache`, `_candidate_signature`,
//! `_candidate_match_score`. The cache is only an ordering hint (triage #255): a miss, a corrupt
//! file or a stale entry never prevents a full scan.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::transport::IfaceInfo;

const SCHEMA_VERSION: i64 = 1;
const TTL_SECONDS: f64 = 60.0 * 60.0 * 24.0 * 30.0;

/// Default cache location, `%APPDATA%\Mouser\last_device.json` (`_cache_path`).
pub fn default_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("Mouser").join("last_device.json"))
}

/// Cached interface identity (`candidate` object).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CandidateSig {
    /// Product id.
    pub pid: u16,
    /// Usage page.
    pub usage_page: u16,
    /// Usage.
    pub usage: u16,
    /// Transport label (`"Bluetooth"` or empty).
    pub transport: String,
    /// Candidate source.
    pub source: String,
    /// OS path (hint only).
    pub path: String,
}

impl CandidateSig {
    /// Signature of an enumerated interface; ports `_candidate_signature`.
    pub fn of(info: &IfaceInfo) -> Self {
        Self {
            pid: info.pid,
            usage_page: info.usage_page,
            usage: info.usage,
            transport: if info.bluetooth { "Bluetooth".into() } else { String::new() },
            source: info.source.to_owned(),
            path: info.path.clone(),
        }
    }

    /// 0 no match, 1 pid+usage, 2 +source, 3 +path; ports `_candidate_match_score`.
    pub fn score(&self, cached: &CandidateSig) -> u8 {
        if self.pid != cached.pid
            || self.usage_page != cached.usage_page
            || self.usage != cached.usage
        {
            return 0;
        }
        let mut s = 1;
        if !cached.source.is_empty() && self.source == cached.source {
            s += 1;
        }
        if !cached.path.is_empty() && self.path == cached.path {
            s += 1;
        }
        s
    }
}

/// Cached device tuple (`device` object plus candidate).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceCache {
    /// Interface that worked last time.
    pub candidate: CandidateSig,
    /// Device name.
    pub name: String,
    /// HID++ device index.
    pub dev_idx: u8,
    /// Transport label.
    pub transport: String,
    /// REPROG_V4 feature index (informational).
    pub feat_idx_reprog: Option<u8>,
}

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Read the cache; None on missing, malformed, expired or wrong version. Never panics.
pub fn load(path: &Path) -> Option<DeviceCache> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    if v.get("version")?.as_i64()? != SCHEMA_VERSION {
        return None;
    }
    let saved_at = v.get("saved_at").and_then(Value::as_f64).unwrap_or(0.0);
    if saved_at != 0.0 && now_secs() - saved_at > TTL_SECONDS {
        return None;
    }
    let c = v.get("candidate")?.as_object()?;
    let d = v.get("device")?.as_object()?;
    let num = |o: &serde_json::Map<String, Value>, k: &str| o.get(k).and_then(Value::as_u64);
    let text = |o: &serde_json::Map<String, Value>, k: &str| {
        o.get(k).and_then(Value::as_str).unwrap_or("").to_owned()
    };
    Some(DeviceCache {
        candidate: CandidateSig {
            pid: u16::try_from(num(c, "pid")?).ok()?,
            usage_page: u16::try_from(num(c, "usage_page")?).ok()?,
            usage: u16::try_from(num(c, "usage")?).ok()?,
            transport: text(c, "transport"),
            source: text(c, "source"),
            path: text(c, "path"),
        },
        name: text(d, "name"),
        dev_idx: u8::try_from(num(d, "dev_idx")?).ok()?,
        transport: text(d, "transport"),
        feat_idx_reprog: num(d, "feat_idx_reprog").and_then(|n| u8::try_from(n).ok()),
    })
}

/// Atomically persist the cache (tmp file + rename); failures are returned, callers ignore them.
pub fn save(path: &Path, c: &DeviceCache) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let payload = json!({
        "version": SCHEMA_VERSION,
        "saved_at": now_secs(),
        "candidate": {
            "pid": c.candidate.pid, "usage_page": c.candidate.usage_page,
            "usage": c.candidate.usage, "transport": c.candidate.transport,
            "source": c.candidate.source, "path": c.candidate.path,
        },
        "device": {
            "name": c.name, "dev_idx": c.dev_idx, "transport": c.transport,
            "feat_idx_reprog": c.feat_idx_reprog,
        },
    });
    let tmp = dir.join(".last_device.tmp");
    let result = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(&payload)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DeviceCache {
        DeviceCache {
            candidate: CandidateSig {
                pid: 0xC52B,
                usage_page: 0xFF00,
                usage: 2,
                transport: String::new(),
                source: "hidapi-enumerate".into(),
                path: "p1".into(),
            },
            name: "MX Master 2S".into(),
            dev_idx: 1,
            transport: "USB Receiver".into(),
            feat_idx_reprog: Some(9),
        }
    }

    #[test]
    fn roundtrip_and_corrupt_cache() {
        let dir = std::env::temp_dir().join(format!("mouser-hidpp-cache-{}", std::process::id()));
        let p = dir.join("last_device.json");
        save(&p, &sample()).unwrap();
        assert_eq!(load(&p), Some(sample()));
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(load(&p), None);
        std::fs::write(&p, r#"{"version":2,"candidate":{},"device":{}}"#).unwrap();
        assert_eq!(load(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn match_score_levels() {
        let c = sample().candidate;
        let mut other = c.clone();
        assert_eq!(other.score(&c), 3);
        other.path = "p2".into();
        assert_eq!(other.score(&c), 2);
        other.source = "x".into();
        assert_eq!(other.score(&c), 1);
        other.usage = 9;
        assert_eq!(other.score(&c), 0);
    }
}
