mod backdrop;
mod calc;
mod clipboard;
mod config;
mod emoji;
mod entries;
mod icons;
mod modules;
mod power;
mod run;
mod ui;
mod windows;

use std::io::{IsTerminal, Read};

use clap::Parser;
use config::Config;
use icons::IconResolver;
use modules::{Module, ModuleKind, Tab};
use rune::{App, AppSpec, WindowOptions, WindowSpec};
use ui::{DmenuOptions, State};

/// Fuzzy searchable application launcher (rune port of `launch`).
///
/// The config is read from `$XDG_CONFIG_HOME/launch/config.toml` unless `--config` is given.
#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Args {
    /// Print a commented example config and exit.
    #[arg(long)]
    pub default_config: bool,

    /// Use the config file at this path instead of the default one.
    #[arg(short, long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// Open on a specific module instead of the "All" tab.
    #[arg(short, long, value_enum)]
    pub mode: Option<ModuleKind>,

    /// Read lines from stdin and print the line picked with Enter (dmenu).
    #[arg(short, long)]
    pub dmenu: bool,

    /// Text shown left of the input (dmenu).
    #[arg(short, long, requires = "dmenu")]
    pub prompt: Option<String>,

    /// Print the index of the picked line instead of the line (dmenu).
    #[arg(long, requires = "dmenu")]
    pub index: bool,

    /// Select several lines with Shift+Enter; Enter prints them all (dmenu).
    #[arg(long, requires = "dmenu")]
    pub multi: bool,

    /// Never print typed text that has no match (dmenu).
    #[arg(long, requires = "dmenu")]
    pub no_custom: bool,
}

fn main() {
    // Rune's own tooling flags (`--snap`, `--tree`, `--script`, ...) share argv with ours and
    // are consumed by rune, not clap: strip them so `--config x --snap out.png` still parses.
    let prog = std::env::args().next().unwrap_or_else(|| "rune-launch".to_string());
    let args = Args::parse_from(std::iter::once(prog).chain(rune_tooling_stripped_args()));

    if args.default_config {
        print!("{}", config::EXAMPLE);
        return;
    }

    let config = Config::load(args.config.clone());
    let icon_pixels = (config.icon_size * 2.).round().max(1.) as u32;
    // Capture the desktop behind the window for the `blur` glass, on a thread so it overlaps
    // the module scan below (which joins its threads). Headless runs never touch X11, so
    // `--snap`/`--scene` stay deterministic. It must finish before the window maps, or it
    // would capture the launcher itself.
    let capture = (!rune::tooling::headless_run().is_some() && config.blur > 0.)
        .then(|| std::thread::spawn(capture_desktop));
    let (modules, initial) = load_modules(&args, &config);
    let backdrop = capture
        .and_then(|handle| handle.join().ok().flatten())
        .map(|backdrop| {
            let pixels = rune::Pixels::new();
            pixels.set(backdrop.width, backdrop.height, backdrop.rgba);
            pixels
        });
    let tabs = build_tabs(&modules, &config, initial);
    let show_tabs = tabs.len() > 1;
    let initial_pos = tabs
        .iter()
        .position(|tab| *tab == initial)
        .unwrap_or(0);

    let rounded = windows::has_compositor();
    let state = State::new(
        modules,
        tabs,
        initial_pos,
        config.clone(),
        IconResolver::new(config.icon_theme.as_deref(), icon_pixels),
        dmenu_options(&args),
        rounded,
        backdrop,
    );

    let size = (config.width, config.window_height(show_tabs));

    // Scenes set the initial state for headless snapshots (`--list-scenes`, `--scene`).
    let scenes: &[(&str, fn(&mut State))] = &[
        ("apps", |s| s.set_query("")),
        ("fire", |s| s.set_query("fire")),
        ("calc", |s| s.set_query("2^10 / 3")),
        ("run", |s| open_tab(s, ModuleKind::Run)),
        ("power", |s| open_tab(s, ModuleKind::Power)),
        // Deterministic glass: a synthetic desktop the panel blurs, no X11 capture.
        ("glass", |s| {
            let (width, height, rgba) = backdrop::synthetic();
            let pixels = rune::Pixels::new();
            pixels.set(width, height, rgba);
            s.backdrop = Some(pixels);
        }),
    ];

    let mut spec = AppSpec::new("Launch", size, || unreachable!(), ui::view);
    spec.scenes = scenes;
    let app = App::new(spec)
        .init(move || state)
        .on_focus(|s: &mut State, focused, _tx| {
            let had = s.focused;
            s.focused = focused;
            if !focused && had && s.config.close_on_unfocus {
                s.host.exit(1);
            }
        });

    rune::run_windows(move |displays| {
        let display = displays.iter().find(|d| d.primary).or(displays.first());
        let (mut pos_x, mut pos_y) = (0, 0);
        if let Some(d) = display {
            let ws = (size.0 as f64 * d.scale) as i32;
            let hs = (size.1 as f64 * d.scale) as i32;
            pos_x = d.position.0 + (d.size.0 as i32 - ws) / 2;
            pos_y = d.position.1 + (d.size.1 as i32 - hs) / 2;
        }
        let transparent = rounded;
        let options = WindowOptions {
            transparent,
            borderless: transparent,
            always_on_top: true,
            position: Some((pos_x, pos_y)),
            min_size: Some((0.0, 0.0)),
            resizable: false,
            ..Default::default()
        };
        let window: rune::AnyWindowSpec =
            WindowSpec::from_app("launch", app).options(options).into();
        vec![window]
    });
}

