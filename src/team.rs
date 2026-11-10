use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Developer,
    Qa,
    Reviewer,
    Custom,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Developer, Role::Qa, Role::Reviewer, Role::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Role::Developer => "Developer",
            Role::Qa => "QA",
            Role::Reviewer => "Reviewer",
            Role::Custom => "Custom",
        }
    }

    pub fn default_instructions(self) -> &'static str {
        match self {
            Role::Developer => "Implements the requested changes, builds them, and reports what changed and how to run it.",
            Role::Qa => "Tests every change a developer hands over by running the app, reports PASS/FAIL with evidence. Never edits code.",
            Role::Reviewer => "Reviews diffs for bugs and convention breaks before work is considered done. Never edits code.",
            Role::Custom => "",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub name: String,
    pub role: Role,
    /// Empty means the lead decides (inherits the lead's model).
    pub model: String,
    pub instructions: String,
}

impl Member {
    pub fn new(name: &str, role: Role) -> Self {
        Self { name: name.into(), role, model: String::new(), instructions: role.default_instructions().into() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub enabled: bool,
    /// Sessions started in this directory (or below) get the team. Empty = every session.
    pub directory: String,
    pub rules: String,
    pub members: Vec<Member>,
    /// `--permission-mode` of the sessions the app runs; nobody can answer a prompt in them.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
}

fn default_permission_mode() -> String {
    "auto".into()
}

pub const PERMISSION_MODES: [&str; 4] = ["auto", "acceptEdits", "dontAsk", "bypassPermissions"];

impl Team {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            directory: String::new(),
            rules: String::new(),
            members: Vec::new(),
            permission_mode: default_permission_mode(),
        }
    }

    /// The lead's session name; teammates address it with SendMessage, so it must be unique on the machine.
    pub fn lead_name(&self) -> String {
        format!("{}-lead", self.name.split_whitespace().collect::<Vec<_>>().join("-"))
    }

    /// The Claude Code shared task list every agent of this team works from (`CLAUDE_CODE_TASK_LIST_ID`).
    pub fn task_list_id(&self) -> String {
        let slug: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        format!("claude-factory-{slug}")
    }

    /// Where the app starts this team's sessions.
    pub fn working_dir(&self) -> String {
        let dir = self.directory.trim();
        if dir.is_empty() { dirs::home_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "/".into()) } else { dir.into() }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Store {
    pub teams: Vec<Team>,
    /// Live thumbnails of the agents' nested desktops in the team view; off means no screenshots at all.
    #[serde(default = "yes")]
    pub desktop_preview: bool,
}

impl Default for Store {
    fn default() -> Self {
        Self { teams: Vec::new(), desktop_preview: true }
    }
}

fn yes() -> bool {
    true
}

pub fn data_dir() -> PathBuf {
    dirs::home_dir().expect("no home directory").join(".claude-factory")
}

impl Store {
    pub fn load() -> Self {
        fs::read_to_string(data_dir().join("teams.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let dir = data_dir();
        fs::create_dir_all(&dir)?;
        write_atomic(&dir.join("teams.json"), &serde_json::to_string_pretty(self)?)
    }

    /// Writes one team as pretty JSON, the same shape it has in teams.json.
    pub fn export(team: &Team, path: &std::path::Path) -> io::Result<()> {
        fs::write(path, serde_json::to_string_pretty(team)?)
    }

    /// Adds the team in `path`, renamed "Name (2)", "Name (3)"… if the name is taken; returns its index.
    pub fn import(&mut self, path: &std::path::Path) -> Result<usize, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let mut team: Team = serde_json::from_str(&text).map_err(|e| format!("not a team file: {e}"))?;
        let taken = |n: &str| self.teams.iter().any(|t| t.name == n);
        if taken(&team.name) {
            let base = team.name.clone();
            team.name = (2..).map(|i| format!("{base} ({i})")).find(|n| !taken(n)).unwrap();
        }
        self.teams.push(team);
        Ok(self.teams.len() - 1)
    }

    /// The enabled team whose directory holds `cwd` most closely; a team with no directory applies everywhere.
    pub fn team_for_dir(&self, cwd: &str) -> Option<&Team> {
        self.teams
            .iter()
            .filter(|t| t.enabled)
            .filter(|t| {
                let dir = t.directory.trim().trim_end_matches('/');
                dir.is_empty() || cwd == dir || cwd.starts_with(&format!("{dir}/"))
            })
            .max_by_key(|t| t.directory.trim().len())
    }
}

// Session-start hooks may read while we write, so never expose a half-written file.
fn write_atomic(path: &PathBuf, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_for_dir_prefers_the_closest_enabled_directory() {
        let mut global = Team::new("global");
        let mut myapp = Team::new("myapp");
        myapp.directory = "/src/MyApp/".into();
        let mut linux = Team::new("linux");
        linux.directory = "/src/MyApp/Linux".into();
        let mut off = Team::new("off");
        off.directory = "/src/MyApp/Linux/Core".into();
        off.enabled = false;
        global.directory.clear();
        let store = Store { teams: vec![global, myapp, linux, off], ..Store::default() };

        assert_eq!(store.team_for_dir("/src/MyApp/Linux/Core").unwrap().name, "linux");
        assert_eq!(store.team_for_dir("/src/MyApp").unwrap().name, "myapp");
        assert_eq!(store.team_for_dir("/src/MyAppX").unwrap().name, "global");
    }

    #[test]
    fn export_import_round_trip_suffixes_clashing_names() {
        let path = std::env::temp_dir().join(format!("cf-export-{}.json", std::process::id()));
        let mut team = Team::new("Foo");
        team.members.push(Member::new("Dev", Role::Developer));
        Store::export(&team, &path).unwrap();
        let mut store = Store { teams: vec![team, Team::new("Foo (2)")], ..Store::default() };
        let i = store.import(&path).unwrap();
        assert_eq!(store.teams[i].name, "Foo (3)");
        assert_eq!(store.teams[i].members[0].name, "Dev");
        fs::write(&path, "{\"name\": 1}").unwrap();
        assert!(store.import(&path).is_err());
        assert_eq!(store.teams.len(), 3);
        fs::remove_file(path).unwrap();
    }
}
