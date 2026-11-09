use std::{env, fs, path::PathBuf};

use freya::prelude::Color;
use serde::{Deserialize, Deserializer, de::Error};

use crate::{
    clipboard::ClipboardConfig, emoji::EmojiAction, modules::ModuleKind, power::PowerConfig,
};

/// User configuration, read from `$XDG_CONFIG_HOME/launch/config.toml`.
/// Every field is optional; missing ones fall back to [`Config::default`].
#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Enabled modules, in tab order. Also the order of results in the "All" tab before
    /// anything is typed.
    pub modules: Vec<ModuleKind>,
    /// Show an "All" tab (opened by default) that searches every module at once.
    pub all_tab: bool,
    /// Window width in logical pixels.
    pub width: f32,
    /// Number of result rows shown at once; the window height follows from it.
    pub rows: usize,
    pub row_height: f32,
    pub input_height: f32,
    pub padding: f32,
    pub corner_radius: f32,
    /// Rounded window corners. Needs a compositor (picom, …); ignored without one.
    pub window_radius: f32,
    /// Blur radius for the desktop behind the launcher; 0 draws a plain background. X11 only.
    pub blur: f32,
    /// Opacity of the background color tinting the blurred desktop, from 0 to 1.
    pub blur_opacity: f32,
    /// Fade/slide in on open and slide the selection highlight.
    pub animations: bool,
    pub font_family: Option<String>,
    pub font_size: f32,
    pub comment_font_size: f32,
    /// Show the `Comment`/`GenericName` line below each app name.
    pub show_comments: bool,
    pub show_icons: bool,
    pub icon_size: f32,
    /// Icon theme name; defaults to the GTK theme from `gtk-3.0/settings.ini`, then `hicolor`.
    pub icon_theme: Option<String>,
    /// Close the launcher when it loses focus, like a popup.
    pub close_on_unfocus: bool,
    pub placeholder: Option<String>,
    /// Terminal used for `Terminal=true` apps; falls back to `$TERMINAL`, then `x-terminal-emulator`.
    pub terminal: Option<String>,
    /// Command that reads text to copy from stdin; defaults to `wl-copy` or `xclip`.
    pub copy_command: Option<String>,
    pub emoji: EmojiConfig,
    pub power: PowerConfig,
    pub clipboard: ClipboardConfig,
    pub colors: Colors,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default, deny_unknown_fields)]
pub struct EmojiConfig {
    /// `copy`, `type` or `both`.
    pub action: EmojiAction,
    /// Overrides the top-level `copy_command` for emoji.
    pub copy_command: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    #[serde(deserialize_with = "hex")]
    pub background: Color,
    #[serde(deserialize_with = "hex")]
    pub input: Color,
    #[serde(deserialize_with = "hex")]
    pub border: Color,
    #[serde(deserialize_with = "hex")]
    pub selected: Color,
    #[serde(deserialize_with = "hex")]
    pub text: Color,
    #[serde(deserialize_with = "hex")]
    pub muted: Color,
    #[serde(deserialize_with = "hex")]
    pub accent: Color,
    /// Clipboard secrets (passwords, tokens).
    #[serde(deserialize_with = "hex")]
    pub secret: Color,
    /// Drawn over the blurred desktop instead of `background`, scaled by `blur_opacity`.
    #[serde(deserialize_with = "optional_hex")]
    pub tint: Option<Color>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modules: vec![
                ModuleKind::Calc,
                ModuleKind::Apps,
                ModuleKind::Run,
                ModuleKind::Windows,
                ModuleKind::Emoji,
                ModuleKind::Power,
            ],
            all_tab: true,
            width: 680.,
            rows: 8,
            row_height: 48.,
            input_height: 46.,
            padding: 14.,
            corner_radius: 8.,
            window_radius: 14.,
            blur: 10.,
            blur_opacity: 0.45,
            animations: true,
            font_family: None,
            font_size: 15.,
            comment_font_size: 12.,
            show_comments: true,
            show_icons: true,
            icon_size: 32.,
            icon_theme: None,
            close_on_unfocus: true,
            placeholder: None,
            terminal: None,
            copy_command: None,
            emoji: EmojiConfig::default(),
            power: PowerConfig::default(),
            clipboard: ClipboardConfig::default(),
            colors: Colors::default(),
        }
    }
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            background: Color::from_rgb(24, 25, 33),
            input: Color::from_rgb(34, 36, 46),
            border: Color::from_rgb(138, 173, 255),
            selected: Color::from_rgb(52, 58, 88),
            text: Color::from_rgb(220, 222, 235),
            muted: Color::from_rgb(130, 134, 156),
            accent: Color::from_rgb(138, 173, 255),
            secret: Color::from_rgb(238, 153, 160),
            tint: None,
        }
    }
}

pub const TAB_HEIGHT: f32 = 30.;

