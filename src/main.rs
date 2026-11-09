mod backdrop;
mod calc;
mod config;
mod emoji;
mod entries;
mod icons;
mod modules;
mod power;
mod ui;
mod windows;

use std::{
    io::{IsTerminal, Read},
    path::PathBuf,
    rc::Rc,
};

use clap::Parser;
use config::Config;
use freya::{
    prelude::*,
    winit::{
        dpi::PhysicalPosition,
        platform::x11::{WindowAttributesExtX11, WindowType},
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::WindowLevel,
    },
};
use icons::IconResolver;
use modules::{Module, ModuleKind, Tab};
use ui::{DmenuOptions, Launcher, LauncherData, Process, WindowInfo};

/// Fuzzy searchable application launcher.
///
/// The config is read from `$XDG_CONFIG_HOME/launch/config.toml` unless `--config` is given.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Pick a line from stdin and print it, like dmenu.
    #[arg(short, long)]
    dmenu: bool,

    /// dmenu: text shown left of the input.
    #[arg(short, long, requires = "dmenu")]
    prompt: Option<String>,

    /// dmenu: print the index of the picked line (0-based) instead of the line.
    #[arg(long, requires = "dmenu")]
    index: bool,

    /// dmenu: mark lines with Shift+Enter, then print all of them with Enter.
    #[arg(long, requires = "dmenu")]
    multi: bool,

    /// dmenu: never print typed text that matches no line.
    #[arg(long, requires = "dmenu")]
    no_custom: bool,

    /// Module to open with. If it is not enabled in the config, only this module is shown.
    #[arg(short, long, value_enum)]
    mode: Option<ModuleKind>,

    /// Config file to use instead of the default location.
    #[arg(short, long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Print a commented example config and exit.
    #[arg(long)]
    default_config: bool,
}

fn main() {
    let args = Args::parse();
    if args.default_config {
        print!("{}", config::EXAMPLE);
        return;
    }
    let config = Config::load(args.config.clone());

    // Grabbed while everything else loads; it has to happen before our window is mapped.
    let capture = (config.blur > 0.)
        .then(|| std::thread::spawn(|| backdrop::pointer_monitor().and_then(backdrop::capture)));

    // Scale is unknown before the window exists; load images for HiDPI so they stay sharp.
    let icon_pixels = config.icon_size as u32 * 2;

    // The icon theme is indexed while the modules load.
    let (icons, (modules, initial)) = std::thread::scope(|scope| {
        let icons = scope.spawn(|| IconResolver::new(config.icon_theme.as_deref(), icon_pixels));
        let modules = load_modules(&args, &config, icon_pixels);
        (icons.join().expect("icon theme loading panicked"), modules)
    });
    let dmenu = args.dmenu.then_some(DmenuOptions {
        prompt: args.prompt,
        index: args.index,
        multi: args.multi,
        no_custom: args.no_custom,
    });

    let mut tabs: Vec<Tab> = (0..modules.len()).map(Tab::Module).collect();
    if config.all_tab && modules.len() > 1 {
        tabs.insert(0, Tab::All);
    }
    let initial = tabs.iter().position(|tab| *tab == initial).unwrap_or(0);
    let show_tabs = tabs.len() > 1;
    let width = config.width as f64;
    let height = config.window_height(show_tabs) as f64;
    let default_font = config.font_family.clone();
    let rounded = config.window_radius > 0. && windows::has_compositor();
    let blur = config.blur;
    let window = Rc::new(WindowInfo::default());

    let launcher = Launcher {
        data: Rc::new(LauncherData {
            modules,
            tabs,
            initial,
            icons,
            rounded,
            config,
            dmenu,
            host: Box::new(Process),
            window: window.clone(),
        }),
    };

    let mut launch_config = LaunchConfig::new();
    if let Some(font) = default_font {
        launch_config = launch_config.with_default_font(font);
    }

    launch(
        launch_config.with_window(
            WindowConfig::new_app(launcher)
                .with_title("launch")
                .with_app_id("launch")
                .with_size(width, height)
                .with_decorations(false)
                .with_resizable(false)
                .with_transparency(rounded)
                .with_background(Color::TRANSPARENT)
                .with_window_attributes({
                    let window = window.clone();
                    move |attributes, event_loop| {
                        // Dialogs float in tiling window managers (awesome, i3, ...).
                        let attributes = attributes
                            .with_window_level(WindowLevel::AlwaysOnTop)
                            .with_x11_window_type(vec![WindowType::Dialog]);
                        let captured = capture.and_then(|thread| {
                            thread
                                .join()
                                .unwrap_or_else(|_| Err("capture panicked".into()))
                                .map_err(|err| {
                                    eprintln!("launch: cannot capture the desktop to blur: {err}")
                                })
                                .ok()
                        });
                        // Center on the captured monitor, the one under the pointer, which is
                        // where window managers tend to open windows. Wayland compositors
                        // ignore this and place the window themselves.
                        let Some(monitor) = event_loop
                            .available_monitors()
                            .find(|monitor| {
                                captured.as_ref().is_some_and(|captured| {
                                    let origin = monitor.position();
                                    (origin.x, origin.y) == (captured.monitor.x, captured.monitor.y)
                                })
                            })
                            .or_else(|| event_loop.primary_monitor())
                            .or_else(|| event_loop.available_monitors().next())
                        else {
                            return attributes;
                        };
                        let scale = monitor.scale_factor();
                        let origin = monitor.position();
                        let size = monitor.size();
                        let x = origin.x + ((size.width as f64 - width * scale) / 2.) as i32;
                        let y = origin.y + ((size.height as f64 - height * scale) / 2.) as i32;
                        window.position.set(Some((x, y)));
                        if let Some(mut captured) = captured {
                            captured.blur(blur * scale as f32);
                            let _ = window.backdrop.set(captured);
                        }
                        attributes.with_position(PhysicalPosition::new(x, y))
                    }
                })
                .with_window_handle(move |handle| {
                    let id = match handle.window_handle().map(|handle| handle.as_raw()) {
                        Ok(RawWindowHandle::Xlib(handle)) => Some(handle.window as u32),
                        Ok(RawWindowHandle::Xcb(handle)) => Some(handle.window.get()),
                        _ => None,
                    };
                    window.id.set(id);
                }),
        ),
    );
}

/// The modules to show and the tab to open with.
fn load_modules(args: &Args, config: &Config, icon_pixels: u32) -> (Vec<Module>, Tab) {
    if args.dmenu {
        let mut input = String::new();
        if !std::io::stdin().is_terminal() {
            let _ = std::io::stdin().read_to_string(&mut input);
        }
        return (vec![Module::dmenu(&input)], Tab::Module(0));
    }
    let mut kinds = config.modules.clone();
    kinds.dedup();
    if kinds.is_empty() {
        kinds.push(ModuleKind::Apps);
    }
    let initial = match args.mode {
        Some(mode) => kinds
            .iter()
            .position(|kind| *kind == mode)
            .map(Tab::Module)
            .unwrap_or_else(|| {
                kinds = vec![mode];
                Tab::Module(0)
            }),
        None => Tab::All,
    };
    // Load before our own window exists so it does not show up in the window list.
    let modules = std::thread::scope(|scope| {
        let loading: Vec<_> = kinds
            .into_iter()
            .map(|kind| scope.spawn(move || Module::load(kind, icon_pixels, config)))
            .collect();
        loading
            .into_iter()
            .map(|module| module.join().expect("module loading panicked"))
            .collect()
    });
    (modules, initial)
}
