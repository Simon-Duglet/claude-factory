//! Files, images and videos pasted into the chat input, and how they reach the agent.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Pasted screenshots are saved as `<temp>/<PREFIX><nanos>.png`.
const PREFIX: &str = "claude-factory-paste-";
/// The API's limit is 5 MB of base64 per image; bigger images go by path.
const MAX_INLINE: u64 = 5 * 1024 * 1024 / 4 * 3;

/// What a Ctrl+V holds, most specific first.
pub enum Paste {
    Files(Vec<PathBuf>),
    Image { width: usize, height: usize, rgba: Vec<u8> },
    Text(String),
}

/// Reads the files or image on the clipboard; `None` when it only holds text, which egui pastes itself.
pub fn read_clipboard() -> Option<Paste> {
    let mut cb = arboard::Clipboard::new().ok()?;
    if let Ok(files) = cb.get().file_list()
        && !files.is_empty()
    {
        return Some(Paste::Files(files));
    }
    let img = cb.get_image().ok()?;
    Some(Paste::Image { width: img.width, height: img.height, rgba: img.bytes.into_owned() })
}

/// The attachments a paste adds, saving a raw image under `dir`. Empty means: paste it as text.
pub fn attachments(paste: Paste, dir: &Path) -> Result<Vec<PathBuf>, String> {
    match paste {
        Paste::Files(files) => Ok(files),
        Paste::Image { width, height, rgba } => {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            let path = dir.join(format!("{PREFIX}{nanos}.png"));
            image::save_buffer(&path, &rgba, width as u32, height as u32, image::ColorType::Rgba8).map_err(|e| e.to_string())?;
            Ok(vec![path])
        }
        Paste::Text(text) => Ok(file_uris(&text).unwrap_or_default()),
    }
}

/// Removes attachment `i`, deleting the file when it is a screenshot we saved under `dir`.
pub fn remove(list: &mut Vec<PathBuf>, i: usize, dir: &Path) {
    let path = list.remove(i);
    if path.parent() == Some(dir) && path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(PREFIX)) {
        let _ = std::fs::remove_file(path);
    }
}

/// Paths of a text made only of `file://` URIs, as file managers put on the clipboard (text/uri-list, or
/// GNOME's "x-special/gnome-copied-files" with its "copy"/"cut" line first). `None` for any other text.
pub fn file_uris(text: &str) -> Option<Vec<PathBuf>> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).peekable();
    if lines.peek().is_some_and(|l| l.starts_with("x-special/")) {
        lines.next();
    }
    if lines.peek().is_some_and(|l| *l == "copy" || *l == "cut") {
        lines.next();
    }
    let paths: Option<Vec<PathBuf>> = lines
        .map(|l| {
            let rest = l.strip_prefix("file://")?;
            let path = rest.strip_prefix("localhost").unwrap_or(rest);
            path.starts_with('/').then(|| PathBuf::from(percent_decode(path)))
        })
        .collect();
    paths.filter(|p| !p.is_empty())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())).flatten();
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The user message for `text` and `files`, as (what the chat shows, stream-json `content`). Every file is
/// listed by path; images small enough also go inline so Claude sees them without reading the file.
pub fn message(text: &str, files: &[PathBuf]) -> (String, Value) {
    if files.is_empty() {
        return (text.into(), json!(text));
    }
    let list: Vec<String> = files.iter().map(|f| format!("- {}", f.display())).collect();
    let shown = format!("{text}\n\nAttached files:\n{}", list.join("\n")).trim_start().to_string();
    let mut blocks = vec![json!({ "type": "text", "text": shown })];
    for f in files {
        let media = match f.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            _ => continue,
        };
        if std::fs::metadata(f).is_ok_and(|m| m.len() <= MAX_INLINE)
            && let Ok(bytes) = std::fs::read(f)
        {
            blocks.push(json!({ "type": "image", "source": { "type": "base64", "media_type": media, "data": base64(&bytes) } }));
        }
    }
    (shown, Value::Array(blocks))
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-factory-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn raw_image_is_saved_as_png() {
        let dir = temp_dir("image");
        let rgba = [255, 0, 0, 255].repeat(6);
        let files = attachments(Paste::Image { width: 3, height: 2, rgba }, &dir).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].parent(), Some(dir.as_path()));
        let img = image::open(&files[0]).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uris("file:///home/me/My%20Video.mp4\r\nfile://localhost/tmp/a.png\n"),
            Some(vec![PathBuf::from("/home/me/My Video.mp4"), PathBuf::from("/tmp/a.png")])
        );
        assert_eq!(file_uris("x-special/gnome-copied-files\ncopy\nfile:///tmp/x"), Some(vec![PathBuf::from("/tmp/x")]));
        assert_eq!(file_uris("# comment\nfile:///tmp/%e2%9c%93"), Some(vec![PathBuf::from("/tmp/✓")]));
        let files = attachments(Paste::Files(vec!["/a/b.mov".into()]), Path::new("/tmp")).unwrap();
        assert_eq!(files, [PathBuf::from("/a/b.mov")]);
    }

    #[test]
    fn plain_text_paste_adds_nothing() {
        for text in ["hello", "see file:///tmp/x", "file:///tmp/x\nand more", "file://host/x", "", "100%"] {
            assert!(attachments(Paste::Text(text.into()), Path::new("/tmp")).unwrap().is_empty(), "{text}");
        }
    }

    #[test]
    fn removing_deletes_only_our_screenshots() {
        let dir = temp_dir("remove");
        let ours = dir.join(format!("{PREFIX}1.png"));
        let theirs = dir.join("photo.png");
        std::fs::write(&ours, "x").unwrap();
        std::fs::write(&theirs, "x").unwrap();
        let mut list = vec![theirs.clone(), ours.clone()];
        remove(&mut list, 1, &dir);
        assert_eq!(list, std::slice::from_ref(&theirs));
        assert!(!ours.exists());
        remove(&mut list, 0, &dir);
        assert!(list.is_empty());
        assert!(theirs.exists(), "a file the user copied must never be deleted");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn attachments_go_into_the_message() {
        assert_eq!(message("hi", &[]), ("hi".to_string(), json!("hi")));

        let dir = temp_dir("message");
        let png = dir.join("shot.png");
        std::fs::write(&png, b"Man").unwrap();
        let video = PathBuf::from("/videos/demo.mp4");
        let (shown, content) = message("look", &[png.clone(), video]);
        let expected = format!("look\n\nAttached files:\n- {}\n- /videos/demo.mp4", png.display());
        assert_eq!(shown, expected);
        assert_eq!(
            content,
            json!([
                { "type": "text", "text": expected },
                { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "TWFu" } }
            ])
        );
        // Attachments alone still make a message.
        assert_eq!(message("", &[PathBuf::from("/a.txt")]).0, "Attached files:\n- /a.txt");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn base64_pads() {
        assert_eq!([base64(b""), base64(b"M"), base64(b"Ma"), base64(b"Man")], ["", "TQ==", "TWE=", "TWFu"]);
    }
}
