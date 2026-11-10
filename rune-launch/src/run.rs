use std::{collections::HashSet, env, fs, os::unix::fs::PermissionsExt, path::Path};

use crate::entries::{Action, Entry, EntryIcon};

/// Collects the executables on `$PATH`, like `rofi -show run`. Earlier directories shadow
/// later ones with the same command name, as in the shell.
pub fn run_entries() -> Vec<Entry> {
    let Some(path) = env::var_os("PATH") else {
        return Vec::new();
    };
    let mut seen_dirs = HashSet::new();
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for dir in env::split_paths(&path) {
        // Relative entries such as "." depend on the launcher's working directory.
        if !dir.is_absolute() {
            continue;
        }
        // `/bin` is often a symlink to `/usr/bin`; list it only once.
        if !seen_dirs.insert(fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone())) {
            continue;
        }
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = read
            .flatten()
            .filter_map(|file| file.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.') && is_executable(&dir.join(name)))
            .collect();
        names.sort();
        for name in names {
            if !seen.insert(name.clone()) {
                continue;
            }
            entries.push(Entry {
                id: name.clone(),
                comment: Some(dir.join(&name).display().to_string()),
                icon: Some(EntryIcon::Name("application-x-executable".to_string())),
                keywords: String::new(),
                action: Action::Exec {
                    command: shell_quote(&name),
                    terminal: false,
                },
                confirm: false,
                expires: None,
                name,
            });
        }
    }
    entries
}

/// A regular file (following symlinks) with any execute bit set.
fn is_executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn shell_quote(word: &str) -> String {
    if word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.+@%:,/".contains(c))
    {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(shell_quote("cargo-watch"), "cargo-watch");
        assert_eq!(shell_quote("my app"), "'my app'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }
}
