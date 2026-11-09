use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use freya::prelude::Bytes;

/// Something that can be picked in the launcher.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Stable identifier, used for usage history (desktop file id or the raw dmenu line).
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    pub icon: Option<EntryIcon>,
    /// Extra text that is searched but ranked below the name.
    pub keywords: String,
    pub action: Action,
    /// Needs a second activation before it runs (reboot, shut down, ...).
    pub confirm: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EntryIcon {
    /// Freedesktop icon name or absolute path, resolved through the icon theme.
    Name(String),
    /// Encoded image data (e.g. a window's `_NET_WM_ICON`).
    Image(Bytes),
    /// Text drawn in place of an icon (emoji).
    Glyph(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Run a shell command (desktop entry `Exec`).
    Exec { command: String, terminal: bool },
    /// Print the line, or its index, to stdout (dmenu mode).
    Print(usize),
    /// Activate an X11 window, switching to its desktop first.
    Focus { window: u32, desktop: Option<u32> },
    /// Copy and/or type an emoji.
    Emoji(String),
    /// Put text on the clipboard (calculator results).
    Copy(String),
}

/// Collects `.desktop` applications from the XDG data directories.
pub fn desktop_entries() -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    // User dirs come first so they shadow system entries with the same id.
    for dir in application_dirs() {
        let mut files = Vec::new();
        collect_desktop_files(&dir, &mut files);
        for path in files {
            let Some(id) = desktop_id(&dir, &path) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(entry) = parse_desktop_file(&path, id) {
                entries.push(entry);
            }
        }
    }

    entries
}

/// Turns stdin lines into entries (dmenu mode).
pub fn dmenu_entries(input: &str) -> Vec<Entry> {
    input
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| Entry {
            id: line.to_string(),
            name: line.to_string(),
            comment: None,
            icon: None,
            keywords: String::new(),
            action: Action::Print(index),
            confirm: false,
        })
        .collect()
}

fn application_dirs() -> Vec<PathBuf> {
    data_dirs()
        .into_iter()
        .map(|dir| dir.join("applications"))
        .collect()
}

/// XDG data dirs in priority order: `$XDG_DATA_HOME`, `$XDG_DATA_DIRS`, then well-known
/// package manager locations that are often missing from the environment (a launcher started
/// by the window manager may not see the shell profile that adds them).
pub fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let state_home = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".local/state")));
    let data_dirs = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());

    let mut extra = Vec::new();
    if let Some(home) = &home {
        extra.push(home.join(".nix-profile/share"));
    }
    if let Some(state_home) = &state_home {
        extra.push(state_home.join("nix/profile/share"));
    }
    if let Ok(user) = env::var("USER") {
        extra.push(PathBuf::from(format!(
            "/etc/profiles/per-user/{user}/share"
        )));
    }
    extra.push(PathBuf::from("/run/current-system/sw/share"));
    extra.push(PathBuf::from("/nix/var/nix/profiles/default/share"));
    if let Some(data_home) = &data_home {
        extra.push(data_home.join("flatpak/exports/share"));
    }
    extra.push(PathBuf::from("/var/lib/flatpak/exports/share"));

    let mut seen = HashSet::new();
    data_home
        .into_iter()
        .chain(data_dirs.split(':').map(PathBuf::from))
        .chain(extra)
        .filter(|dir| seen.insert(dir.clone()) && dir.is_dir())
        .collect()
}

fn collect_desktop_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_desktop_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "desktop") {
            out.push(path);
        }
    }
}

/// Desktop file id per the spec: path relative to the applications dir, `/` replaced by `-`.
fn desktop_id(base: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(base).ok()?;
    Some(relative.to_string_lossy().replace('/', "-"))
}

