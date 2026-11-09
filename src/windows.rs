//! Open windows via EWMH on X11. Lists nothing on pure Wayland sessions.

use std::error::Error;

use freya::prelude::Bytes;
use resvg::tiny_skia::{ColorU8, Pixmap};
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ClientMessageEvent, ConnectionExt, EventMask, Window},
    rust_connection::RustConnection,
};

use crate::entries::{Action, Entry, EntryIcon};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        UTF8_STRING,
        _NET_CLIENT_LIST_STACKING,
        _NET_ACTIVE_WINDOW,
        _NET_CLOSE_WINDOW,
        _NET_CURRENT_DESKTOP,
        _NET_DESKTOP_NAMES,
        _NET_WM_NAME,
        _NET_WM_DESKTOP,
        _NET_WM_ICON,
        _NET_WM_STATE,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_WM_WINDOW_TYPE_DESKTOP,
    }
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Desktop value EWMH uses for windows shown on all desktops.
const ALL_DESKTOPS: u32 = 0xFFFF_FFFF;

/// Open windows, most recently raised first. `icon_size` is the preferred icon size in pixels.
pub fn window_entries(icon_size: u32) -> Vec<Entry> {
    list(icon_size).unwrap_or_else(|err| {
        eprintln!("launch: cannot list windows: {err}");
        Vec::new()
    })
}

fn list(icon_size: u32) -> Result<Vec<Entry>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn)?.reply()?;

    let desktop_names: Vec<String> = property(&conn, root, atoms._NET_DESKTOP_NAMES)?
        .split(|byte| *byte == 0)
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect();

    let mut entries = Vec::new();
    for window in property32(&conn, root, atoms._NET_CLIENT_LIST_STACKING)?
        .into_iter()
        .rev()
    {
        let types = property32(&conn, window, atoms._NET_WM_WINDOW_TYPE)?;
        let states = property32(&conn, window, atoms._NET_WM_STATE)?;
        if types.contains(&atoms._NET_WM_WINDOW_TYPE_DOCK)
            || types.contains(&atoms._NET_WM_WINDOW_TYPE_DESKTOP)
            || states.contains(&atoms._NET_WM_STATE_SKIP_TASKBAR)
        {
            continue;
        }

        let mut title =
            String::from_utf8_lossy(&property(&conn, window, atoms._NET_WM_NAME)?).into_owned();
        if title.is_empty() {
            title = String::from_utf8_lossy(&property(&conn, window, AtomEnum::WM_NAME.into())?)
                .into_owned();
        }
        // WM_CLASS is "instance\0class\0".
        let class_property = property(&conn, window, AtomEnum::WM_CLASS.into())?;
        let mut class_parts = class_property
            .split(|byte| *byte == 0)
            .map(|part| String::from_utf8_lossy(part).into_owned());
        let instance = class_parts.next().unwrap_or_default();
        let class = class_parts.next().unwrap_or_else(|| instance.clone());

        let desktop = property32(&conn, window, atoms._NET_WM_DESKTOP)?
            .first()
            .copied();
        let desktop_name = desktop.and_then(|desktop| {
            if desktop == ALL_DESKTOPS {
                Some("all desktops".to_string())
            } else {
                desktop_names
                    .get(desktop as usize)
                    .filter(|name| !name.is_empty())
                    .cloned()
                    .or_else(|| Some(format!("desktop {}", desktop + 1)))
            }
        });

        let icon = property32(&conn, window, atoms._NET_WM_ICON)
            .ok()
            .and_then(|data| icon_png(&data, icon_size))
            .map(EntryIcon::Image)
            .or_else(|| (!instance.is_empty()).then(|| EntryIcon::Name(instance.to_lowercase())));

        let comment = [Some(class.clone()), desktop_name]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ·  ");

        entries.push(Entry {
            id: window.to_string(),
            name: if title.is_empty() {
                class.clone()
            } else {
                title
            },
            comment: Some(comment),
            icon,
            keywords: format!("{class} {instance}"),
            action: Action::Focus { window, desktop },
            confirm: false,
        });
    }

    Ok(entries)
}

