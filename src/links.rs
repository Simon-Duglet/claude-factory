//! Labels whose http(s) URLs are drawn as links and open in the default browser on click.

use bevy_egui::egui::{self, Color32, Stroke, text::LayoutJob, text_selection::LabelSelectionState};
use std::ops::Range;

/// A selectable label like `ui.label`, in `color` (the normal text color when `None`), with clickable URLs.
pub fn label(ui: &mut egui::Ui, text: &str, color: Option<Color32>) -> egui::Response {
    let links = find(text);
    if links.is_empty() {
        let rich = egui::RichText::new(text);
        return ui.label(if let Some(c) = color { rich.color(c) } else { rich });
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let link_color = ui.visuals().hyperlink_color;
    let mut job = LayoutJob::default();
    let mut at = 0;
    for r in &links {
        // PLACEHOLDER takes the color passed when painting.
        job.append(&text[at..r.start], 0.0, egui::TextFormat::simple(font.clone(), Color32::PLACEHOLDER));
        let format = egui::TextFormat { underline: Stroke::new(1.0, link_color), ..egui::TextFormat::simple(font.clone(), link_color) };
        job.append(&text[r.clone()], 0.0, format);
        at = r.end;
    }
    job.append(&text[at..], 0.0, egui::TextFormat::simple(font, Color32::PLACEHOLDER));

    let (pos, galley, response) = egui::Label::new(job).selectable(true).layout_in_ui(ui);
    let hovered_link = response.hover_pos().and_then(|p| {
        let cursor = galley.cursor_from_pos(p - pos);
        // The cursor snaps to the nearest character: make sure the pointer is actually over text.
        let at = galley.pos_from_cursor(cursor).translate(pos.to_vec2());
        if (at.min.x - p.x).abs() > 12.0 || p.y < at.min.y - 2.0 || p.y > at.max.y + 2.0 {
            return None;
        }
        let byte = text.char_indices().nth(cursor.index.0).map_or(text.len(), |(b, _)| b);
        links.iter().find(|r| r.start <= byte && byte <= r.end).map(|r| &text[r.clone()])
    });
    let response = match hovered_link {
        Some(url) => response.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(url),
        None => response,
    };
    if let (true, Some(url)) = (response.clicked(), hovered_link) {
        open(url);
    }
    if ui.is_rect_visible(response.rect) {
        let fallback = color.unwrap_or_else(|| ui.visuals().text_color());
        LabelSelectionState::label_text_selection(ui, &response, pos, galley, fallback, Stroke::NONE);
    }
    response
}

/// Opens `url` in the default browser, off the UI thread so the child is reaped.
pub fn open(url: &str) {
    let url = url.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("xdg-open").arg(url).status();
    });
}

/// Byte ranges of the http(s) URLs in `text`, without trailing punctuation or an unmatched closing bracket.
pub fn find(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("http").map(|i| i + from) {
        let rest = &text[i..];
        let scheme = ["https://", "http://"].into_iter().find(|s| rest.starts_with(s));
        let preceded_by_word = text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let Some(scheme) = scheme.filter(|_| !preceded_by_word) else {
            from = i + 4;
            continue;
        };
        let len = rest.find(|c: char| c.is_whitespace() || "<>\"'`".contains(c)).unwrap_or(rest.len());
        let mut url = &rest[..len];
        loop {
            let before = url.len();
            url = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
            let unmatched = |open, close| url.ends_with(close) && url.matches(close).count() > url.matches(open).count();
            if unmatched('(', ')') || unmatched('[', ']') {
                url = &url[..url.len() - 1];
            }
            if url.len() == before {
                break;
            }
        }
        if url.len() > scheme.len() {
            found.push(i..i + url.len());
        }
        from = i + len.max(4);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::find;

    fn urls(text: &str) -> Vec<&str> {
        find(text).into_iter().map(|r| &text[r]).collect()
    }

    #[test]
    fn finds_urls_without_surrounding_punctuation() {
        assert_eq!(urls("see https://a.com/x."), ["https://a.com/x"]);
        assert_eq!(urls("(http://a.com/b_(c)) and https://b.org"), ["http://a.com/b_(c)", "https://b.org"]);
        assert_eq!(urls("[docs](https://d.io/p?q=1#h)"), ["https://d.io/p?q=1#h"]);
        assert_eq!(urls("<https://x.y>, **https://z.w**"), ["https://x.y", "https://z.w"]);
        assert_eq!(urls("no http here, nor https:// or xhttps://a.b"), Vec::<&str>::new());
        assert_eq!(urls("été https://é.fr/à"), ["https://é.fr/à"]);
    }
}
