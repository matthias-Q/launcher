//! The clipboard history on disk.
//!
//! Ordinary entries live in `$XDG_DATA_HOME/cliphist/history.json`. Secrets live in
//! `$XDG_RUNTIME_DIR/cliphist/secrets.json`, which is a per-user tmpfs, so they never
//! touch the disk and are gone after a reboot at the latest. The directories keep the
//! `cliphist` name from when this was a separate tool, so its history carries over.

use std::{
    env,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub text: String,
    /// Unix milliseconds of the last time this was copied.
    pub time: u64,
    /// Set for secrets: Unix milliseconds after which the entry is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<u64>,
}

impl Item {
    pub fn is_secret(&self) -> bool {
        self.expires.is_some()
    }

    fn is_expired(&self, now: u64) -> bool {
        self.expires.is_some_and(|expires| expires <= now)
    }
}

/// All entries, newest first.
#[derive(Default, Debug)]
pub struct Store {
    pub items: Vec<Item>,
}

impl Store {
    /// Reads history and secrets. Unreadable files count as empty. Expired secrets are
    /// included until [`Store::expire`] drops them.
    pub fn load() -> Self {
        let mut items: Vec<Item> = [history_path(), secrets_path()]
            .into_iter()
            .flatten()
            .flat_map(|path| read_items(&path))
            .collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.time));
        Self { items }
    }

    pub fn save(&self) -> io::Result<()> {
        let (secrets, history): (Vec<&Item>, Vec<&Item>) =
            self.items.iter().partition(|item| item.is_secret());
        if let Some(path) = history_path() {
            write_items(&path, &history)?;
        }
        match secrets_path() {
            Some(path) => write_items(&path, &secrets)?,
            // No safe place for secrets: they were never stored, nothing to save.
            None => debug_assert!(secrets.is_empty()),
        }
        Ok(())
    }

    /// Records `text` as the newest entry. Copying something already in the history moves it
    /// to the top; an entry that was a secret stays one.
    pub fn push(&mut self, text: String, secret: bool, secret_ttl: u64, max_entries: usize) {
        let now = now();
        let was_secret = match self.items.iter().position(|item| item.text == text) {
            Some(i) => self.items.remove(i).is_secret(),
            None => false,
        };
        let secret = secret || was_secret;
        // Without a runtime dir there is nowhere to keep secrets off the disk.
        if secret && secrets_path().is_none() {
            return;
        }
        self.items.insert(
            0,
            Item {
                text,
                time: now,
                expires: secret.then_some(now + secret_ttl * 1000),
            },
        );

        let mut kept = 0;
        self.items.retain(|item| {
            if item.is_secret() {
                return true;
            }
            kept += 1;
            kept <= max_entries
        });
    }

    pub fn remove(&mut self, text: &str) {
        self.items.retain(|item| item.text != text);
    }

    /// Drops expired secrets and returns them.
    pub fn expire(&mut self) -> Vec<Item> {
        let now = now();
        let (expired, kept) = self.items.drain(..).partition(|item| item.is_expired(now));
        self.items = kept;
        expired
    }

    /// When the next secret expires, in Unix milliseconds.
    pub fn next_expiry(&self) -> Option<u64> {
        self.items.iter().filter_map(|item| item.expires).min()
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn history_path() -> Option<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|dir| dir.join("cliphist").join("history.json"))
}

fn secrets_path() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .map(|dir| dir.join("cliphist").join("secrets.json"))
}

fn read_items(path: &Path) -> Vec<Item> {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

/// Writes atomically with owner-only permissions.
fn write_items(path: &Path, items: &[&Item]) -> io::Result<()> {
    let dir = path.parent().expect("store paths have a parent");
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&serde_json::to_vec(items)?)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(store: &Store) -> Vec<&str> {
        store.items.iter().map(|item| item.text.as_str()).collect()
    }

    #[test]
    fn push_dedups_and_limits() {
        let mut store = Store::default();
        for text in ["a", "b", "c", "a"] {
            store.push(text.to_string(), false, 30, 2);
        }
        assert_eq!(texts(&store), ["a", "c"]);
    }

    #[test]
    fn secrets_stay_secret_and_expire() {
        if secrets_path().is_none() {
            return;
        }
        let mut store = Store::default();
        store.push("pw".to_string(), true, 0, 10);
        store.push("x".to_string(), false, 0, 10);
        store.push("pw".to_string(), false, 0, 10);
        assert!(store.items[0].is_secret());
        assert_eq!(store.expire().len(), 1);
        assert_eq!(texts(&store), ["x"]);
    }
}