fn parse_desktop_file(path: &Path, id: String) -> Option<Entry> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_main_group = false;
    let mut fields = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_main_group || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            // Localized keys (Name[de]) are skipped, the unlocalized one is used.
            fields.insert(key.trim(), value.trim());
        }
    }

    let is_true = |key: &str| fields.get(key).is_some_and(|value| *value == "true");
    if fields.get("Type") != Some(&"Application") || is_true("NoDisplay") || is_true("Hidden") {
        return None;
    }

    let name = fields.get("Name")?.to_string();
    let command = strip_field_codes(fields.get("Exec")?);
    let comment = fields
        .get("Comment")
        .or_else(|| fields.get("GenericName"))
        .map(|comment| comment.to_string());
    let keywords = [
        fields.get("GenericName").copied().unwrap_or_default(),
        &fields
            .get("Keywords")
            .copied()
            .unwrap_or_default()
            .replace(';', " "),
        command.split_whitespace().next().unwrap_or_default(),
    ]
    .join(" ");

    Some(Entry {
        id,
        name,
        comment,
        icon: fields
            .get("Icon")
            .map(|icon| EntryIcon::Name(icon.to_string())),
        keywords,
        action: Action::Exec {
            command,
            terminal: is_true("Terminal"),
        },
        confirm: false,
    })
}

/// Removes `%f`, `%U`, ... placeholders from an `Exec` value since we launch without arguments.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        if let Some('%') = chars.next() {
            out.push('%');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Starts the command detached from the launcher so it outlives it.
pub fn spawn(command: &str, terminal: bool, terminal_command: Option<&str>) -> std::io::Result<()> {
    let command = if terminal {
        let term = terminal_command
            .map(str::to_string)
            .or_else(|| env::var("TERMINAL").ok())
            .unwrap_or_else(|| "x-terminal-emulator".to_string());
        format!("{term} -e {command}")
    } else {
        command.to_string()
    };

    Command::new("sh")
        .arg("-c")
        .arg(format!("exec {command}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

/// Pipes `text` into `command`, or `wl-copy` / `xclip` when none is configured.
pub fn copy(text: &str, command: Option<&str>) -> std::io::Result<()> {
    let command = command.unwrap_or(if env::var_os("WAYLAND_DISPLAY").is_some() {
        "wl-copy"
    } else {
        "xclip -selection clipboard"
    });
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(text.as_bytes())?;
    // xclip and wl-copy fork to keep serving the selection, so this returns quickly.
    child.wait()?;
    Ok(())
}

/// Launch count and last launch time per entry id, used to rank recent and frequent entries
/// first. Stored as `count<TAB>unix_seconds<TAB>id` lines.
pub struct History {
    path: Option<PathBuf>,
    uses: HashMap<String, Use>,
}

#[derive(Clone, Copy, Default)]
pub struct Use {
    pub count: u32,
    /// Unix seconds of the last launch, 0 if unknown.
    pub last: u64,
}

impl History {
    pub fn load(namespace: &str) -> Self {
        let path = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .map(|cache| cache.join("launch").join(namespace));

        let uses = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|content| content.lines().filter_map(parse_history_line).collect())
            .unwrap_or_default();

        Self { path, uses }
    }

    pub fn get(&self, id: &str) -> Use {
        self.uses.get(id).copied().unwrap_or_default()
    }

    pub fn record(&mut self, id: &str) {
        let entry = self.uses.entry(id.to_string()).or_default();
        entry.count += 1;
        entry.last = now();
        self.save();
    }

    pub fn forget(&mut self, id: &str) {
        if self.uses.remove(id).is_some() {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let content: String = self
            .uses
            .iter()
            .map(|(id, entry)| format!("{}\t{}\t{id}\n", entry.count, entry.last))
            .collect();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, content);
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn parse_history_line(line: &str) -> Option<(String, Use)> {
    let (count, rest) = line.split_once('\t')?;
    let count = count.parse().ok()?;
    // Lines written before timestamps were tracked are `count<TAB>id`.
    let (last, id) = match rest.split_once('\t') {
        Some((last, id)) if last.parse::<u64>().is_ok() => (last.parse().ok()?, id),
        _ => (0, rest),
    };
    Some((id.to_string(), Use { count, last }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("firefox %u"), "firefox");
        assert_eq!(strip_field_codes("app --flag %F --x"), "app --flag --x");
        assert_eq!(strip_field_codes("echo 100%%"), "echo 100%");
    }

    #[test]
    fn parses_history_lines() {
        let (id, entry) = parse_history_line("3\t1700000000\tfirefox.desktop").unwrap();
        assert_eq!(
            (id.as_str(), entry.count, entry.last),
            ("firefox.desktop", 3, 1700000000)
        );
        let (id, entry) = parse_history_line("2\tfirefox.desktop").unwrap();
        assert_eq!(
            (id.as_str(), entry.count, entry.last),
            ("firefox.desktop", 2, 0)
        );
    }
}
