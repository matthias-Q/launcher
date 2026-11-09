//! Reading, writing and watching the clipboard through `xclip` (X11) or `wl-clipboard`
//! (Wayland). Changes are detected with XFixes on X11 and `wl-paste --watch` on Wayland.

use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xfixes::{ConnectionExt as _, SelectionEventMask},
        xproto::ConnectionExt as _,
    },
};

#[derive(Clone, Copy, Debug)]
pub enum Backend {
    X11,
    Wayland,
}

impl Backend {
    pub fn detect() -> Self {
        if env::var_os("WAYLAND_DISPLAY").is_some() {
            Self::Wayland
        } else {
            Self::X11
        }
    }

    /// The clipboard's text, if it holds any.
    pub fn read(self) -> Option<String> {
        let output = match self {
            Self::X11 => run(&[
                "xclip",
                "-o",
                "-selection",
                "clipboard",
                "-t",
                "UTF8_STRING",
            ]),
            Self::Wayland => run(&["wl-paste", "--no-newline", "--type", "text"]),
        }?;
        String::from_utf8(output).ok()
    }

    /// The formats the current clipboard owner offers.
    pub fn targets(self) -> Vec<String> {
        let output = match self {
            Self::X11 => run(&["xclip", "-o", "-selection", "clipboard", "-t", "TARGETS"]),
            Self::Wayland => run(&["wl-paste", "--list-types"]),
        };
        output
            .map(|output| {
                String::from_utf8_lossy(&output)
                    .lines()
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn write(self, text: &str) -> io::Result<()> {
        let (program, args): (&str, &[&str]) = match self {
            Self::X11 => ("xclip", &["-selection", "clipboard"]),
            Self::Wayland => ("wl-copy", &[]),
        };
        let mut child = Command::new(program)
            .args(args)
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

    /// Sends a message whenever the clipboard owner changes. The channel closes when the
    /// connection to the display is lost.
    pub fn watch(self) -> io::Result<Receiver<()>> {
        let (tx, rx) = mpsc::channel();
        match self {
            Self::X11 => {
                let (conn, screen) = x11rb::connect(None).map_err(io::Error::other)?;
                let root = conn.setup().roots[screen].root;
                let setup = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                    conn.xfixes_query_version(5, 0)?.reply()?;
                    let clipboard = conn.intern_atom(false, b"CLIPBOARD")?.reply()?.atom;
                    conn.xfixes_select_selection_input(
                        root,
                        clipboard,
                        SelectionEventMask::SET_SELECTION_OWNER,
                    )?;
                    conn.flush()?;
                    Ok(())
                };
                setup().map_err(io::Error::other)?;
                thread::spawn(move || {
                    while let Ok(event) = conn.wait_for_event() {
                        if matches!(event, Event::XfixesSelectionNotify(_)) && tx.send(()).is_err()
                        {
                            break;
                        }
                    }
                });
            }
            Self::Wayland => {
                // The watch command runs on every change; it prints one line per change.
                let mut child = Command::new("wl-paste")
                    .args(["--watch", "sh", "-c", "cat >/dev/null; echo"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .spawn()?;
                let stdout = child.stdout.take().expect("stdout is piped");
                thread::spawn(move || {
                    for _ in BufReader::new(stdout).lines() {
                        if tx.send(()).is_err() {
                            break;
                        }
                    }
                    let _ = child.kill();
                });
            }
        }
        Ok(rx)
    }
}

/// Runs a command and returns its stdout if it succeeded.
fn run(command: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new(command[0])
        .args(&command[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}