/// Switches to the window's desktop and asks the window manager to activate it.
pub fn focus(window: Window, desktop: Option<u32>) -> Result<()> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn)?.reply()?;

    if let Some(desktop) = desktop.filter(|desktop| *desktop != ALL_DESKTOPS) {
        send(
            &conn,
            root,
            root,
            atoms._NET_CURRENT_DESKTOP,
            [desktop, 0, 0, 0, 0],
        )?;
    }
    // Source indication 2 = pager, which window managers honor without focus-stealing checks.
    send(
        &conn,
        root,
        window,
        atoms._NET_ACTIVE_WINDOW,
        [2, 0, 0, 0, 0],
    )?;
    conn.flush()?;
    Ok(())
}

/// Asks the window manager to close the window gracefully.
pub fn close(window: Window) -> Result<()> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn)?.reply()?;
    send(&conn, root, window, atoms._NET_CLOSE_WINDOW, [0, 2, 0, 0, 0])?;
    conn.flush()?;
    Ok(())
}

/// Whether a compositing manager is running, which is needed for transparent windows.
pub fn has_compositor() -> bool {
    let check = || -> Result<bool> {
        let (conn, screen) = x11rb::connect(None)?;
        let atom = conn
            .intern_atom(false, format!("_NET_WM_CM_S{screen}").as_bytes())?
            .reply()?
            .atom;
        Ok(conn.get_selection_owner(atom)?.reply()?.owner != x11rb::NONE)
    };
    check().unwrap_or(false)
}

fn send(
    conn: &RustConnection,
    root: Window,
    window: Window,
    message_type: u32,
    data: [u32; 5],
) -> Result<()> {
    let event = ClientMessageEvent::new(32, window, message_type, data);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )?;
    Ok(())
}

fn property(conn: &RustConnection, window: Window, atom: u32) -> Result<Vec<u8>> {
    Ok(conn
        .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX / 4)?
        .reply()?
        .value)
}

fn property32(conn: &RustConnection, window: Window, atom: u32) -> Result<Vec<u32>> {
    let reply = conn
        .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX / 4)?
        .reply()?;
    Ok(reply.value32().map(Iterator::collect).unwrap_or_default())
}

/// Picks the smallest `_NET_WM_ICON` image at least `size` wide (or the largest one) and
/// encodes it as PNG. The property is a list of `width, height, ARGB pixels...` images.
fn icon_png(data: &[u32], size: u32) -> Option<Bytes> {
    let mut images = Vec::new();
    let mut rest = data;
    while let [width, height, tail @ ..] = rest {
        let len = (*width as usize).checked_mul(*height as usize)?;
        if len == 0 || tail.len() < len {
            break;
        }
        images.push((*width, *height, &tail[..len]));
        rest = &tail[len..];
    }

    let (width, height, pixels) = images
        .iter()
        .filter(|(width, ..)| *width >= size)
        .min_by_key(|(width, ..)| *width)
        .or_else(|| images.iter().max_by_key(|(width, ..)| *width))?;

    let mut pixmap = Pixmap::new(*width, *height)?;
    for (target, argb) in pixmap.pixels_mut().iter_mut().zip(pixels.iter()) {
        let [a, r, g, b] = argb.to_be_bytes();
        *target = ColorU8::from_rgba(r, g, b, a).premultiply();
    }
    pixmap.encode_png().ok().map(Bytes::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_best_icon_size() {
        // A 1x1 red and a 2x2 green image.
        let data = [
            1, 1, 0xFFFF0000, 2, 2, 0xFF00FF00, 0xFF00FF00, 0xFF00FF00, 0xFF00FF00,
        ];
        assert!(icon_png(&data, 2).is_some());
        assert!(icon_png(&data, 64).is_some());
        assert!(icon_png(&[4, 4, 1], 4).is_none());
    }
}
