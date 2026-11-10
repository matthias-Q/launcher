use std::{
    env,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

use emojis::Group;
use serde::Deserialize;

use crate::entries::{self, Action, Entry, EntryIcon};

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EmojiAction {
    /// Put the emoji on the clipboard.
    #[default]
    Copy,
    /// Type the emoji into the previously focused window.
    Type,
    /// Both of the above.
    Both,
}

pub fn emoji_entries() -> Vec<Entry> {
    emojis::iter()
        .map(|emoji| {
            let shortcodes: Vec<String> =
                emoji.shortcodes().map(|code| format!(":{code}:")).collect();
            let group = group_name(emoji.group());
            let comment = if shortcodes.is_empty() {
                group.to_string()
            } else {
                format!("{}  ·  {group}", shortcodes.join(" "))
            };
            Entry {
                id: emoji.as_str().to_string(),
                name: capitalize(emoji.name()),
                comment: Some(comment),
                icon: Some(EntryIcon::Glyph(emoji.as_str().to_string())),
                keywords: format!("{} {group}", shortcodes.join(" ")),
                action: Action::Emoji(emoji.as_str().to_string()),
                confirm: false,
                expires: None,
            }
        })
        .collect()
}

/// Copies and/or types `emoji`. Typing happens after a short delay so the launcher window
/// is gone and focus is back on the previous window.
pub fn deliver(
    emoji: &str,
    action: EmojiAction,
    copy_command: Option<&str>,
) -> std::io::Result<()> {
    if matches!(action, EmojiAction::Copy | EmojiAction::Both) {
        entries::copy(emoji, copy_command)?;
    }

    if matches!(action, EmojiAction::Type | EmojiAction::Both) {
        let typer = if env::var_os("WAYLAND_DISPLAY").is_some() {
            r#"sleep 0.2; exec wtype -- "$1""#
        } else {
            r#"sleep 0.2; exec xdotool type --clearmodifiers -- "$1""#
        };
        Command::new("sh")
            .args(["-c", typer, "sh", emoji])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(entries::reap)?;
    }

    Ok(())
}

fn group_name(group: Group) -> &'static str {
    match group {
        Group::SmileysAndEmotion => "Smileys & Emotion",
        Group::PeopleAndBody => "People & Body",
        Group::AnimalsAndNature => "Animals & Nature",
        Group::FoodAndDrink => "Food & Drink",
        Group::TravelAndPlaces => "Travel & Places",
        Group::Activities => "Activities",
        Group::Objects => "Objects",
        Group::Symbols => "Symbols",
        Group::Flags => "Flags",
    }
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
