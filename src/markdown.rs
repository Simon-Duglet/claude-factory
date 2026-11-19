//! Chat messages rendered as markdown: headings, emphasis, code, lists, tables and clickable links.

use crate::links;
use bevy_egui::egui::{self, text::LayoutJob};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::sync::{Arc, Mutex};

/// Draws `text` as markdown. The returned response covers the whole message and senses hover only,
/// so the labels inside keep their text selection and link clicks.
///
/// Never grows wider than the space it is given: text wraps, while tables and code blocks that cannot
/// wrap scroll sideways in their own strip instead of widening the chat panel.
pub fn show(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let cache = ui.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<CommonMarkCache>>>(egui::Id::new("markdown_cache")).clone());
    let mut cache = cache.lock().unwrap();
    ui.scope(|ui| {
        for (i, block) in split_tables(text).into_iter().enumerate() {
            ui.push_id(i, |ui| {
                sideways(ui, |ui| match block {
                    Block::Text(t) => {
                        CommonMarkViewer::new().show(ui, &mut cache, &autolink(t));
                    }
                    Block::Table(rows) => table(ui, &rows),
                });
            });
        }
    })
    .response
}

/// Lays out `add` at most as wide as `ui`; whatever is wider scrolls sideways (wheel or scroll bar,
/// not drag, so text selection still works).
fn sideways(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.scope(|ui| {
        // A solid bar takes its own room under the block, only when it overflows; the default floating bar
        // widened over the block's last line while hovered.
        ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
        egui::ScrollArea::horizontal()
            .max_width(ui.available_width())
            .scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR | egui::scroll_area::ScrollSource::MOUSE_WHEEL)
            .show(ui, add);
    });
}

enum Block<'a> {
    Text(&'a str),
    Table(Vec<Vec<String>>),
}

/// Cuts `text` into plain markdown and GitHub-style tables (outside code fences).
fn split_tables(text: &str) -> Vec<Block<'_>> {
    let lines: Vec<(usize, &str)> = text
        .split_inclusive('\n')
        .scan(0, |at, l| {
            let start = *at;
            *at += l.len();
            Some((start, l))
        })
        .collect();
    let mut blocks = Vec::new();
    let mut fence: Option<&str> = None;
    let mut text_start = 0;
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].1.trim();
        if let Some(m) = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m)) {
            fence = match fence {
                None => Some(m),
                Some(f) if f == m => None,
                f => f,
            };
        }
        let starts_table = fence.is_none() && trimmed.starts_with('|') && lines.get(i + 1).is_some_and(|(_, l)| is_separator(l));
        if !starts_table {
            i += 1;
            continue;
        }
        let start = lines[i].0;
        if start > text_start {
            blocks.push(Block::Text(&text[text_start..start]));
        }
        let mut rows = vec![cells(lines[i].1)];
        i += 2;
        while i < lines.len() && lines[i].1.trim().starts_with('|') {
            rows.push(cells(lines[i].1));
            i += 1;
        }
        blocks.push(Block::Table(rows));
        text_start = lines.get(i).map_or(text.len(), |(s, _)| *s);
    }
    if text_start < text.len() {
        blocks.push(Block::Text(&text[text_start..]));
    }
    blocks
}

/// `|---|:--:|` and the like.
fn is_separator(line: &str) -> bool {
    let t = line.trim().trim_matches('|');
    !t.is_empty() && t.split('|').all(|c| {
        let c = c.trim().trim_matches(':');
        !c.is_empty() && c.chars().all(|ch| ch == '-')
    })
}

/// The cells of a table row; `\|` and pipes inside code spans stay in the cell.
fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').filter(|r| !r.ends_with('\\')).unwrap_or(t);
    let mut out = vec![String::new()];
    let mut code = false;
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => out.last_mut().unwrap().push(chars.next().unwrap()),
            '`' => {
                code = !code;
                out.last_mut().unwrap().push(c);
            }
            '|' if !code => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out.iter().map(|c| c.trim().to_string()).collect()
}

/// A table whose cells never wrap: each column is as wide as its widest cell, so cells cannot overlap.
fn table(ui: &mut egui::Ui, rows: &[Vec<String>]) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        egui::Grid::new("table").striped(true).show(ui, |ui| {
            for (r, row) in rows.iter().enumerate() {
                for c in 0..columns {
                    ui.label(inline(ui, row.get(c).map_or("", String::as_str), r == 0));
                }
                ui.end_row();
            }
        });
    });
}

/// A cell's inline markdown: `code`, **bold** (the header row is all bold); other markers are dropped.
fn inline(ui: &egui::Ui, text: &str, header: bool) -> LayoutJob {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let (normal, strong) = (ui.visuals().text_color(), ui.visuals().strong_text_color());
    let mut job = LayoutJob::default();
    let (mut code, mut bold) = (false, header);
    let mut run = String::new();
    let flush = |job: &mut LayoutJob, run: &mut String, code: bool, bold: bool| {
        if run.is_empty() {
            return;
        }
        let format = if code {
            egui::TextFormat { background: ui.visuals().code_bg_color, ..egui::TextFormat::simple(mono.clone(), normal) }
        } else {
            egui::TextFormat::simple(body.clone(), if bold { strong } else { normal })
        };
        job.append(&std::mem::take(run), 0.0, format);
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            flush(&mut job, &mut run, code, bold);
            code = !code;
        } else if !code && c == '*' && chars.peek() == Some(&'*') {
            chars.next();
            flush(&mut job, &mut run, code, bold);
            bold = !bold || header;
        } else {
            run.push(c);
        }
    }
    flush(&mut job, &mut run, code, bold);
    job
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
    use super::{Block, autolink, split_tables};

    #[test]
    fn splits_out_tables() {
        let md = "Hi\n\n| A | B |\n|---|:-:|\n| `x|y` | **z** |\n| 1 |\nafter\n```\n| a |\n|---|\n```\n";
        let blocks = split_tables(md);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], Block::Text("Hi\n\n")));
        let Block::Table(rows) = &blocks[1] else { panic!() };
        assert_eq!(rows, &vec![vec!["A", "B"], vec!["`x|y`", "**z**"], vec!["1"]]);
        assert!(matches!(blocks[2], Block::Text(t) if t.starts_with("after") && t.contains("| a |")));
    }

    #[test]
    fn wraps_bare_urls_only() {
        assert_eq!(autolink("see https://a.io/x."), "see <https://a.io/x>.");
        assert_eq!(autolink("[a](https://a.io) <https://b.io>"), "[a](https://a.io) <https://b.io>");
        assert_eq!(autolink("`https://a.io` https://b.io"), "`https://a.io` <https://b.io>");
        assert_eq!(autolink("```\nhttps://a.io\n```\nhttps://b.io"), "```\nhttps://a.io\n```\n<https://b.io>");
    }
}
