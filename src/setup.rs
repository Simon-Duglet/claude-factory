use serde_json::{Map, Value};
use std::{fs, io};

pub const TEAMS_VAR: &str = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";
pub const PLUGIN_DIRS_VAR: &str = "CLAUDE_CODE_PLUGIN_DIRS";

/// Where the bundled mod is written, so the install doesn't depend on the repo staying in place.
pub fn mod_dir() -> String {
    crate::team::data_dir().join("mod").display().to_string()
}

/// Where older builds loaded the mod from; dropped from the plugin dirs on install.
const STALE_MOD_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/mod");

const MOD_FILES: [(&str, &str); 3] = [
    (".claude-plugin/plugin.json", include_str!("../mod/.claude-plugin/plugin.json")),
    ("hooks/hooks.json", include_str!("../mod/hooks/hooks.json")),
    ("hooks/register.js", include_str!("../mod/hooks/register.js")),
];

/// Writes the mod bundled in this binary to mod_dir(), so it always matches the binary.
pub fn write_mod() -> io::Result<()> {
    let dir = std::path::PathBuf::from(mod_dir());
    for (name, text) in MOD_FILES {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, text)?;
    }
    Ok(())
}

fn settings_path() -> std::path::PathBuf {
    dirs::home_dir().expect("no home directory").join(".claude/settings.json")
}

/// True when agent teams are on and the mod is loaded for every session.
pub fn is_installed() -> bool {
    let Ok(text) = fs::read_to_string(settings_path()) else { return false };
    let Ok(settings) = serde_json::from_str::<Value>(&text) else { return false };
    let env = &settings["env"];
    env[TEAMS_VAR] == "1" && env[PLUGIN_DIRS_VAR].as_str().is_some_and(|d| d.split(':').any(|p| p == mod_dir()))
}

/// Turns agent teams on and loads the mod in every session, keeping the rest of the user's settings.
pub fn install() -> io::Result<()> {
    let path = settings_path();
    let original = fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
    let mut settings: Value = serde_json::from_str(&original).map_err(io::Error::other)?;
    patch(&mut settings, &mod_dir(), STALE_MOD_DIR);
    if path.exists() {
        fs::copy(&path, path.with_extension("json.bak-claude-factory"))?;
    }
    fs::write(&path, serde_json::to_string_pretty(&settings)? + "\n")
}

fn patch(settings: &mut Value, mod_dir: &str, stale: &str) {
    let root = settings.as_object_mut().expect("settings.json is not an object");
    let env = root.entry("env").or_insert_with(|| Value::Object(Map::new()));
    let env = env.as_object_mut().expect("settings.json env is not an object");
    env.insert(TEAMS_VAR.into(), "1".into());
    let dirs = env.get(PLUGIN_DIRS_VAR).and_then(Value::as_str).unwrap_or("");
    let mut dirs: Vec<&str> = dirs.split(':').filter(|d| !d.is_empty() && *d != stale).collect();
    if !dirs.contains(&mod_dir) {
        dirs.push(mod_dir);
    }
    env.insert(PLUGIN_DIRS_VAR.into(), dirs.join(":").into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_keeps_existing_settings_and_is_idempotent() {
        let mut s = json!({ "model": "opus", "env": { "FOO": "1", PLUGIN_DIRS_VAR: "/other" } });
        patch(&mut s, "/m", "/old");
        patch(&mut s, "/m", "/old");
        assert_eq!(s["model"], "opus");
        assert_eq!(s["env"]["FOO"], "1");
        assert_eq!(s["env"][TEAMS_VAR], "1");
        assert_eq!(s["env"][PLUGIN_DIRS_VAR], "/other:/m");
    }

    #[test]
    fn patch_creates_env() {
        let mut s = json!({});
        patch(&mut s, "/m", "/old");
        assert_eq!(s["env"][PLUGIN_DIRS_VAR], "/m");
    }
}
