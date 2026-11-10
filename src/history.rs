//! Past agent conversations, read back from the transcripts Claude Code keeps in
//! `~/.claude/projects/<encoded cwd>/<session id>.jsonl`.

use crate::runner::{Entry, tool_summary};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// One past conversation of an agent, as listed in the History view.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub id: String,
    /// Unix seconds.
    pub started_at: i64,
    pub preview: String,
    /// Where the transcript is, if Claude still has it.
    pub path: Option<PathBuf>,
}

fn projects_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join("projects"))
}

/// Claude names a project's folder after its cwd with every character but ASCII letters and digits turned into '-'.
fn project_dir(cwd: &str) -> Option<PathBuf> {
    let name: String = cwd.trim_end_matches('/').chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    Some(projects_dir()?.join(name))
}

/// The transcript of session `id`: in the cwd's project folder, else in any project folder.
pub fn find(cwd: &str, id: &str) -> Option<PathBuf> {
    let file = format!("{id}.jsonl");
    let direct = project_dir(cwd)?.join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    std::fs::read_dir(projects_dir()?).ok()?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file())
}

/// A session name without the app's launch tag ("Developer 1-3fa9" → "Developer 1").
pub fn untag(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((base, tag)) if tag.len() == 4 && tag.bytes().all(|b| b.is_ascii_hexdigit()) => base,
        _ => name,
    }
}

/// Transcripts in the cwd's project folder whose session was named after `agent` (launch tag aside).
pub fn discover(cwd: &str, agent: &str) -> Vec<Summary> {
    let Some(dir) = project_dir(cwd) else { return Vec::new() };
    let Ok(files) = std::fs::read_dir(dir) else { return Vec::new() };
    files
        .flatten()
        .map(|f| f.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|p| {
            let (name, summary) = summarize(&p)?;
            (untag(&name) == agent).then_some(summary)
        })
        .collect()
}

/// The session's name, start time and first message, from the head of its transcript.
pub fn summarize(path: &Path) -> Option<(String, Summary)> {
    let file = std::fs::File::open(path).ok()?;
    let mut name = None;
    let mut summary = Summary { id: path.file_stem()?.to_string_lossy().into(), path: Some(path.into()), ..Default::default() };
    // The name and first message sit near the top; a cap keeps a huge unrelated transcript cheap.
    for line in BufReader::new(file).lines().map_while(Result::ok).take(300) {
        let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
        if name.is_none() && event["type"] == "agent-name" {
            name = event["agentName"].as_str().map(String::from);
        }
        if summary.started_at == 0
            && let Some(t) = event["timestamp"].as_str().and_then(parse_time)
        {
            summary.started_at = t;
        }
        if summary.preview.is_empty()
            && let Some(first) = entries_of(&event).into_iter().find_map(|e| match e {
                Entry::User(t) => Some(t),
                Entry::Incoming { from, text } => Some(format!("From {from}: {text}")),
                _ => None,
            })
        {
            summary.preview = preview(&first);
        }
        if name.is_some() && summary.started_at != 0 && !summary.preview.is_empty() {
            break;
        }
    }
    Some((name?, summary))
}

/// The whole conversation, for the read-only view and to show again when the session is resumed.
pub fn load(path: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).flat_map(|e| entries_of(&e)).collect()
}

/// One line, at most 100 characters.
pub fn preview(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 100 { format!("{}…", flat.chars().take(99).collect::<String>()) } else { flat }
}

fn entries_of(event: &Value) -> Vec<Entry> {
    if event["isSidechain"] == true {
        return Vec::new();
    }
    let content = &event["message"]["content"];
    match event["type"].as_str() {
        Some("user") => {
            let text = match content {
                Value::String(s) => s.clone(),
                Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str().filter(|_| b["type"] == "text")).collect::<Vec<_>>().join("\n"),
                _ => return Vec::new(),
            };
            if let Some(entry) = cross_session(&text) {
                return vec![entry];
            }
            // Meta messages are what Claude adds on its own (skill bodies, reminders), not what anyone typed.
            if text.trim().is_empty() || event["isMeta"] == true { Vec::new() } else { vec![Entry::User(text)] }
        }
        Some("assistant") => content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") => Some(Entry::Assistant(b["text"].as_str().unwrap_or("").into())),
                Some("tool_use") => Some(Entry::Tool(tool_summary(b))),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A teammate's message, as Claude wraps it: `<cross-session-message from-name="…" …>text</cross-session-message>`.
fn cross_session(text: &str) -> Option<Entry> {
    let start = text.find("<cross-session-message")?;
    let tag = &text[start..];
    let body_start = tag.find('>')? + 1;
    let body = &tag[body_start..tag.find("</cross-session-message>").unwrap_or(tag.len())];
    let from = tag[..body_start]
        .split_once("from-name=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or("another session", |(name, _)| untag(name));
    Some(Entry::Incoming { from: from.into(), text: body.trim().into() })
}

/// "2026-11-10T19:08:25.553Z" (UTC) → Unix seconds.
fn parse_time(s: &str) -> Option<i64> {
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i32>().ok();
    // SAFETY: tm is plain data; timegm only reads it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = num(0..4)? - 1900;
    tm.tm_mon = num(5..7)? - 1;
    tm.tm_mday = num(8..10)?;
    tm.tm_hour = num(11..13)?;
    tm.tm_min = num(14..16)?;
    tm.tm_sec = num(17..19)?;
    Some(unsafe { libc::timegm(&mut tm) })
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Local "YYYY-MM-DD HH:MM".
pub fn format_time(secs: i64) -> String {
    // SAFETY: localtime_r writes only into tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn untags_only_a_four_hex_suffix() {
        assert_eq!(untag("Developer 1-3fa9"), "Developer 1");
        assert_eq!(untag("MyApp-lead-0042"), "MyApp-lead");
        assert_eq!(untag("MyApp-lead"), "MyApp-lead");
        assert_eq!(untag("Dev-xyz1"), "Dev-xyz1");
    }

    #[test]
    fn reads_transcript_lines() {
        let typed = json!({"type":"user","message":{"content":[{"type":"text","text":"build it"}]}});
        let teammate = json!({"type":"user","isMeta":true,"message":{"content":
            "Another Claude session sent a message:\n<cross-session-message from=\"uds:/x.sock\" from-name=\"Lead-ab12\">\nTask #3\n</cross-session-message>\n\nThis came from…"}});
        let reminder = json!({"type":"user","isMeta":true,"message":{"content":"<system-reminder>x</system-reminder>"}});
        let result = json!({"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}});
        let reply = json!({"type":"assistant","message":{"content":[{"type":"thinking"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}},{"type":"text","text":"done"}]}});
        let shown: Vec<String> = [typed, teammate, reminder, result, reply]
            .iter()
            .flat_map(entries_of)
            .map(|e| match e {
                Entry::User(t) => format!("you:{t}"),
                Entry::Incoming { from, text } => format!("{from}:{text}"),
                Entry::Assistant(t) => format!("out:{t}"),
                Entry::Tool(t) => format!("tool:{t}"),
                Entry::Error(t) => format!("err:{t}"),
            })
            .collect();
        assert_eq!(shown, ["you:build it", "Lead:Task #3", "tool:Bash: ls", "out:done"]);
    }

    #[test]
    fn parses_utc_timestamps() {
        assert_eq!(parse_time("1970-01-02T00:00:01.5Z"), Some(86401));
        assert_eq!(parse_time("garbage"), None);
    }
}
