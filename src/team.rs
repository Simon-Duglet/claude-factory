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
    /// Empty means its role's model, else the lead's (`Team::model_of`).
    pub model: String,
    /// `--effort`; empty means its role's, else the lead's (`Team::effort_of`).
    #[serde(default)]
    pub effort: String,
    /// The member's own prompt, used when `override_prompt` is on.
    pub instructions: String,
    /// The name of one of the team's roles (Roles tab); empty = the built-in `role`.
    #[serde(default)]
    pub team_role: String,
    /// Use `instructions` instead of the role's prompt. Members saved before roles existed load with it on, keeping their text.
    #[serde(default = "yes")]
    pub override_prompt: bool,
}

impl Member {
    pub fn new(name: &str, role: Role) -> Self {
        Self { name: name.into(), role, model: String::new(), effort: String::new(), instructions: String::new(), team_role: String::new(), override_prompt: false }
    }

    /// What the member is called in prompts and the graph: its team role, else its built-in role.
    pub fn role_label(&self) -> &str {
        if self.team_role.trim().is_empty() { self.role.label() } else { self.team_role.trim() }
    }

    /// The prompt the member runs with: its own when overridden, else the team role of that name (a team role named
    /// like a built-in one replaces it), else the built-in role's default.
    pub fn prompt<'a>(&'a self, roles: &'a [TeamRole]) -> &'a str {
        if self.override_prompt {
            return &self.instructions;
        }
        let label = self.role_label();
        roles.iter().find(|r| r.name.trim() == label).map_or(self.role.default_instructions(), |r| &r.prompt)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    pub enabled: bool,
    /// Sessions started in this directory (or below) get the team. Empty = every session.
    pub directory: String,
    pub rules: String,
    /// `--model` of the lead the app runs, and of members whose own model is empty. Empty = Claude Code's default.
    #[serde(default)]
    pub lead_model: String,
    /// `--effort` of the lead, and of members whose own and role's effort are empty. Empty = no flag.
    #[serde(default)]
    pub lead_effort: String,
    pub members: Vec<Member>,
    /// `--permission-mode` of the sessions the app runs; nobody can answer a prompt in them.
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    /// Prompts the app sends to the lead on a schedule, while the app and the lead are running.
    #[serde(default)]
    pub routines: Vec<Routine>,
    /// Named prompts the user keeps for this team; picking one fills in a teammate's instructions.
    #[serde(default)]
    pub roles: Vec<TeamRole>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamRole {
    pub name: String,
    pub prompt: String,
    /// Default `--model` of the role's agents; empty = the lead's.
    #[serde(default)]
    pub model: String,
    /// Default `--effort` of the role's agents; empty = the lead's.
    #[serde(default)]
    pub effort: String,
    /// The MCP servers the role's agents load, by name; None = every server Claude Code would load.
    #[serde(default)]
    pub mcp_servers: Option<Vec<String>>,
}

impl TeamRole {
    pub fn new(name: &str, prompt: &str) -> Self {
        Self { name: name.into(), prompt: prompt.into(), model: String::new(), effort: String::new(), mcp_servers: None }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub enabled: bool,
    pub prompt: String,
    pub schedule: Schedule,
    /// Unix seconds of the last time it was sent; 0 = never.
    pub last_run: i64,
}

impl Routine {
    pub fn new(name: &str) -> Self {
        Self { name: name.into(), enabled: true, prompt: String::new(), schedule: Schedule::Every { n: 1, hours: true }, last_run: 0 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Schedule {
    /// Every `n` minutes, or hours when `hours`.
    Every { n: u32, hours: bool },
    /// At `hour:minute` local time on the ticked days, Monday first.
    Daily { hour: u32, minute: u32, days: [bool; 7] },
}

impl Schedule {
    /// The first time this schedule fires strictly after `t` (Unix seconds); i64::MAX if never.
    pub fn next_after(&self, t: i64) -> i64 {
        match *self {
            Schedule::Every { n, hours } => t + i64::from(n.max(1)) * if hours { 3600 } else { 60 },
            Schedule::Daily { hour, minute, days } => {
                // SAFETY: tm is plain data; localtime_r writes into it and mktime reads and normalizes it.
                let mut today: libc::tm = unsafe { std::mem::zeroed() };
                if unsafe { libc::localtime_r(&t, &mut today) }.is_null() {
                    return i64::MAX;
                }
                for offset in 0..=7 {
                    let mut tm = today;
                    tm.tm_mday += offset;
                    tm.tm_hour = hour as i32;
                    tm.tm_min = minute as i32;
                    tm.tm_sec = 0;
                    tm.tm_isdst = -1;
                    let at = unsafe { libc::mktime(&mut tm) };
                    if at > t && days[((tm.tm_wday + 6) % 7) as usize] {
                        return at;
                    }
                }
                i64::MAX
            }
        }
    }
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
            lead_model: String::new(),
            lead_effort: String::new(),
            members: Vec::new(),
            permission_mode: default_permission_mode(),
            routines: Vec::new(),
            roles: Vec::new(),
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

    /// The team role `member` is in, matched by name like `Member::prompt`.
    pub fn role_of(&self, member: &Member) -> Option<&TeamRole> {
        let label = member.role_label();
        self.roles.iter().find(|r| r.name.trim() == label)
    }

    /// The member's own model, else its role's; empty when neither is set.
    pub fn own_model(&self, member: &Member) -> String {
        first_set([&member.model, self.role_of(member).map_or("", |r| &r.model)])
    }

    /// The member's own effort, else its role's; empty when neither is set.
    pub fn own_effort(&self, member: &Member) -> String {
        first_set([&member.effort, self.role_of(member).map_or("", |r| &r.effort)])
    }

    /// The `--model` the app starts `member` with: its own, else its role's, else the lead's; empty = no flag.
    pub fn model_of(&self, member: &Member) -> String {
        first_set([&self.own_model(member), &self.lead_model])
    }

    /// The `--effort` the app starts `member` with, in the same order as `model_of`; empty = no flag.
    pub fn effort_of(&self, member: &Member) -> String {
        first_set([&self.own_effort(member), &self.lead_effort])
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
    /// Sessions the app ran, so the History view can list them by team and agent.
    #[serde(default)]
    pub history: Vec<PastSession>,
    /// The team view's background.
    #[serde(default)]
    pub theme: crate::scene::Theme,
}

/// One conversation an agent had; its transcript is Claude Code's `<id>.jsonl`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PastSession {
    pub team: String,
    pub agent: String,
    pub id: String,
    /// Unix seconds.
    pub started_at: i64,
    /// The first message, on one line.
    pub preview: String,
}

impl Default for Store {
    fn default() -> Self {
        Self { teams: Vec::new(), desktop_preview: true, history: Vec::new(), theme: Default::default() }
    }
}

fn first_set<const N: usize>(values: [&str; N]) -> String {
    values.iter().map(|v| v.trim()).find(|v| !v.is_empty()).unwrap_or("").to_string()
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
    fn members_use_their_role_prompt_unless_overridden() {
        // Saved before roles existed: no override_prompt field, so the member keeps its own text.
        let old: Member = serde_json::from_str(r#"{"name":"A","role":"Developer","model":"","instructions":"mine"}"#).unwrap();
        assert!(old.override_prompt);
        assert_eq!(old.prompt(&[]), "mine");
        let roles = vec![TeamRole::new("Developer", "team dev"), TeamRole::new("Writer", "writes")];
        let mut m = Member::new("B", Role::Developer);
        assert!(!m.override_prompt);
        assert_eq!(m.prompt(&[]), Role::Developer.default_instructions());
        assert_eq!(m.prompt(&roles), "team dev");
        m.team_role = "Writer".into();
        assert_eq!((m.role_label(), m.prompt(&roles)), ("Writer", "writes"));
        m.override_prompt = true;
        m.instructions = "own".into();
        assert_eq!(m.prompt(&roles), "own");
    }

    #[test]
    fn schedules_find_their_next_run() {
        assert_eq!(Schedule::Every { n: 15, hours: false }.next_after(1000), 1000 + 900);
        assert_eq!(Schedule::Every { n: 2, hours: true }.next_after(0), 7200);
        assert_eq!(Schedule::Daily { hour: 9, minute: 0, days: [false; 7] }.next_after(0), i64::MAX);
        // Every day at 09:30: within a day after t, at 09:30 local, and stable when asked again from the result.
        let daily = Schedule::Daily { hour: 9, minute: 30, days: [true; 7] };
        let t = 1_800_000_000;
        let next = daily.next_after(t);
        assert!(next > t && next <= t + 86_400);
        assert!(crate::history::format_time(next).ends_with("09:30"));
        assert!(daily.next_after(next) - next >= 82_800, "next day, allowing a DST shift");
        // Only one weekday ticked: the next run is within a week.
        let mut days = [false; 7];
        days[2] = true;
        let weekly = Schedule::Daily { hour: 9, minute: 30, days };
        let next = weekly.next_after(t);
        assert!(next > t && next <= t + 7 * 86_400);
        assert!(weekly.next_after(next) - next >= 6 * 86_400);
    }

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
    fn team_saved_before_the_lead_model_existed_still_loads() {
        let old = r#"{"teams":[{"name":"Old","enabled":true,"directory":"","rules":"",
            "members":[{"name":"Dev","role":"Developer","model":"","instructions":""}]}]}"#;
        let store: Store = serde_json::from_str(old).unwrap();
        assert_eq!(store.teams[0].lead_model, "");
        assert_eq!(store.teams[0].members[0].name, "Dev");
    }

    #[test]
    fn members_without_a_model_use_the_lead_model() {
        let mut team = Team::new("T");
        team.lead_model = "opus".into();
        let mut own = Member::new("A", Role::Developer);
        own.model = "haiku".into();
        assert_eq!(team.model_of(&own), "haiku");
        assert_eq!(team.model_of(&Member::new("B", Role::Qa)), "opus");
        team.lead_model.clear();
        assert_eq!(team.model_of(&Member::new("B", Role::Qa)), "");
    }

    #[test]
    fn model_and_effort_go_member_then_role_then_lead() {
        let mut team = Team::new("T");
        team.lead_model = "opus".into();
        let mut role = TeamRole::new("QA", "");
        role.model = "sonnet".into();
        role.effort = "high".into();
        team.roles.push(role);
        let qa = Member::new("B", Role::Qa);
        assert_eq!((team.model_of(&qa), team.effort_of(&qa)), ("sonnet".into(), "high".into()));
        let mut own = qa.clone();
        own.model = "haiku".into();
        own.effort = "low".into();
        assert_eq!((team.model_of(&own), team.effort_of(&own)), ("haiku".into(), "low".into()));
        let dev = Member::new("A", Role::Developer);
        assert_eq!((team.model_of(&dev), team.effort_of(&dev)), ("opus".into(), String::new()));
        team.lead_effort = "max".into();
        assert_eq!(team.effort_of(&dev), "max");
    }

    #[test]
    fn roles_saved_before_model_and_effort_still_load() {
        let old = r#"{"name":"Old","enabled":true,"directory":"","rules":"","members":[],"roles":[{"name":"QA","prompt":"p"}]}"#;
        let team: Team = serde_json::from_str(old).unwrap();
        assert_eq!((team.roles[0].model.as_str(), team.roles[0].effort.as_str(), team.lead_effort.as_str()), ("", "", ""));
        assert_eq!(team.roles[0].mcp_servers, None);
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
