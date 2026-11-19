//! The user's Claude plan limits, from the OAuth usage endpoint Claude Code's `/usage` reads.
//! Fetched on a background thread with curl; the token goes through curl's stdin, never argv, logs or disk.

use serde::Deserialize;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub five_hour: Option<Window>,
    #[serde(default)]
    pub seven_day: Option<Window>,
    #[serde(default)]
    pub extra_usage: Option<Extra>,
}

#[derive(Clone, Deserialize)]
pub struct Window {
    /// Percent used, 0–100.
    #[serde(default)]
    pub utilization: Option<f64>,
    #[serde(default)]
    pub resets_at: Option<String>,
}

#[derive(Clone, Deserialize)]
pub struct Extra {
    #[serde(default)]
    pub is_enabled: bool,
    /// Cents.
    #[serde(default)]
    pub monthly_limit: Option<f64>,
    /// Cents.
    #[serde(default)]
    pub used_credits: Option<f64>,
}

/// `None` until the first fetch finishes; `Some(None)` when the last fetch failed.
static LATEST: Mutex<Option<Option<Usage>>> = Mutex::new(None);

/// Starts the refresh loop. Call once.
pub fn start() {
    std::thread::spawn(|| loop {
        let usage = fetch();
        *LATEST.lock().unwrap() = Some(usage);
        std::thread::sleep(Duration::from_secs(90));
    });
}

pub fn latest() -> Option<Option<Usage>> {
    LATEST.lock().unwrap().clone()
}

fn fetch() -> Option<Usage> {
    let creds: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dirs::home_dir()?.join(".claude/.credentials.json")).ok()?).ok()?;
    let token = creds["claudeAiOauth"]["accessToken"].as_str()?;
    let mut child = Command::new("curl")
        .args(["-sf", "--max-time", "15", "-K", "-", "-H", "anthropic-beta: oauth-2025-04-20", "https://api.anthropic.com/api/oauth/usage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let config = format!("header = \"Authorization: Bearer {token}\"\n");
    child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// "at 4:10 PM" when the reset is within a day, else "Fri 9:00 AM", in local time.
pub fn reset_label(iso: &str) -> Option<String> {
    let secs = parse_iso(iso)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; localtime_r is the thread-safe variant.
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return None;
    }
    let hour = match tm.tm_hour % 12 {
        0 => 12,
        h => h,
    };
    let time = format!("{hour}:{:02} {}", tm.tm_min, if tm.tm_hour < 12 { "AM" } else { "PM" });
    if secs - now < 86_400 {
        return Some(format!("at {time}"));
    }
    let day = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][tm.tm_wday.rem_euclid(7) as usize];
    Some(format!("{day} {time}"))
}

/// Unix seconds from an RFC 3339 timestamp like `2025-11-19T21:10:00.123+00:00` or `...Z`.
fn parse_iso(s: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    // Offset: after the seconds and optional fraction.
    let rest = s.get(19..)?.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => 0,
        Some(&sign) => {
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
            let o = oh * 3600 + om * 60;
            if sign == b'-' { -o } else { o }
        }
    };
    // Days from civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - offset)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_iso() {
        assert_eq!(super::parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(super::parse_iso("2025-11-19T21:10:00.123456+00:00"), Some(1_763_586_600));
        assert_eq!(super::parse_iso("2025-11-19T16:10:00-05:00"), Some(1_763_586_600));
    }
}
