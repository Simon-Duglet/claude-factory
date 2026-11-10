use serde::Deserialize;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

/// A session snapshot written by the `team-factory` mod to `~/.claude-factory/live/<session>.json`.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    pub cwd: String,
    pub updated_at: u64,
    #[serde(default)]
    pub ended: bool,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

#[derive(Clone, Deserialize)]
pub struct Agent {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
}

impl Session {
    /// The mod rewrites its snapshot every 2 s; older than that means the session is gone.
    pub fn is_live(&self) -> bool {
        !self.ended && now_ms().saturating_sub(self.updated_at) < 10_000
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents.iter().rev().find(|a| a.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name)))
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub fn read_sessions() -> Vec<Session> {
    let Ok(entries) = fs::read_dir(crate::team::data_dir().join("live")) else { return Vec::new() };
    let mut sessions: Vec<Session> = entries
        .flatten()
        .filter_map(|e| {
            let session: Session = serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok()?;
            // The mod writes one file per session forever; drop the ones untouched for a day.
            if now_ms().saturating_sub(session.updated_at) > 86_400_000 {
                let _ = fs::remove_file(e.path());
                return None;
            }
            Some(session)
        })
        .collect();
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions
}
