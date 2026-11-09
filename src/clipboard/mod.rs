//! Clipboard history: a daemon records every copied text, the module lists it.
//! Passwords and tokens are kept apart and forgotten after a short time.

pub mod backend;
pub mod daemon;
pub mod secret;
pub mod service;
pub mod store;

use serde::Deserialize;

use crate::entries::{Action, Entry, EntryIcon};
use store::{Item, Store};

/// Characters of an entry that are searched; keeps huge entries from slowing down typing.
const SEARCH_LIMIT: usize = 4096;
const PREVIEW_LIMIT: usize = 200;

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    /// Number of (non-secret) entries kept in the history.
    pub max_entries: usize,
    /// Clipboard contents larger than this many bytes are not recorded.
    pub max_bytes: usize,
    /// Treat entries that look like passwords or tokens as secrets.
    pub detect_secrets: bool,
    /// Seconds a secret stays in the history. Secrets are never written to disk.
    pub secret_ttl: u64,
    /// Also empty the clipboard when an expiring secret is still on it.
    pub clear_expired_secrets: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_entries: 500,
            max_bytes: 1 << 20,
            detect_secrets: true,
            secret_ttl: 30,
            clear_expired_secrets: true,
        }
    }
}

/// The history, newest first. Secrets only match by the word "secret".
pub fn clipboard_entries() -> Vec<Entry> {
    let mut store = Store::load();
    store.expire();
    let now = store::now();
    store
        .items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let keywords = if item.is_secret() {
                "secret".to_string()
            } else {
                match item.text.char_indices().nth(SEARCH_LIMIT) {
                    Some((end, _)) => item.text[..end].to_string(),
                    None => item.text.clone(),
                }
            };
            Entry {
                // Texts can be huge; the position is enough within one session.
                id: format!("{i}:{}", item.time),
                name: preview(&item),
                comment: (!item.is_secret()).then(|| details(&item, now)),
                icon: Some(EntryIcon::Glyph(
                    if item.is_secret() { "🔒" } else { "📋" }.to_string(),
                )),
                keywords,
                confirm: false,
                expires: item.expires,
                action: Action::Clipboard(item.text),
            }
        })
        .collect()
}

/// Removes `text` from the stored history.
pub fn delete(text: &str) -> std::io::Result<()> {
    let mut store = Store::load();
    store.remove(text);
    store.save()
}

/// Prints the history, newest first, one entry per line. Secrets are masked.
pub fn list() {
    let mut store = Store::load();
    store.expire();
    for item in &store.items {
        println!("{}", preview(item));
    }
}

/// Deletes the whole history, including secrets.
pub fn clear() -> std::io::Result<()> {
    Store::default().save()
}

/// One line describing `item`: its first non-empty line, or a mask for secrets.
fn preview(item: &Item) -> String {
    if item.is_secret() {
        return "•".repeat(item.text.chars().count().clamp(6, 16));
    }
    let line = item
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let mut preview: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.chars().count() > PREVIEW_LIMIT {
        preview = preview.chars().take(PREVIEW_LIMIT).collect();
        preview.push('…');
    }
    preview
}

fn details(item: &Item, now: u64) -> String {
    let mut details = age(now.saturating_sub(item.time) / 1000);
    let lines = item.text.trim_end().lines().count();
    if lines > 1 {
        details.push_str(&format!("  ·  {lines} lines"));
    }
    let chars = item.text.chars().count();
    if chars > PREVIEW_LIMIT {
        details.push_str(&format!("  ·  {chars} chars"));
    }
    details
}

fn age(seconds: u64) -> String {
    match seconds {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m ago", seconds / 60),
        3_600..86_400 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(text: &str, secret: bool) -> Item {
        Item {
            text: text.to_string(),
            time: 0,
            expires: secret.then_some(u64::MAX),
        }
    }

    #[test]
    fn previews() {
        assert_eq!(preview(&item("\n  foo   bar\nbaz", false)), "foo bar");
        assert_eq!(preview(&item("hunter2", true)), "•••••••");
    }

    #[test]
    fn details_count_lines_and_age() {
        let item = item("a\nb\n", false);
        assert_eq!(details(&item, 90_000), "1m ago  ·  2 lines");
    }
}