/// Commented example config, printed by `launch --default-config`.
pub const EXAMPLE: &str = r##"# ~/.config/launch/config.toml

# Enabled modules in tab order; also the order of the "All" tab before you type.
# Available: "calc", "apps", "run", "windows", "emoji", "power", "clipboard".
# "run" lists every executable on $PATH; "All" only shows them once you type.
# "clipboard" needs the daemon: run `launch cliphist install-service` once.
modules = ["calc", "apps", "run", "windows", "emoji", "power"]
# An "All" tab, opened by default, that fuzzy searches every module at once.
all_tab = true

width = 680
rows = 8
row_height = 48
input_height = 46
padding = 14
corner_radius = 8
# Rounded window corners, needs a compositor such as picom.
window_radius = 14
# Blur the desktop behind the launcher (X11); 0 disables it.
blur = 10
# How strongly the background color tints the blurred desktop, from 0 to 1.
blur_opacity = 0.45
animations = true
# font_family = "Inter"
font_size = 15
comment_font_size = 12
show_comments = true
show_icons = true
icon_size = 32
# icon_theme = "Papirus-Dark"
close_on_unfocus = true
# placeholder = "Search applications…"
# terminal = "alacritty"
# Reads text to copy (calculator results, emoji) from stdin.
# copy_command = "xclip -selection clipboard"

[emoji]
# "copy", "type" or "both".
action = "copy"
# copy_command = "xclip -selection clipboard"

# Power menu commands; an empty string hides the entry.
[power]
# Press Enter twice to log out, reboot or shut down.
confirm = true
lock = "loginctl lock-session"
suspend = "systemctl suspend"
hibernate = ""
logout = 'loginctl terminate-session "$XDG_SESSION_ID"'
reboot = "systemctl reboot"
shutdown = "systemctl poweroff"

# Clipboard history, recorded by `launch cliphist daemon`.
[clipboard]
max_entries = 500
# Larger clipboard contents are skipped.
max_bytes = 1048576
# Passwords/tokens (password manager hint or heuristics) are kept in memory
# only (in $XDG_RUNTIME_DIR) and dropped after `secret_ttl` seconds.
detect_secrets = true
secret_ttl = 30
# Empty the clipboard when an expiring secret is still on it.
clear_expired_secrets = true

# Colors are "#rrggbb" or "#rrggbbaa".
[colors]
background = "#181921"
input = "#22242e"
border = "#8aadff"
selected = "#343a58"
text = "#dcdeeb"
muted = "#82869c"
accent = "#8aadff"
secret = "#ee99a0"
# Tint over the blurred desktop; defaults to the background color.
# tint = "#1e2240"
"##;

impl Config {
    pub fn default_path() -> Option<PathBuf> {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .map(|dir| dir.join("launch").join("config.toml"))
    }

    /// Loads the config at `path` (or the default location). A missing file yields the
    /// defaults; an invalid one is reported on stderr and also yields the defaults.
    pub fn load(path: Option<PathBuf>) -> Self {
        let Some(path) = path.or_else(Self::default_path) else {
            return Self::default();
        };
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&content).unwrap_or_else(|err| {
            eprintln!("launch: invalid config {}: {err}", path.display());
            Self::default()
        })
    }

    pub fn window_height(&self, show_tabs: bool) -> f32 {
        let tabs = if show_tabs {
            TAB_HEIGHT + self.padding
        } else {
            0.
        };
        self.padding * 3. + self.input_height + tabs + self.row_height * self.rows as f32
    }
}

fn hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Color, D::Error> {
    let value = String::deserialize(deserializer)?;
    parse_hex(&value).ok_or_else(|| D::Error::custom(format!("invalid color {value:?}")))
}

fn optional_hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Color>, D::Error> {
    hex(deserializer).map(Some)
}

fn parse_hex(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    match hex.len() {
        6 => Some(Color::from_rgb(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::from_argb(byte(6)?, byte(0)?, byte(2)?, byte(4)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        toml::from_str::<Config>(EXAMPLE).unwrap();
    }

    #[test]
    fn tint_is_optional() {
        let config: Config = toml::from_str("[colors]\ntint = \"#ff000080\"").unwrap();
        assert_eq!(config.colors.tint, Some(Color::from_argb(128, 255, 0, 0)));
        assert_eq!(toml::from_str::<Config>("").unwrap().colors.tint, None);
        assert!(toml::from_str::<Config>("[colors]\ntint = \"red\"").is_err());
    }

    #[test]
    fn parses_hex_colors() {
        assert_eq!(parse_hex("#ff0080"), Some(Color::from_rgb(255, 0, 128)));
        assert_eq!(
            parse_hex("#ff008040"),
            Some(Color::from_argb(64, 255, 0, 128))
        );
        assert_eq!(parse_hex("ff0080"), None);
        assert_eq!(parse_hex("#ff00"), None);
    }
}
