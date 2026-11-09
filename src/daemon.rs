//! `launch daemon`: keeps the event loop, GPU and fonts loaded so the launcher opens faster.
//! `launch` sends its working directory and arguments over a Unix socket; the daemon loads
//! fresh entries and opens a new window for each request.

use std::{
    cell::RefCell,
    env,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{
        ffi::OsStringExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use clap::Parser;
use freya::{prelude::*, winit::window::WindowId};

use crate::{Args, ClipCmd, Cmd, config::Config, modules::ModuleKind, ui::Host};

pub enum Reply {
    Shown,
    Failed(String),
    NotRunning,
}

/// One daemon per display.
fn socket_path() -> PathBuf {
    let dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    match env::var("DISPLAY") {
        Ok(display) if !display.is_empty() => {
            dir.join(format!("launch-{}.sock", display.replace('/', "_")))
        }
        _ => dir.join("launch.sock"),
    }
}

/// Asks a running daemon to open the launcher with this process's arguments.
pub fn request() -> io::Result<Reply> {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return Ok(Reply::NotRunning);
    };
    let mut message = env::current_dir()?.into_os_string().into_vec();
    for arg in env::args_os().skip(1) {
        message.push(0);
        message.extend(arg.into_vec());
    }
    stream.write_all(&message)?;
    stream.shutdown(Shutdown::Write)?;
    // A hung daemon must not swallow the key press for good.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    match reply.strip_prefix("error: ") {
        Some(err) => Ok(Reply::Failed(err.trim_end().to_string())),
        None if reply == "ok" => Ok(Reply::Shown),
        None => Err(io::Error::other("the daemon closed the connection")),
    }
}

pub fn run(config: Option<PathBuf>) -> io::Result<()> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        return Err(io::Error::other(format!(
            "a daemon is already running on {}",
            path.display()
        )));
    }
    // Left behind by a daemon that did not shut down cleanly.
    let _ = fs::remove_file(&path);
    let listener = Arc::new(UnixListener::bind(&path)?);

    let mut launch_config = LaunchConfig::new().with_exit_on_close(false);
    // Fonts are set up once; a changed `font_family` needs a daemon restart.
    if let Some(font) = Config::load(config.clone()).font_family {
        launch_config = launch_config.with_default_font(font);
    }
    launch(launch_config.with_task(move |proxy| serve(listener, proxy, config)));
    Ok(())
}

/// The open launcher window and the request that opened it.
type Open = Rc<RefCell<Option<(WindowId, Vec<u8>)>>>;

async fn serve(listener: Arc<UnixListener>, proxy: LaunchProxy, config: Option<PathBuf>) {
    let open = Open::default();
    loop {
        let listener = listener.clone();
        let received = thread(move || -> io::Result<(UnixStream, Vec<u8>)> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(1)))?;
            let mut message = Vec::new();
            stream.read_to_end(&mut message)?;
            Ok((stream, message))
        })
        .await;
        let (mut stream, message) = match received {
            Ok(received) => received,
            Err(err) => {
                eprintln!("launch: daemon: {err}");
                continue;
            }
        };
        // A request always starts with the working directory; an empty one is `launch daemon`
        // checking whether this daemon is running (say, when the window manager restarts).
        if message.is_empty() {
            continue;
        }
        let reply = match show(&proxy, &open, message, config.clone()).await {
            Ok(()) => "ok".to_string(),
            Err(err) => format!("error: {err}"),
        };
        let _ = stream.write_all(reply.as_bytes());
    }
}

async fn show(
    proxy: &LaunchProxy,
    open: &Open,
    message: Vec<u8>,
    config: Option<PathBuf>,
) -> Result<(), String> {
    // The arguments without the working directory, to tell whether the same key was pressed.
    let request = message
        .iter()
        .position(|&byte| byte == 0)
        .map_or(Vec::new(), |end| message[end + 1..].to_vec());
    let mut parts = message.split(|&byte| byte == 0);
    let cwd = PathBuf::from(OsString::from_vec(
        parts.next().unwrap_or_default().to_vec(),
    ));
    let argv = std::iter::once(OsString::from("launch"))
        .chain(parts.map(|part| OsString::from_vec(part.to_vec())));
    let mut args = Args::try_parse_from(argv).map_err(|err| err.to_string())?;
    match &args.command {
        None => {}
        Some(Cmd::Cliphist {
            command: None | Some(ClipCmd::Pick),
        }) => args.mode = Some(ModuleKind::Clipboard),
        Some(_) => return Err("the daemon only opens the launcher".to_string()),
    }
    if args.dmenu {
        return Err("dmenu mode does not run in the daemon".to_string());
    }

    // The same key again closes the launcher; another one (say, the clipboard history while
    // the app launcher is open) replaces it.
    let previous = open.borrow_mut().take();
    if let Some((id, previous)) = previous {
        let still_open = proxy
            .post_callback(move |renderer| renderer.windows().contains_key(&id))
            .await
            .unwrap_or(false);
        if still_open {
            Platform::get().close_window(id);
            if previous == request {
                return Ok(());
            }
        }
    }

    let config = args.config.take().map(|path| cwd.join(path)).or(config);
    let loaded = thread(move || crate::load(&args, Config::load(config))).await;
    let window = crate::window_config(loaded, Box::new(DaemonWindow));
    let id = proxy
        .post_callback(move |renderer| renderer.launch_window(window))
        .await
        .map_err(|_| "the event loop has stopped".to_string())?;
    *open.borrow_mut() = Some((id, request));
    Ok(())
}

/// Closing the launcher only closes its window; the daemon keeps running.
struct DaemonWindow;

impl Host for DaemonWindow {
    fn print(&self, _line: &str) {}

    fn exit(&self, _code: i32) {
        Platform::get().close_window(Platform::window_id());
    }
}
