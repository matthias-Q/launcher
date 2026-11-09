use std::{io, sync::mpsc::RecvTimeoutError, time::Duration};

use super::{
    ClipboardConfig,
    backend::Backend,
    secret,
    store::{self, Store},
};

/// Records every clipboard change until the display goes away.
pub fn run(config: &ClipboardConfig) -> io::Result<()> {
    let backend = Backend::detect();
    let changes = backend.watch()?;
    capture(backend, config);

    loop {
        let timeout = Store::load()
            .next_expiry()
            .map(|expires| Duration::from_millis(expires.saturating_sub(store::now())))
            .unwrap_or(Duration::from_secs(3600));
        match changes.recv_timeout(timeout) {
            Ok(()) => capture(backend, config),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("lost the clipboard watcher"));
            }
        }
        expire(backend, config);
    }
}

fn capture(backend: Backend, config: &ClipboardConfig) {
    let targets = backend.targets();
    let Some(text) = backend.read() else {
        return;
    };
    if text.trim().is_empty() || text.len() > config.max_bytes {
        return;
    }
    let is_secret = secret::has_password_hint(&targets)
        || (config.detect_secrets && secret::looks_secret(&text));

    // Reload every time so deletions from the picker are kept.
    let mut store = Store::load();
    store.push(text, is_secret, config.secret_ttl, config.max_entries);
    save(&store);
}

fn expire(backend: Backend, config: &ClipboardConfig) {
    let mut store = Store::load();
    let expired = store.expire();
    if expired.is_empty() {
        return;
    }
    save(&store);

    if config.clear_expired_secrets
        && let Some(current) = backend.read()
        && expired.iter().any(|item| item.text == current)
        && let Err(err) = backend.write("")
    {
        eprintln!("launch: failed to clear the clipboard: {err}");
    }
}

fn save(store: &Store) {
    if let Err(err) = store.save() {
        eprintln!("launch: failed to save history: {err}");
    }
}
