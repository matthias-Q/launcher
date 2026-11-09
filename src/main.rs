mod config;
mod emoji;
mod entries;
mod icons;
mod modules;
mod windows;

use std::{
    cell::Cell,
    io::{IsTerminal, Read},
    path::PathBuf,
    rc::Rc,
};

use clap::Parser;
use config::{Config, TAB_HEIGHT};
use entries::{Action, Entry, EntryIcon};
use freya::{
    animation::*,
    prelude::*,
    winit::{
        dpi::PhysicalPosition,
        platform::x11::{WindowAttributesExtX11, WindowType},
        window::WindowLevel,
    },
};
use icons::{Icon, IconResolver};
use modules::{Hit, Module, ModuleKind, Tab};
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// Fuzzy searchable application launcher.
///
/// The config is read from `$XDG_CONFIG_HOME/launch/config.toml` unless `--config` is given.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Pick a line from stdin and print it, like dmenu.
    #[arg(short, long)]
    dmenu: bool,

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

const TAB_WIDTH: f32 = 96.;

fn main() {
    let args = Args::parse();
    if args.default_config {
        print!("{}", config::EXAMPLE);
        return;
    }
    let config = Config::load(args.config);

    // Scale is unknown before the window exists; load images for HiDPI so they stay sharp.
    let icon_pixels = config.icon_size as u32 * 2;

    let (modules, initial) = if args.dmenu {
        let mut input = String::new();
        if !std::io::stdin().is_terminal() {
            let _ = std::io::stdin().read_to_string(&mut input);
        }
        (vec![Module::dmenu(&input)], Tab::Module(0))
    } else {
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
        let modules = kinds
            .into_iter()
            .map(|kind| Module::load(kind, icon_pixels))
            .collect();
        (modules, initial)
    };

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

    let icons = IconResolver::new(config.icon_theme.as_deref(), icon_pixels);

    let launcher = Launcher {
        data: Rc::new(LauncherData {
            modules,
            tabs,
            initial,
            icons,
            rounded,
            config,
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
                .with_window_attributes(move |attributes, event_loop| {
                    // Dialogs float in tiling window managers (awesome, i3, ...).
                    let attributes = attributes
                        .with_window_level(WindowLevel::AlwaysOnTop)
                        .with_x11_window_type(vec![WindowType::Dialog]);
                    // Center on the primary monitor. Wayland compositors ignore this and
                    // place the window themselves.
                    let Some(monitor) = event_loop
                        .primary_monitor()
                        .or_else(|| event_loop.available_monitors().next())
                    else {
                        return attributes;
                    };
                    let scale = monitor.scale_factor();
                    let origin = monitor.position();
                    let size = monitor.size();
                    let x = origin.x + ((size.width as f64 - width * scale) / 2.) as i32;
                    let y = origin.y + ((size.height as f64 - height * scale) / 2.) as i32;
                    attributes.with_position(PhysicalPosition::new(x, y))
                }),
        ),
    );
}

struct LauncherData {
    modules: Vec<Module>,
    tabs: Vec<Tab>,
    /// Index into `tabs` shown on start.
    initial: usize,
    icons: IconResolver,
    /// Transparent window with rounded corners (a compositor is running).
    rounded: bool,
    config: Config,
}

impl LauncherData {
    fn activate(&self, hit: Option<Hit>, query: &str) -> ! {
        let Some((module, entry)) =
            hit.map(|(m, i)| (&self.modules[m], &self.modules[m].entries[i]))
        else {
            // dmenu convention: with nothing matched, the typed text is the selection.
            if self.modules[0].title == "dmenu" && !query.is_empty() {
                println!("{query}");
                std::process::exit(0);
            }
            std::process::exit(1);
        };

        module.record(entry);

        let result = match &entry.action {
            Action::Print => {
                println!("{}", entry.name);
                Ok(())
            }
            Action::Exec { command, terminal } => {
                entries::spawn(command, *terminal, self.config.terminal.as_deref())
                    .map_err(Into::into)
            }
            Action::Focus { window, desktop } => windows::focus(*window, *desktop),
            Action::Emoji(emoji) => emoji::deliver(
                emoji,
                self.config.emoji.action,
                self.config.emoji.copy_command.as_deref(),
            )
            .map_err(Into::into),
        };
        if let Err(err) = result {
            eprintln!("launch: {:?} failed: {err}", entry.name);
            std::process::exit(1);
        }
        std::process::exit(0);
    }
}

/// Duration for UI animations, or instant when they are disabled.
fn anim_time(config: &Config, millis: u64) -> u64 {
    if config.animations { millis } else { 1 }
}

struct Launcher {
    data: Rc<LauncherData>,
}

impl App for Launcher {
    fn render(&self) -> impl IntoElement {
        let data = self.data.clone();
        let config = &data.config;
        let colors = &config.colors;
        let visible_rows = config.rows.max(1);
        let row_height = config.row_height;
        let close_on_unfocus = config.close_on_unfocus;
        let tab_count = data.tabs.len();

        let query = use_state(String::new);
        let mut active = use_state(|| data.initial);
        let mut selected = use_state(|| 0usize);
        let mut offset = use_state(|| 0usize);

        // Rank every module once per query; tabs pick from (or merge) these.
        let ranked = use_memo({
            let data = data.clone();
            move || {
                let query = query.read();
                data.modules
                    .iter()
                    .map(|module| module.rank(&query))
                    .collect::<Vec<_>>()
            }
        });
        let tab_results = use_memo({
            let data = data.clone();
            move || {
                let ranked = ranked.read();
                let has_query = !query.read().trim().is_empty();
                data.tabs
                    .iter()
                    .map(|tab| match tab {
                        Tab::All => modules::merge(&data.modules, &ranked, has_query),
                        Tab::Module(m) => ranked[*m].iter().map(|&(_, i)| (*m, i)).collect(),
                    })
                    .collect::<Vec<Vec<Hit>>>()
            }
        });
        let results = use_memo(move || tab_results.read()[*active.read()].clone());

        // A new query or module starts back at the top.
        use_side_effect(move || {
            let _ = results.read();
            selected.set(0);
            offset.set(0);
        });

        // Behave like a popup: disappear once focus moves elsewhere.
        let was_focused = use_hook(|| Rc::new(Cell::new(false)));
        use_side_effect(move || {
            if !close_on_unfocus {
                return;
            }
            if *Platform::get().current_window().is_app_focused.read() {
                was_focused.set(true);
            } else if was_focused.get() {
                std::process::exit(1);
            }
        });

        // Fade and slide the whole panel in on open.
        let entrance = use_animation({
            let time = anim_time(config, 260);
            move |conf| {
                conf.on_creation(OnCreation::Run);
                AnimNum::new(0., 1.)
                    .time(time)
                    .function(Function::Expo)
                    .ease(Ease::Out)
            }
        });

        // The selection highlight slides between rows instead of jumping.
        let highlight_slot = use_memo(move || {
            let slot = (*selected.read()).checked_sub(*offset.read());
            slot.filter(|slot| *slot < visible_rows)
                .map(|slot| slot as f32 * row_height)
                .unwrap_or(-1.)
        });
        let highlight = use_animation_transition(highlight_slot, {
            let time = anim_time(config, 140);
            move |from: f32, to: f32| {
                // Snap when appearing from hidden; slide otherwise.
                let from = if from < 0. { to } else { from };
                AnimNum::new(from, to)
                    .time(time)
                    .function(Function::Cubic)
                    .ease(Ease::Out)
            }
        });

        // The tab indicator slides too.
        let tab_x = use_memo(move || *active.read() as f32 * (TAB_WIDTH + 4.));
        let tab_indicator = use_animation_transition(tab_x, {
            let time = anim_time(config, 220);
            move |from: f32, to: f32| {
                AnimNum::new(from, to)
                    .time(time)
                    .function(Function::Expo)
                    .ease(Ease::Out)
            }
        });

        let len = results.read().len();
        let current = (*selected.read()).min(len.saturating_sub(1));
        let active_index = *active.read();

        let mut select = move |index: usize| {
            selected.set(index);
            let top = *offset.peek();
            if index < top {
                offset.set(index);
            } else if index >= top + visible_rows {
                offset.set(index + 1 - visible_rows);
            }
        };

        let on_key = {
            let data = data.clone();
            move |event: Event<KeyboardEventData>| -> bool {
                let ctrl = event.modifiers.contains(Modifiers::CONTROL);
                let alt = event.modifiers.contains(Modifiers::ALT);
                let shift = event.modifiers.contains(Modifiers::SHIFT);
                let len = results.read().len();
                let step = |forward: bool| {
                    if len == 0 {
                        0
                    } else if forward {
                        (current + 1) % len
                    } else {
                        (current + len - 1) % len
                    }
                };
                let tab_step = |forward: bool| {
                    if forward {
                        (active_index + 1) % tab_count
                    } else {
                        (active_index + tab_count - 1) % tab_count
                    }
                };

                match &event.key {
                    Key::Named(NamedKey::Escape) => std::process::exit(1),
                    Key::Named(NamedKey::Enter) => {
                        let hit = results.read().get(current).copied();
                        data.activate(hit, &query.read());
                    }
                    // Tab / Shift+Tab switch tabs; with a single tab they move the selection.
                    Key::Named(NamedKey::Tab) if tab_count > 1 => active.set(tab_step(!shift)),
                    Key::Named(NamedKey::Tab) => select(step(!shift)),
                    Key::Named(NamedKey::ArrowDown) => select(step(true)),
                    Key::Named(NamedKey::ArrowUp) => select(step(false)),
                    Key::Named(NamedKey::PageDown) => {
                        select((current + visible_rows).min(len.saturating_sub(1)))
                    }
                    Key::Named(NamedKey::PageUp) => select(current.saturating_sub(visible_rows)),
                    Key::Character(c) if ctrl && (c == "n" || c == "j") => select(step(true)),
                    Key::Character(c) if ctrl && (c == "p" || c == "k") => select(step(false)),
                    Key::Character(c) if alt => match c.parse::<usize>() {
                        Ok(n @ 1..=9) if n <= tab_count => active.set(n - 1),
                        _ => return true,
                    },
                    _ => return true,
                }
                event.stop_propagation();
                event.prevent_default();
                false
            }
        };

        let active_tab = data.tabs[active_index];
        let placeholder = match active_tab {
            Tab::All => "Search apps, windows, emoji…",
            Tab::Module(m) => data.modules[m].placeholder,
        };

        let input = Input::new(query)
            .placeholder(
                config
                    .placeholder
                    .clone()
                    .unwrap_or_else(|| placeholder.to_string()),
            )
            .auto_focus(true)
            .width(Size::fill())
            .height(Size::px(config.input_height))
            .theme_colors(
                InputColorsThemePartial::new()
                    .background(colors.input)
                    .focus_background(colors.input)
                    .border_fill(colors.input)
                    .focus_border_fill(colors.border)
                    .color(colors.text)
                    .placeholder_color(colors.muted),
            )
            .theme_layout(InputLayoutThemePartial::new().corner_radius(config.corner_radius))
            .on_pre_key_down(on_key);

        let tabs = (tab_count > 1).then(|| {
            let has_query = !query.read().trim().is_empty();
            let counts: Vec<usize> = tab_results.read().iter().map(Vec::len).collect();
            let indicator = rect()
                .position(
                    Position::new_absolute()
                        .left(tab_indicator.get().value())
                        .top(0.),
                )
                .width(Size::px(TAB_WIDTH))
                .height(Size::px(TAB_HEIGHT))
                .corner_radius(TAB_HEIGHT / 2.)
                .background(colors.selected)
                .border(Border::new().fill(colors.border).width(1.));
            rect()
                .horizontal()
                .width(Size::fill())
                .height(Size::px(TAB_HEIGHT))
                .spacing(4.)
                .child(indicator)
                .children(data.tabs.iter().enumerate().map(|(i, tab)| {
                    let is_active = i == active_index;
                    let title = match tab {
                        Tab::All => "All",
                        Tab::Module(m) => data.modules[*m].title,
                    };
                    let count = has_query.then(|| {
                        label()
                            .text(counts[i].to_string())
                            .font_size(config.comment_font_size - 1.)
                            .color(if is_active {
                                colors.accent
                            } else {
                                colors.muted
                            })
                    });
                    rect()
                        .horizontal()
                        .width(Size::px(TAB_WIDTH))
                        .height(Size::fill())
                        .center()
                        .spacing(6.)
                        .on_press(move |_| active.set(i))
                        .child(
                            label()
                                .text(title)
                                .font_size(config.comment_font_size + 1.)
                                .font_weight(if is_active {
                                    FontWeight::BOLD
                                } else {
                                    FontWeight::NORMAL
                                })
                                .color(if is_active { colors.text } else { colors.muted }),
                        )
                        .maybe_child(count)
                        .into_element()
                }))
        });

        let pattern = Pattern::parse(&query.read(), CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let top = *offset.read();

        let rows = results
            .read()
            .iter()
            .enumerate()
            .skip(top)
            .take(visible_rows)
            .map(|(position, &(m, index))| {
                let module = &data.modules[m];
                let entry = &module.entries[index];
                // In "All", label where each result comes from.
                let badge = (active_tab == Tab::All).then_some(module.title);
                let activate_data = data.clone();
                let icon = config.show_icons.then(|| match &entry.icon {
                    Some(EntryIcon::Name(name)) => data.icons.resolve(name).map(RowIcon::Image),
                    Some(EntryIcon::Image(png)) => Some(RowIcon::Image(Icon::Svg(
                        format!("{m}:{}", entry.id).into(),
                        png.clone(),
                    ))),
                    Some(EntryIcon::Glyph(glyph)) => Some(RowIcon::Glyph(glyph.clone())),
                    None => None,
                });
                row(
                    entry,
                    m,
                    badge,
                    icon,
                    config,
                    &pattern,
                    &mut matcher,
                    position == current,
                    move || {
                        let hit = results.read().get(position).copied();
                        activate_data.activate(hit, &query.read());
                    },
                    move || {
                        if *selected.peek() != position {
                            selected.set(position);
                        }
                    },
                )
            })
            .collect::<Vec<_>>();

        let highlight_y = highlight.get().value();
        let selection = (len > 0 && *highlight_slot.read() >= 0.).then(|| {
            rect()
                .position(Position::new_absolute().top(highlight_y).left(0.))
                .horizontal()
                .width(Size::fill())
                .height(Size::px(row_height))
                .padding(Gaps::new(0., 0., 0., 4.))
                .corner_radius(config.corner_radius)
                .background(colors.selected)
                .cross_align(Alignment::Center)
                .child(
                    rect()
                        .width(Size::px(3.))
                        .height(Size::px(row_height * 0.45))
                        .corner_radius(2.)
                        .background(colors.accent),
                )
        });

        let empty_text = if match active_tab {
            Tab::All => data.modules.iter().all(|module| module.entries.is_empty()),
            Tab::Module(m) => data.modules[m].entries.is_empty(),
        } {
            "Nothing here"
        } else {
            "No matches"
        };

        let list = rect()
            .width(Size::fill())
            .height(Size::fill())
            .on_wheel(move |event: Event<WheelEventData>| {
                let max_top = len.saturating_sub(visible_rows);
                let top = *offset.peek();
                let top = if event.delta_y < 0. {
                    (top + 1).min(max_top)
                } else {
                    top.saturating_sub(1)
                };
                offset.set(top);
            })
            .maybe_child(selection)
            .children(rows)
            .maybe_child((len == 0).then(|| {
                rect()
                    .width(Size::fill())
                    .height(Size::px(row_height * 2.))
                    .center()
                    .color(colors.muted)
                    .child(empty_text)
            }));

        let progress = entrance.get().value();
        let radius = if data.rounded {
            config.window_radius
        } else {
            0.
        };

        rect()
            .width(Size::fill())
            .height(Size::fill())
            .background(colors.background)
            .corner_radius(radius)
            .border(
                data.rounded
                    .then(|| Border::new().fill(colors.input).width(1.)),
            )
            .color(colors.text)
            .font_size(config.font_size)
            .padding(Gaps::new_all(config.padding))
            .spacing(config.padding)
            .opacity(progress)
            .offset_y((1. - progress) * 12.)
            .child(input)
            .maybe_child(tabs)
            .child(list)
    }
}

enum RowIcon {
    Image(Icon),
    Glyph(String),
}

#[allow(clippy::too_many_arguments)]
fn row(
    entry: &Entry,
    module: usize,
    badge: Option<&'static str>,
    // `None` hides the icon column, `Some(None)` keeps the space for alignment.
    icon: Option<Option<RowIcon>>,
    config: &Config,
    pattern: &Pattern,
    matcher: &mut Matcher,
    is_selected: bool,
    mut on_activate: impl FnMut() + 'static,
    mut on_hover: impl FnMut() + 'static,
) -> Element {
    // Highlight the characters of the name that matched the query.
    let mut buf = Vec::new();
    let mut matched = Vec::new();
    pattern.indices(Utf32Str::new(&entry.name, &mut buf), matcher, &mut matched);

    let name = paragraph()
        .width(Size::fill())
        .font_size(config.font_size)
        .max_lines(1)
        .text_overflow(TextOverflow::Ellipsis)
        .spans_iter(entry.name.chars().enumerate().map(|(i, c)| {
            let span = Span::new(c.to_string());
            if matched.contains(&(i as u32)) {
                span.color(config.colors.accent)
                    .font_weight(FontWeight::BOLD)
            } else {
                span.color(config.colors.text).font_weight(if is_selected {
                    FontWeight::MEDIUM
                } else {
                    FontWeight::NORMAL
                })
            }
        }));

    let comment = entry
        .comment
        .as_ref()
        .filter(|_| config.show_comments)
        .map(|comment| {
            label()
                .text(comment.clone())
                .width(Size::fill())
                .font_size(config.comment_font_size)
                .color(config.colors.muted)
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis)
        });

    let text = rect()
        .width(Size::fill())
        .main_align(Alignment::Center)
        .spacing(2.)
        .child(name)
        .maybe_child(comment);

    let icon = icon.map(|icon| {
        let size = Size::px(config.icon_size);
        let content: Element = match icon {
            Some(RowIcon::Image(Icon::Svg(path, png))) => ImageViewer::new((path, png))
                .loading_placeholder(rect())
                .width(size.clone())
                .height(size.clone())
                .into(),
            Some(RowIcon::Image(Icon::Raster(path))) => ImageViewer::new(path)
                .loading_placeholder(rect())
                .width(size.clone())
                .height(size.clone())
                .into(),
            Some(RowIcon::Glyph(glyph)) => {
                label().text(glyph).font_size(config.icon_size * 0.8).into()
            }
            None => rect().into(),
        };
        rect()
            .width(size.clone())
            .height(size)
            .center()
            .key(format!("{module}:{}", entry.id))
            .child(content)
    });

    let badge = badge.map(|badge| {
        rect()
            .padding(Gaps::new(3., 8., 3., 8.))
            .corner_radius(10.)
            .background(config.colors.input)
            .child(
                label()
                    .text(badge)
                    .font_size(config.comment_font_size - 1.)
                    .color(config.colors.muted),
            )
    });

    rect()
        .horizontal()
        .width(Size::fill())
        .height(Size::px(config.row_height))
        .padding(Gaps::new(0., 12., 0., 14.))
        .cross_align(Alignment::Center)
        .spacing(12.)
        .on_press(move |_| on_activate())
        .on_pointer_move(move |_| on_hover())
        .maybe_child(icon)
        .child(text)
        .maybe_child(badge)
        .into()
}
