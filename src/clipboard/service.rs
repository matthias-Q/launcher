//! Running the daemon as a systemd user service.

use std::{env, fs, io, path::PathBuf, process::Command};

const UNIT: &str = "cliphist.service";

fn unit_path() -> io::Result<PathBuf> {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|dir| dir.join("systemd/user").join(UNIT))
        .ok_or_else(|| io::Error::other("neither XDG_CONFIG_HOME nor HOME is set"))
}

fn unit(exe: &str) -> String {
    format!(
        "[Unit]
Description=Clipboard history daemon (launch cliphist)
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=\"{exe}\" cliphist daemon
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
"
    )
}

/// Writes the unit for the running binary, then enables and (re)starts it.
pub fn install() -> io::Result<()> {
    let exe = env::current_exe()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("binary path is not UTF-8"))?;
    let path = unit_path()?;
    fs::create_dir_all(path.parent().expect("unit path has a parent"))?;
    fs::write(&path, unit(exe))?;
    println!("wrote {}", path.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    // Restart so a reinstall picks up the new binary.
    systemctl(&["restart", UNIT])?;
    println!("{UNIT} enabled and running");
    Ok(())
}

/// Stops and disables the service and removes the unit.
pub fn uninstall() -> io::Result<()> {
    let path = unit_path()?;
    // Ignore failures: the unit may already be stopped or unknown.
    let _ = systemctl(&["disable", "--now", UNIT]);
    match fs::remove_file(&path) {
        Ok(()) => println!("removed {}", path.display()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    systemctl(&["daemon-reload"])
}

fn systemctl(args: &[&str]) -> io::Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "systemctl --user {} failed",
            args.join(" ")
        )))
    }
}
