use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

/// One entry of a Claude Code shared task list: `~/.claude/tasks/<list id>/<id>.json`.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    pub status: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

impl Task {
    pub fn in_progress(&self) -> bool {
        self.status == "in_progress"
    }

    pub fn done(&self) -> bool {
        self.status == "completed"
    }
}

fn dir(list_id: &str) -> PathBuf {
    dirs::home_dir().expect("no home directory").join(".claude/tasks").join(list_id)
}

pub fn read(list_id: &str) -> Vec<Task> {
    let Ok(entries) = fs::read_dir(dir(list_id)) else { return Vec::new() };
    let mut tasks: Vec<Task> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str(&fs::read_to_string(e.path()).ok()?).ok())
        .collect();
    tasks.sort_by_key(|t| (t.id.parse::<u64>().unwrap_or(u64::MAX), t.id.clone()));
    tasks
}

/// Empties the team's list, leaving any other file Claude keeps there (such as its lock) alone.
pub fn clear(list_id: &str) {
    let Ok(entries) = fs::read_dir(dir(list_id)) else { return };
    for e in entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")) {
        let _ = fs::remove_file(e.path());
    }
}
