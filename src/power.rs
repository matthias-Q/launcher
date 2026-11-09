use serde::Deserialize;

use crate::entries::{Action, Entry, EntryIcon};

/// Commands for the power menu. An empty command hides the entry.
#[derive(Deserialize, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct PowerConfig {
    /// Ask for a second Enter before logging out, rebooting or shutting down.
    pub confirm: bool,
    pub lock: String,
    pub suspend: String,
    pub hibernate: String,
    pub logout: String,
    pub reboot: String,
    pub shutdown: String,
}

impl Default for PowerConfig {
    fn default() -> Self {
        Self {
            confirm: true,
            lock: "loginctl lock-session".to_string(),
            suspend: "systemctl suspend".to_string(),
            hibernate: String::new(),
            logout: r#"loginctl terminate-session "$XDG_SESSION_ID""#.to_string(),
            reboot: "systemctl reboot".to_string(),
            shutdown: "systemctl poweroff".to_string(),
        }
    }
}

pub fn power_entries(config: &PowerConfig) -> Vec<Entry> {
    [
        ("lock", "Lock", "Lock the screen", "screen", "system-lock-screen", &config.lock, false),
        ("suspend", "Suspend", "Sleep, keeping the session in memory", "sleep", "system-suspend", &config.suspend, false),
        ("hibernate", "Hibernate", "Save the session to disk and power off", "", "system-suspend-hibernate", &config.hibernate, false),
        ("logout", "Log Out", "End the session", "logoff exit quit", "system-log-out", &config.logout, true),
        ("reboot", "Reboot", "Restart the computer", "restart", "system-reboot", &config.reboot, true),
        ("shutdown", "Shut Down", "Power off the computer", "poweroff halt", "system-shutdown", &config.shutdown, true),
    ]
    .into_iter()
    .filter(|(.., command, _)| !command.trim().is_empty())
    .map(|(id, name, comment, keywords, icon, command, destructive)| Entry {
        id: id.to_string(),
        name: name.to_string(),
        comment: Some(comment.to_string()),
        icon: Some(EntryIcon::Name(icon.to_string())),
        keywords: format!("power {keywords}"),
        action: Action::Exec {
            command: command.clone(),
            terminal: false,
        },
        confirm: destructive && config.confirm,
    })
    .collect()
}
