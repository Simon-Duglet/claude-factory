//! Chat messages rendered as markdown: headings, emphasis, code, lists, tables and clickable links.

use crate::links;
use bevy_egui::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::sync::{Arc, Mutex};

/// Draws `text` as markdown. The returned response covers the whole message and senses hover only,
/// so the labels inside keep their text selection and link clicks.
pub fn show(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let cache = ui.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<CommonMarkCache>>>(egui::Id::new("markdown_cache")).clone());
    let mut cache = cache.lock().unwrap();
    // egui_commonmark lays table cells out without wrapping, so a table with long cells is wider than the chat.
    // Left in the log, it widened the panel to its maximum, laid the text after it out past the panel's edge, and
    // moved the panel's separator line into the middle of the chat. In a horizontal scroll area the text still
    // wraps at the visible width and only a wide table overflows, scrolling sideways within its message.
    let id = ui.next_auto_id().with("markdown");
    ui.scope(|ui| {
        egui::ScrollArea::horizontal()
            .id_salt(id)
            .auto_shrink([false, true])
            .show(ui, |ui| CommonMarkViewer::new().show(ui, &mut cache, &autolink(text)))
    })
    .response
}

/// Wraps bare http(s) URLs in `<...>` so they become links: CommonMark only links URLs written that way.
/// URLs in code, already in angle brackets or in a `[text](url)` target are left alone.
fn autolink(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (None, Some(m)) => fence = Some(m),
            (Some(f), Some(m)) if f == m => fence = None,
            _ => {}
        }
        if fence.is_some() || marker.is_some() || line.starts_with("    ") || line.starts_with('\t') {
            out.push_str(line);
            continue;
        }
        let mut at = 0;
        for r in links::find(line) {
            let before = &line[..r.start];
            let in_code = before.matches('`').count() % 2 == 1;
            let wrapped = before.ends_with('<') || before.ends_with("](") || before.ends_with("]: ");
            out.push_str(&line[at..r.start]);
            if in_code || wrapped {
                out.push_str(&line[r.clone()]);
            } else {
                out.push('<');
                out.push_str(&line[r.clone()]);
                out.push('>');
            }
            at = r.end;
        }
        out.push_str(&line[at..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::autolink;

    #[test]
    fn wraps_bare_urls_only() {
        assert_eq!(autolink("see https://a.io/x."), "see <https://a.io/x>.");
        assert_eq!(autolink("[a](https://a.io) <https://b.io>"), "[a](https://a.io) <https://b.io>");
        assert_eq!(autolink("`https://a.io` https://b.io"), "`https://a.io` <https://b.io>");
        assert_eq!(autolink("```\nhttps://a.io\n```\nhttps://b.io"), "```\nhttps://a.io\n```\n<https://b.io>");
    }
}
