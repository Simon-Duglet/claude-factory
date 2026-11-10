use crate::team::{Member, Team};
use std::fmt::Write;

/// The text Claude receives at session start for this team.
pub fn render(team: &Team) -> String {
    let mut out = format!("# Agent team: {}\n\n", team.name);
    if !team.rules.trim().is_empty() {
        let _ = write!(out, "## Team rules\n{}\n\n", team.rules.trim());
    }
    if team.members.is_empty() {
        return out;
    }
    out.push_str(
        "## Roster\n\
         You are the team lead. When work calls for the team, spawn these teammates with the Agent tool, \
         setting `name` exactly as written. Reuse a teammate with SendMessage instead of spawning a duplicate, \
         and do not spawn teammates outside this roster unless the user asks.\n",
    );
    for m in &team.members {
        let _ = write!(out, "- **{}**: {}", m.name.trim(), m.role.label());
        if !m.model.trim().is_empty() {
            let _ = write!(out, " (model: {})", m.model.trim());
        }
        if !m.instructions.trim().is_empty() {
            let _ = write!(out, ". {}", m.instructions.trim());
        }
        out.push('\n');
    }
    out
}

/// The system prompt of one teammate the app runs as its own session.
/// `tag` is appended to every session name (`<name>-<tag>`) so names stay unique on the machine.
pub fn render_member(team: &Team, me: &Member, tag: &str) -> String {
    let mut out = format!("# Agent team: {}\n\n", team.name);
    if !team.rules.trim().is_empty() {
        let _ = write!(out, "## Team rules\n{}\n\n", team.rules.trim());
    }
    let _ = write!(out, "## You\nYou are **{}**, the {} of this team.", tagged(me.name.trim(), tag), me.role.label());
    if !me.instructions.trim().is_empty() {
        let _ = write!(out, " {}", me.instructions.trim());
    }
    out.push_str(" The user talks to you directly from the Claude Factory app.\n");
    out.push_str(
        "\n## Shared task list\n\
         The team shares one task list, which the user watches. When you start a task, TaskUpdate it to \
         `in_progress` with `owner` set to your name; mark it `completed` when it is done. Check TaskList for \
         unblocked tasks that are yours.\n",
    );
    out.push_str(
        "\n## Your teammates\n\
         Each teammate is a separate Claude Code session on this machine. Reach one with the SendMessage tool, \
         using the exact name below as `to`. Their messages reach you the same way; reply to the sender when \
         they ask for something. Do not spawn subagents to play a teammate's role.\n",
    );
    let _ = writeln!(out, "- **{}**: Team lead. Assigns work; report back to it when you finish.", tagged(&team.lead_name(), tag));
    {
        for m in team.members.iter().filter(|m| m.name.trim() != me.name.trim()) {
            let _ = write!(out, "- **{}**: {}", tagged(m.name.trim(), tag), m.role.label());
            if !m.instructions.trim().is_empty() {
                let _ = write!(out, ". {}", m.instructions.trim());
            }
            out.push('\n');
        }
    }
    out
}

/// The system prompt of the lead the app runs: the user talks to it, and it delegates to the members.
pub fn render_lead(team: &Team, tag: &str) -> String {
    let mut out = format!("# Agent team: {}\n\n", team.name);
    if !team.rules.trim().is_empty() {
        let _ = write!(out, "## Team rules\n{}\n\n", team.rules.trim());
    }
    out.push_str(
        "## You\n\
         You are the team lead. The user talks to you from the Claude Factory app. Break their requests into work \
         for your teammates, hand it over, follow up, enforce the team rules, and report the outcome to the user. \
         Do not do a teammate's work yourself and do not spawn subagents or teammates for it.\n\n\
         ## Shared task list\n\
         The whole team shares one task list. Plan with it: TaskCreate one task per piece of work, with `owner` set \
         to the teammate who does it and `blockedBy` for ordering (QA after the developer), then give the teammate \
         the task id when you hand it over. Keep the list true: it is what the user watches.\n\n\
         ## Your teammates\n\
         Each teammate is already running as a separate Claude Code session on this machine. Reach one with the \
         SendMessage tool, using the exact name below as `to`. Their replies arrive as messages and start a new \
         turn for you, so end your turn after delegating instead of waiting.\n",
    );
    for m in &team.members {
        let _ = write!(out, "- **{}**: {}", tagged(m.name.trim(), tag), m.role.label());
        if !m.instructions.trim().is_empty() {
            let _ = write!(out, ". {}", m.instructions.trim());
        }
        out.push('\n');
    }
    out
}

/// The session name of `name` in a launch tagged `tag`.
pub fn tagged(name: &str, tag: &str) -> String {
    if tag.is_empty() { name.into() } else { format!("{name}-{tag}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::Role;

    #[test]
    fn member_prompt_names_self_and_lists_only_others() {
        let mut team = Team::new("MyApp");
        team.rules = "Never create PRs.".into();
        team.members.push(Member::new("Alice", Role::Developer));
        team.members.push(Member::new("Bob", Role::Qa));

        let text = render_member(&team, &team.members[0], "");
        assert!(text.contains("You are **Alice**, the Developer"));
        assert!(text.contains("Never create PRs."));
        assert!(text.contains("- **Bob**: QA"));
        assert!(text.contains("- **MyApp-lead**: Team lead"));
        assert!(!text.contains("- **Alice**"));

        let lead = render_lead(&team, "");
        assert!(lead.contains("You are the team lead"));
        assert!(lead.contains("Shared task list") && text.contains("Shared task list"));
        assert!(lead.contains("- **Alice**: Developer") && lead.contains("- **Bob**: QA"));
    }

    #[test]
    fn renders_rules_and_roster() {
        let mut team = Team::new("Linux MyApp");
        team.rules = "All developer work is tested by QA. Never create PRs.".into();
        team.members.push(Member::new("Alice", Role::Developer));
        let mut qa = Member::new("Bob", Role::Qa);
        qa.model = "sonnet".into();
        team.members.push(qa);

        let text = render(&team);
        assert!(text.starts_with("# Agent team: Linux MyApp\n"));
        assert!(text.contains("Never create PRs."));
        assert!(text.contains("- **Alice**: Developer. Implements"));
        assert!(text.contains("- **Bob**: QA (model: sonnet). Tests"));
    }

    #[test]
    fn empty_team_has_no_roster() {
        assert!(!render(&Team::new("x")).contains("Roster"));
    }
}