fn open_tab(s: &mut State, kind: ModuleKind) {
    s.active = s
        .tabs
        .iter()
        .position(|tab| {
            matches!(tab, Tab::Module(m) if s.modules[*m].title == kind_title(kind))
        })
        .unwrap_or(0);
    s.on_query_changed();
}

fn kind_title(kind: ModuleKind) -> &'static str {
    match kind {
        ModuleKind::Calc => "Calc",
        ModuleKind::Apps => "Apps",
        ModuleKind::Run => "Run",
        ModuleKind::Windows => "Windows",
        ModuleKind::Emoji => "Emoji",
        ModuleKind::Power => "Power",
        ModuleKind::Clipboard => "Clipboard",
    }
}

/// Captures the desktop for the glass effect, reporting (not failing on) capture errors.
fn capture_desktop() -> Option<backdrop::Backdrop> {
    match backdrop::pointer_monitor().and_then(backdrop::capture) {
        Ok(backdrop) => Some(backdrop),
        Err(err) => {
            eprintln!("rune-launch: cannot capture the desktop to blur: {err}");
            None
        }
    }
}

fn dmenu_options(args: &Args) -> Option<DmenuOptions> {
    if !args.dmenu {
        return None;
    }
    Some(DmenuOptions {
        prompt: args.prompt.clone(),
        index: args.index,
        multi: args.multi,
        no_custom: args.no_custom,
    })
}

/// The modules to show and the tab to open with, exactly as the Freya launch decides it.
fn load_modules(args: &Args, config: &Config) -> (Vec<Module>, Tab) {
    if args.dmenu {
        let mut input = String::new();
        if !std::io::stdin().is_terminal() {
            let _ = std::io::stdin().read_to_string(&mut input);
        }
        return (vec![Module::dmenu(&input)], Tab::Module(0));
    }

    let icon_size = (config.icon_size * 2.).round().max(1.) as u32;
    let mut kinds = config.modules.clone();
    kinds.dedup();
    if kinds.is_empty() {
        kinds.push(ModuleKind::Apps);
    }

    let mut initial = Tab::All;
    if let Some(mode) = args.mode {
        initial = match kinds.iter().position(|kind| *kind == mode) {
            Some(position) => Tab::Module(position),
            // A module not in the config replaces the list.
            None => {
                kinds = vec![mode];
                Tab::Module(0)
            }
        };
    }

    // Each kind loads on its own thread; join in configured order. The window opens after.
    let handles: Vec<_> = kinds
        .iter()
        .map(|&kind| {
            let config = config.clone();
            std::thread::spawn(move || Module::load(kind, icon_size, &config))
        })
        .collect();
    let modules: Vec<Module> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap_or_else(|_| panic!("module thread panicked")))
        .collect();
    (modules, initial)
}

fn build_tabs(modules: &[Module], config: &Config, initial: Tab) -> Vec<Tab> {
    let mut tabs: Vec<Tab> = (0..modules.len()).map(Tab::Module).collect();
    if config.all_tab && modules.len() > 1 {
        tabs.insert(0, Tab::All);
    }
    // `Tab::All` falls back to the first module tab when the All tab is disabled.
    if initial == Tab::All && config.all_tab && modules.len() > 1 {
        return tabs;
    }
    tabs
}

/// Removes rune's tooling arguments (and their values) from argv, leaving ours intact.
fn rune_tooling_stripped_args() -> Vec<String> {
    const NO_VALUE: &[&str] = &[
        "--whole-window", "--pointer", "--native", "--realtime", "--trace-all",
        "--list-scenes", "--first-pixels", "--list-windows", "--chrome", "--hot",
    ];
    const VALUE: &[&str] = &[
        "--snap", "--scene", "--variant", "--at", "--size", "--scale", "--theme",
        "--scroll-bars", "--script", "--frames", "--start", "--fps", "--out", "--video",
        "--duration", "--keys-theme", "--desktop", "--tree", "--html", "--html-template",
        "--html-widths", "--trace", "--strip", "--crop", "--cols", "--every", "--window",
        "--snap-on-patch", "--hot-control",
    ];
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if NO_VALUE.contains(&flag) {
            i += 1;
        } else if VALUE.contains(&flag) {
            i += 2;
        } else if flag == "--keys" {
            // Optional corner value: consume the next arg only when it parses as one.
            if args.get(i + 1).is_some_and(|v| v != "--" && !v.starts_with('-')) {
                i += 2;
            } else {
                i += 1;
            }
        } else if flag == "--playground" {
            // Optional story name: consume a non-flag next arg.
            if args
                .get(i + 1)
                .is_some_and(|v| v != "--" && !v.starts_with('-'))
            {
                i += 2;
            } else {
                i += 1;
            }
        } else {
            out.push(args[i].clone());
            i += 1;
        }
    }
    out
}
