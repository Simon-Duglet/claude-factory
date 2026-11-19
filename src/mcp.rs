//! The MCP servers an agent can load, and the `claude` flags that limit it to some of them.
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;

/// One MCP server Claude Code would load in a directory.
#[derive(Clone, Debug)]
pub struct Server {
    pub name: String,
    /// "user", "local", "project" or "claude.ai".
    pub source: &'static str,
    /// The server's entry in its config file; None for claude.ai connectors, which only Claude Code can start.
    pub config: Option<Value>,
}

impl Server {
    fn connector(&self) -> bool {
        self.config.is_none()
    }
}

/// The servers a session started in `cwd` loads: user and local scope from ~/.claude.json, project scope from
/// `cwd`/.mcp.json, and the claude.ai connectors this account has connected. A later scope wins a name clash.
pub fn available(cwd: &str) -> Vec<Server> {
    let home = dirs::home_dir().unwrap_or_default();
    let read = |p: &Path| fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(Value::Null);
    from_configs(&read(&home.join(".claude.json")), &read(&Path::new(cwd).join(".mcp.json")), cwd)
}

/// `available`, re-read at most every few seconds: the UI asks every frame and ~/.claude.json can be large.
pub fn available_cached(cwd: &str) -> Vec<Server> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(String, Instant, Vec<Server>)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    match &*cache {
        Some((dir, at, list)) if dir == cwd && at.elapsed() < Duration::from_secs(5) => list.clone(),
        _ => {
            let list = available(cwd);
            *cache = Some((cwd.into(), Instant::now(), list.clone()));
            list
        }
    }
}

fn from_configs(claude_json: &Value, mcp_json: &Value, cwd: &str) -> Vec<Server> {
    let mut out: Vec<Server> = Vec::new();
    let mut add = |servers: &Value, source: &'static str| {
        for (name, config) in servers.as_object().into_iter().flatten() {
            out.retain(|s| s.name != *name);
            out.push(Server { name: name.clone(), source, config: Some(config.clone()) });
        }
    };
    add(&claude_json["mcpServers"], "user");
    add(&mcp_json["mcpServers"], "project");
    add(&claude_json["projects"][cwd.trim_end_matches('/')]["mcpServers"], "local");
    for name in claude_json["claudeAiMcpEverConnected"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if !out.iter().any(|s| s.name == name) {
            out.push(Server { name: name.into(), source: "claude.ai", config: None });
        }
    }
    out
}

/// The `claude` flags that load only the `selected` servers out of `all`.
/// Config-file servers go through `--strict-mcp-config --mcp-config`, which also drops every claude.ai connector,
/// so that the others never start. Connectors can't be passed that way: when one is selected, all servers load
/// and the unselected ones have their tools denied instead.
pub fn launch_args(all: &[Server], selected: &[String]) -> Vec<String> {
    let picked = |s: &&Server| selected.contains(&s.name);
    if !all.iter().filter(picked).any(Server::connector) {
        let servers: Map<String, Value> = all.iter().filter(picked).filter_map(|s| Some((s.name.clone(), s.config.clone()?))).collect();
        return vec!["--strict-mcp-config".into(), "--mcp-config".into(), json!({ "mcpServers": servers }).to_string()];
    }
    let denied: Vec<String> = all.iter().filter(|s| !selected.contains(&s.name)).map(|s| format!("mcp__{}", tool_prefix(&s.name))).collect();
    if denied.is_empty() { Vec::new() } else { std::iter::once("--disallowedTools".to_string()).chain(denied).collect() }
}

/// How Claude Code names a server in its tool names (`mcp__<prefix>__<tool>`).
fn tool_prefix(name: &str) -> String {
    let mut p: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if name.starts_with("claude.ai ") {
        while p.contains("__") {
            p = p.replace("__", "_");
        }
        p = p.trim_matches('_').to_string();
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn servers() -> Vec<Server> {
        let claude_json = json!({
            "mcpServers": { "a": { "command": "a" }, "b": { "command": "b-user" } },
            "projects": { "/w": { "mcpServers": { "b": { "command": "b-local" } } } },
            "claudeAiMcpEverConnected": ["claude.ai Slack", "claude.ai Docs (Beta)"],
        });
        from_configs(&claude_json, &json!({ "mcpServers": { "p": { "command": "p" } } }), "/w/")
    }

    #[test]
    fn lists_every_scope_with_the_closest_winning() {
        let all = servers();
        let names: Vec<(&str, &str)> = all.iter().map(|s| (s.name.as_str(), s.source)).collect();
        assert_eq!(names, [("a", "user"), ("p", "project"), ("b", "local"), ("claude.ai Slack", "claude.ai"), ("claude.ai Docs (Beta)", "claude.ai")]);
        assert_eq!(all[2].config.as_ref().unwrap()["command"], "b-local");
    }

    #[test]
    fn strict_config_unless_a_connector_is_picked() {
        let all = servers();
        let args = launch_args(&all, &["b".into()]);
        assert_eq!(args[..2], ["--strict-mcp-config", "--mcp-config"]);
        assert_eq!(serde_json::from_str::<Value>(&args[2]).unwrap(), json!({ "mcpServers": { "b": { "command": "b-local" } } }));
        assert_eq!(launch_args(&all, &[])[2], r#"{"mcpServers":{}}"#);
        let args = launch_args(&all, &["a".into(), "claude.ai Slack".into()]);
        assert_eq!(args, ["--disallowedTools", "mcp__p", "mcp__b", "mcp__claude_ai_Docs_Beta"]);
    }
}
