use std::{
    cell::{Cell, OnceCell},
    error::Error,
    io::Write,
    rc::Rc,
    time::Duration,
};

use freya::{
    animation::*,
    elements::image::{ImageHandle, image},
    engine::prelude::AlphaType,
    prelude::*,
};
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

use crate::{
    backdrop::Backdrop,
    config::{Config, TAB_HEIGHT},
    emoji,
    entries::{self, Action, Entry, EntryIcon},
    icons::{Icon, IconResolver},
    modules::{self, Hit, Module, Ranked, Tab},
    windows,
};

pub const TAB_WIDTH: f32 = 96.;

/// Where results go: stdout and the exit code normally, a recorder in tests.
pub trait Host {
    fn print(&self, line: &str);
    fn exit(&self, code: i32);
}

pub struct Process;

impl Host for Process {
    fn print(&self, line: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    fn exit(&self, code: i32) {
        std::process::exit(code);
    }
}

#[derive(Clone, Default)]
pub struct DmenuOptions {
    /// Shown left of the input.
    pub prompt: Option<String>,
    /// Print the index of the picked line instead of the line.
    pub index: bool,
    /// Shift+Enter marks lines; Enter prints all marked lines.
    pub multi: bool,
    /// Never print typed text that matches no line.
    pub no_custom: bool,
}

pub struct LauncherData {
    pub modules: Vec<Module>,
    pub tabs: Vec<Tab>,
    /// Index into `tabs` shown on start.
    pub initial: usize,
    pub icons: IconResolver,
    /// Transparent window with rounded corners (a compositor is running).
    pub rounded: bool,
    pub config: Config,
    /// Set in dmenu mode.
    pub dmenu: Option<DmenuOptions>,
    pub host: Box<dyn Host>,
    pub window: Rc<WindowInfo>,
}

/// Filled in while the native window is created.
#[derive(Default)]
pub struct WindowInfo {
    /// The blurred desktop of the monitor the window opens on.
    pub backdrop: OnceCell<Backdrop>,
    pub id: Cell<Option<u32>>,
    /// Requested top-left corner in physical root window pixels.
    pub position: Cell<Option<(i32, i32)>>,
}

impl LauncherData {
    /// Records the use and runs the entry's action.
    fn run(&self, module: &Module, entry: &Entry) -> Result<(), Box<dyn Error>> {
        module.record(entry);
        let copy_command = self.config.copy_command.as_deref();
        match &entry.action {
            Action::Print(index) => {
                if self.dmenu.as_ref().is_some_and(|dmenu| dmenu.index) {
                    self.host.print(&index.to_string());
                } else {
                    self.host.print(&entry.name);
                }
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
                self.config.emoji.copy_command.as_deref().or(copy_command),
            )
            .map_err(Into::into),
            Action::Copy(text) => entries::copy(text, copy_command).map_err(Into::into),
        }
    }

    /// Runs `hit`, then exits unless `keep_open`. Entries that need confirmation only arm on
    /// the first call. Returns whether the hit is now armed.
    fn activate(
        &self,
        ranked: &[Ranked],
        hit: Option<Hit>,
        query: &str,
        keep_open: bool,
        armed: Option<Hit>,
    ) -> bool {
        let Some((m, i)) = hit else {
            let custom = self
                .dmenu
                .as_ref()
                .is_some_and(|dmenu| !dmenu.no_custom && !dmenu.index);
            if custom && !query.is_empty() {
                self.host.print(query);
                if !keep_open {
                    self.host.exit(0);
                }
            } else if !keep_open {
                self.host.exit(1);
            }
            return false;
        };

        let module = &self.modules[m];
        let entry = ranked[m].entry(module, i);
        if entry.confirm && armed != hit {
            return true;
        }
        let result = self.run(module, entry);
        if let Err(err) = &result {
            eprintln!("launch: {:?} failed: {err}", entry.name);
        }
        if !keep_open || result.is_err() {
            self.host.exit(if result.is_ok() { 0 } else { 1 });
        }
        false
    }

    /// Prints every marked line in input order and exits (dmenu multi-select).
    fn print_marked(&self, ranked: &[Ranked], marked: &[Hit]) {
        let mut marked = marked.to_vec();
        marked.sort();
        for (m, i) in marked {
            let _ = self.run(&self.modules[m], ranked[m].entry(&self.modules[m], i));
        }
        self.host.exit(0);
    }

    /// Closes a window, or forgets the entry's usage history. Returns whether anything changed.
    fn remove(&self, ranked: &[Ranked], (m, i): Hit) -> bool {
        let module = &self.modules[m];
        let entry = ranked[m].entry(module, i);
        match entry.action {
            Action::Focus { window, .. } => match windows::close(window) {
                Ok(()) => {
                    module.hide(entry);
                    true
                }
                Err(err) => {
                    eprintln!("launch: cannot close {:?}: {err}", entry.name);
                    false
                }
            },
            _ => module.forget(entry),
        }
    }
}

/// Duration for UI animations, or instant when they are disabled.
fn anim_time(config: &Config, millis: u64) -> u64 {
    if config.animations { millis } else { 1 }
}

pub struct Launcher {
    pub data: Rc<LauncherData>,
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
        let multi = data.dmenu.as_ref().is_some_and(|dmenu| dmenu.multi);

        let query = use_state(String::new);
        let mut active = use_state(|| data.initial);
        let mut selected = use_state(|| 0usize);
        let mut offset = use_state(|| 0usize);
        // Bumped when entries are removed or forgotten, to rank again.
        let mut revision = use_state(|| 0u32);
        // The entry waiting for a second Enter to confirm.
        let mut armed = use_state(|| None::<Hit>);
        let mut marked = use_state(Vec::<Hit>::new);

        // Rank every module once per query; tabs pick from (or merge) these.
        let ranked = use_memo({
            let data = data.clone();
            move || {
                let _ = revision.read();
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
                        Tab::Module(m) => ranked[*m].hits.iter().map(|&(_, i)| (*m, i)).collect(),
                    })
                    .collect::<Vec<Vec<Hit>>>()
            }
        });
        let hit_at =
            move |position: usize| tab_results.peek()[*active.peek()].get(position).copied();

        // A new query or tab starts back at the top.
        use_side_effect(move || {
            let _ = query.read();
            let _ = active.read();
            selected.set(0);
            offset.set(0);
            armed.set(None);
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

        let backdrop_image = use_hook(|| {
            let backdrop = data.window.backdrop.get()?;
            ImageHandle::from_rgba(
                backdrop.width,
                backdrop.height,
                backdrop.bytes(),
                AlphaType::Opaque,
            )
        });
        // The window manager may move the window when mapping it; follow it so the backdrop
        // lines up with the real desktop.
        let mut window_position = use_state(|| data.window.position.get());
        use_hook(|| {
            let (Some(_), Some(id)) = (data.window.backdrop.get(), data.window.id.get()) else {
                return;
            };
            spawn(async move {
                for _ in 0..40 {
                    if let Some(position) = thread(move || windows::position(id).ok()).await {
                        window_position.set_if_modified(Some(position));
                    }
                    timer(Duration::from_millis(25)).await;
                }
            });
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

        let len = tab_results.read()[*active.read()].len();
        let current = (*selected.read()).min(len.saturating_sub(1));
        let active_index = *active.read();

        // The selection highlight slides between rows instead of jumping.
        let highlight_slot = use_memo(move || {
            let len = tab_results.read()[*active.read()].len();
            let current = (*selected.read()).min(len.saturating_sub(1));
            let slot = current.checked_sub(*offset.read());
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

        let mut select = move |index: usize| {
            selected.set(index);
            armed.set_if_modified(None);
            let top = *offset.peek();
            if index < top {
                offset.set(index);
            } else if index >= top + visible_rows {
                offset.set(index + 1 - visible_rows);
            }
        };

        let activate = {
            let data = data.clone();
            move |position: usize, keep_open: bool| {
                let hit = hit_at(position);
                if multi && keep_open {
                    if let Some(hit) = hit {
                        let mut marked = marked.write();
                        match marked.iter().position(|marked| *marked == hit) {
                            Some(index) => {
                                marked.remove(index);
                            }
                            None => marked.push(hit),
                        }
                    }
                    return;
                }
                let ranked = ranked.peek();
                if multi && !marked.peek().is_empty() {
                    data.print_marked(&ranked, &marked.peek());
                    return;
                }
                let arm = data.activate(&ranked, hit, &query.peek(), keep_open, *armed.peek());
                armed.set(if arm { hit } else { None });
            }
        };

        let mut remove = {
            let data = data.clone();
            move |position: usize| {
                let Some(hit) = hit_at(position) else {
                    return;
                };
                let changed = data.remove(&ranked.peek(), hit);
                if changed {
                    *revision.write() += 1;
                }
            }
        };

        let on_key = {
            let data = data.clone();
            let mut activate = activate.clone();
            move |event: Event<KeyboardEventData>| -> bool {
                let ctrl = event.modifiers.contains(Modifiers::CONTROL);
                let alt = event.modifiers.contains(Modifiers::ALT);
                let shift = event.modifiers.contains(Modifiers::SHIFT);
                let len = tab_results.peek()[*active.peek()].len();
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
                    Key::Named(NamedKey::Escape) => data.host.exit(1),
                    Key::Named(NamedKey::Enter) => {
                        activate(current, shift);
                        if shift && multi {
                            select(step(true));
                        }
                    }
                    Key::Named(NamedKey::Delete) if shift => remove(current),
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
            .width(Size::flex(1.))
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

        let prompt = data.dmenu.as_ref().and_then(|dmenu| dmenu.prompt.clone());
        let input_row = rect()
            .horizontal()
            .content(Content::flex())
            .width(Size::fill())
            .height(Size::px(config.input_height))
            .cross_align(Alignment::Center)
            .spacing(config.padding)
            .map(prompt, |el, prompt| {
                el.child(
                    label()
                        .text(prompt)
                        .color(colors.accent)
                        .font_weight(FontWeight::BOLD),
                )
            })
            .child(input);

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
        let ranked_now = ranked.read();
        let armed_now = *armed.read();
        let marked_now = marked.read();

        let rows = tab_results.read()[active_index]
            .iter()
            .enumerate()
            .skip(top)
            .take(visible_rows)
            .map(|(position, &(m, index))| {
                let module = &data.modules[m];
                let entry = ranked_now[m].entry(module, index);
                // In "All", label where each result comes from.
                let badge = (active_tab == Tab::All).then_some(module.title);
                let icon = config.show_icons.then(|| match &entry.icon {
                    Some(EntryIcon::Name(name)) => data.icons.resolve(name).map(RowIcon::Image),
                    Some(EntryIcon::Image(png)) => Some(RowIcon::Image(Icon::Svg(
                        format!("{m}:{}", entry.id).into(),
                        png.clone(),
                    ))),
                    Some(EntryIcon::Glyph(glyph)) => Some(RowIcon::Glyph(glyph.clone())),
                    None => None,
                });
                let state = RowState {
                    selected: position == current,
                    armed: armed_now == Some((m, index)),
                    marked: marked_now.contains(&(m, index)),
                };
                let mut activate = activate.clone();
                row(
                    entry,
                    m,
                    badge,
                    icon,
                    config,
                    &pattern,
                    &mut matcher,
                    state,
                    move || activate(position, false),
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

        let is_empty = |module: &Module| module.entries.is_empty() && module.compute.is_none();
        let empty_text = match active_tab {
            Tab::Module(m) if data.modules[m].compute.is_some() => {
                "Type a calculation, like 2^10 / 3"
            }
            Tab::Module(m) if is_empty(&data.modules[m]) => "Nothing here",
            Tab::All if data.modules.iter().all(is_empty) => "Nothing here",
            _ => "No matches",
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
        let scale = *Platform::get().current_window().scale_factor.read() as f32;
        let backdrop = data
            .window
            .backdrop
            .get()
            .zip(backdrop_image.clone())
            .zip(*window_position.read())
            .map(|((backdrop, handle), (x, y))| {
                let monitor = backdrop.monitor;
                image(handle)
                    // Siblings share a layer and could paint over the tinted panel.
                    .layer(Layer::Relative(-1))
                    .position(
                        Position::new_absolute()
                            .left((monitor.x - x) as f32 / scale)
                            .top((monitor.y - y) as f32 / scale),
                    )
                    .width(Size::px(monitor.width as f32 / scale))
                    .height(Size::px(monitor.height as f32 / scale))
                    .aspect_ratio(AspectRatio::None)
                    .sampling_mode(SamplingMode::Bilinear)
            });
        let background = if backdrop.is_some() {
            // Scales the color's own alpha.
            colors
                .tint
                .unwrap_or(colors.background)
                .with_af32(config.blur_opacity.clamp(0., 1.))
        } else {
            colors.background
        };

        let panel = rect()
            .width(Size::fill())
            .height(Size::fill())
            .background(background)
            .corner_radius(radius)
            .border(
                data.rounded
                    .then(|| Border::new().fill(colors.input).width(1.)),
            )
            .color(colors.text)
            .font_size(config.font_size)
            .padding(Gaps::new_all(config.padding))
            .spacing(config.padding)
            .offset_y((1. - progress) * 12.)
            .child(input_row)
            .maybe_child(tabs)
            .child(list);

        rect()
            .width(Size::fill())
            .height(Size::fill())
            .corner_radius(radius)
            .overflow(Overflow::Clip)
            .opacity(progress)
            .maybe_child(backdrop)
            .child(panel)
    }
}

enum RowIcon {
    Image(Icon),
    Glyph(String),
}

#[derive(Clone, Copy)]
struct RowState {
    selected: bool,
    /// Waiting for a second Enter to confirm.
    armed: bool,
    /// Marked for printing (dmenu multi-select).
    marked: bool,
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
    state: RowState,
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
                span.color(config.colors.text)
                    .font_weight(if state.selected {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
            }
        }));

    let comment = if state.armed {
        Some((
            format!("Press Enter again to confirm {}", entry.name.to_lowercase()),
            config.colors.accent,
        ))
    } else {
        entry
            .comment
            .clone()
            .filter(|_| config.show_comments)
            .map(|comment| (comment, config.colors.muted))
    };
    let comment = comment.map(|(comment, color)| {
        label()
            .text(comment)
            .width(Size::fill())
            .font_size(config.comment_font_size)
            .color(color)
            .max_lines(1)
            .text_overflow(TextOverflow::Ellipsis)
    });

    // Flex so the text only takes what the icon and badge leave over; `fill` would
    // squeeze the badge to zero width and wrap it one character per line.
    let text = rect()
        .width(Size::flex(1.))
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
        .content(Content::flex())
        .cross_align(Alignment::Center)
        .spacing(12.)
        .on_press(move |_| on_activate())
        .on_pointer_move(move |_| on_hover())
        .maybe_child(icon)
        .child(text)
        .maybe_child(badge)
        .maybe(state.marked, |el| {
            el.child(
                label()
                    .text("✓")
                    .font_weight(FontWeight::BOLD)
                    .color(config.colors.accent),
            )
        })
        .into()
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use freya_testing::prelude::*;

    use super::*;
    use crate::{entries::dmenu_entries, power};

    #[derive(Default)]
    struct Recorder {
        printed: RefCell<Vec<String>>,
        exit: Cell<Option<i32>>,
    }

    impl Host for Rc<Recorder> {
        fn print(&self, line: &str) {
            self.printed.borrow_mut().push(line.to_string());
        }

        fn exit(&self, code: i32) {
            self.exit.set(Some(code));
        }
    }

    fn config() -> Config {
        Config {
            animations: false,
            close_on_unfocus: false,
            show_icons: false,
            ..Config::default()
        }
    }

    fn runner(modules: Vec<Module>, dmenu: Option<DmenuOptions>) -> (TestingRunner, Rc<Recorder>) {
        runner_with_window(modules, dmenu, WindowInfo::default())
    }

    fn runner_with_window(
        modules: Vec<Module>,
        dmenu: Option<DmenuOptions>,
        window: WindowInfo,
    ) -> (TestingRunner, Rc<Recorder>) {
        let recorder = Rc::new(Recorder::default());
        let config = config();
        let mut tabs: Vec<Tab> = (0..modules.len()).map(Tab::Module).collect();
        if modules.len() > 1 {
            tabs.insert(0, Tab::All);
        }
        let size = (config.width, config.window_height(tabs.len() > 1));
        let data = LauncherData {
            modules,
            tabs,
            initial: 0,
            icons: IconResolver::new(Some("hicolor"), 32),
            rounded: false,
            config,
            dmenu,
            host: Box::new(recorder.clone()),
            window: Rc::new(window),
        };
        let app = freya_core::element::AppComponent::new(Launcher {
            data: Rc::new(data),
        });
        let (mut runner, ()) = TestingRunner::new(app, size.into(), |_| {}, 1.);
        settle(&mut runner);
        (runner, recorder)
    }

    fn dmenu(lines: &str) -> Module {
        Module::new("dmenu", "Filter…", dmenu_entries(lines))
    }

    /// Memos recompute in async tasks, one level per tick.
    fn settle(runner: &mut TestingRunner) {
        runner.poll_n(std::time::Duration::ZERO, 4);
    }

    fn key(runner: &mut TestingRunner, key: Key, modifiers: Modifiers) {
        runner.send_event(PlatformEvent::Keyboard {
            name: KeyboardEventName::KeyDown,
            key,
            code: Code::Unidentified,
            modifiers,
        });
        settle(runner);
    }

    fn enter(runner: &mut TestingRunner, modifiers: Modifiers) {
        key(runner, Key::Named(NamedKey::Enter), modifiers);
    }

    fn type_text(runner: &mut TestingRunner, text: &str) {
        for c in text.chars() {
            runner.write_text(c);
            settle(runner);
        }
    }

    fn texts(runner: &TestingRunner) -> Vec<String> {
        runner.find_many(|_, element| {
            Label::try_downcast(element).map(|label| label.text.to_string())
        })
    }

    #[test]
    fn dmenu_prints_the_filtered_line() {
        let (mut runner, recorder) = runner(vec![dmenu("apple\nbanana\ncherry")], None);
        type_text(&mut runner, "ban");
        enter(&mut runner, Modifiers::empty());
        assert_eq!(*recorder.printed.borrow(), ["banana"]);
        assert_eq!(recorder.exit.get(), Some(0));
    }

    #[test]
    fn dmenu_prints_index_and_custom_text() {
        let options = DmenuOptions {
            index: true,
            ..DmenuOptions::default()
        };
        let (mut runner, recorder) = runner(vec![dmenu("apple\nbanana")], Some(options));
        key(
            &mut runner,
            Key::Named(NamedKey::ArrowDown),
            Modifiers::empty(),
        );
        enter(&mut runner, Modifiers::empty());
        assert_eq!(*recorder.printed.borrow(), ["1"]);

        let (mut runner, recorder) = runner_custom(false);
        type_text(&mut runner, "zzz");
        enter(&mut runner, Modifiers::empty());
        assert_eq!(*recorder.printed.borrow(), ["zzz"]);
        assert_eq!(recorder.exit.get(), Some(0));

        let (mut runner, recorder) = runner_custom(true);
        type_text(&mut runner, "zzz");
        enter(&mut runner, Modifiers::empty());
        assert!(recorder.printed.borrow().is_empty());
        assert_eq!(recorder.exit.get(), Some(1));
    }

    fn runner_custom(no_custom: bool) -> (TestingRunner, Rc<Recorder>) {
        let options = DmenuOptions {
            no_custom,
            ..DmenuOptions::default()
        };
        runner(vec![dmenu("apple\nbanana")], Some(options))
    }

    #[test]
    fn dmenu_multi_select_prints_marked_lines_in_order() {
        let options = DmenuOptions {
            multi: true,
            ..DmenuOptions::default()
        };
        let (mut runner, recorder) = runner(vec![dmenu("a\nb\nc")], Some(options));
        key(
            &mut runner,
            Key::Named(NamedKey::ArrowDown),
            Modifiers::empty(),
        );
        key(
            &mut runner,
            Key::Named(NamedKey::ArrowDown),
            Modifiers::empty(),
        );
        enter(&mut runner, Modifiers::SHIFT);
        assert_eq!(recorder.exit.get(), None);
        enter(&mut runner, Modifiers::SHIFT);
        assert!(texts(&runner).iter().filter(|text| *text == "✓").count() == 2);
        enter(&mut runner, Modifiers::empty());
        assert_eq!(*recorder.printed.borrow(), ["a", "c"]);
        assert_eq!(recorder.exit.get(), Some(0));
    }

    #[test]
    fn keep_open_prints_without_exiting() {
        let (mut runner, recorder) = runner(vec![dmenu("a\nb")], Some(DmenuOptions::default()));
        enter(&mut runner, Modifiers::SHIFT);
        key(
            &mut runner,
            Key::Named(NamedKey::ArrowDown),
            Modifiers::empty(),
        );
        enter(&mut runner, Modifiers::SHIFT);
        assert_eq!(*recorder.printed.borrow(), ["a", "b"]);
        assert_eq!(recorder.exit.get(), None);
    }

    #[test]
    fn shows_the_prompt() {
        let options = DmenuOptions {
            prompt: Some("Pick:".to_string()),
            ..DmenuOptions::default()
        };
        let (runner, _) = runner(vec![dmenu("a")], Some(options));
        assert!(texts(&runner).contains(&"Pick:".to_string()));
    }

    #[test]
    fn calculator_shows_results_in_all() {
        let calc = Module::load(crate::modules::ModuleKind::Calc, 32, &config());
        let (mut runner, _) = runner(vec![calc, dmenu("apples\nbananas")], None);
        type_text(&mut runner, "6*7");
        assert!(texts(&runner).contains(&"Calc".to_string()));
        let names = runner.find_many(|_, element| {
            Paragraph::try_downcast(element).map(|el| {
                el.spans
                    .iter()
                    .map(|span| span.text.as_ref())
                    .collect::<String>()
            })
        });
        assert!(names.contains(&"42".to_string()), "{names:?}");
    }

    #[test]
    fn destructive_entries_need_a_second_enter() {
        let power = Module::new(
            "Power",
            "",
            power::power_entries(&power::PowerConfig {
                lock: "true".to_string(),
                suspend: String::new(),
                logout: String::new(),
                shutdown: String::new(),
                reboot: "true".to_string(),
                ..power::PowerConfig::default()
            }),
        );
        let (mut runner, recorder) = runner(vec![power], None);
        type_text(&mut runner, "reboot");
        enter(&mut runner, Modifiers::empty());
        assert_eq!(recorder.exit.get(), None);
        assert!(
            texts(&runner)
                .iter()
                .any(|text| text.starts_with("Press Enter again"))
        );
        enter(&mut runner, Modifiers::empty());
        assert_eq!(recorder.exit.get(), Some(0));
    }

    #[test]
    fn badges_keep_their_width() {
        let (mut runner, _) = runner(
            vec![
                dmenu("a very long line that would push the badge out of the row"),
                dmenu("b"),
            ],
            None,
        );
        settle(&mut runner);
        let widths = runner.find_many(|node, element| {
            Label::try_downcast(element)
                .filter(|label| label.text.as_ref() == "dmenu")
                .map(|_| node.layout().area.width())
        });
        assert!(!widths.is_empty());
        assert!(widths.iter().all(|width| *width > 20.), "{widths:?}");
    }

    /// A window at `position` over a stand-in desktop with hard-edged stripes.
    fn striped_desktop(position: (i32, i32)) -> WindowInfo {
        let (width, height) = (200, 150);
        let rgba = (0..width * height)
            .flat_map(|i| {
                [[230, 60, 60, 255], [60, 200, 90, 255], [70, 110, 240, 255]][(i % width) / 20 % 3]
            })
            .collect();
        let mut backdrop = Backdrop {
            monitor: crate::backdrop::Monitor {
                x: 0,
                y: 0,
                width: width as u32 * crate::backdrop::DOWNSCALE,
                height: height as u32 * crate::backdrop::DOWNSCALE,
            },
            width: width as u32,
            height: height as u32,
            rgba,
        };
        backdrop.blur(config().blur);
        let window = WindowInfo::default();
        let _ = window.backdrop.set(backdrop);
        window.position.set(Some(position));
        window
    }

    /// Alphas of the rects filled with `color`, ignoring its own alpha.
    fn fill_alphas(runner: &TestingRunner, color: Color) -> Vec<u8> {
        runner.find_many(|_, element| {
            let rect = Rect::try_downcast(element)?;
            match rect.style.background {
                Fill::Color(fill)
                    if (fill.r(), fill.g(), fill.b()) == (color.r(), color.g(), color.b()) =>
                {
                    Some(fill.a())
                }
                _ => None,
            }
        })
    }

    fn panel_alpha(runner: &TestingRunner) -> Vec<u8> {
        fill_alphas(runner, config().colors.background)
    }

    #[test]
    fn draws_the_desktop_behind_a_tinted_background() {
        let (blurred, _) = runner_with_window(vec![dmenu("a\nb")], None, striped_desktop((40, 30)));
        let image = blurred.find(|node, element| {
            freya_core::elements::image::Image::try_downcast(element)
                .map(|image| (image.image_data.blur, node.layout().area))
        });
        let (blur, area) = image.expect("backdrop image");
        assert_eq!(blur, 0., "blurred on the CPU, not by a GPU filter");
        assert_eq!((area.min_x(), area.min_y()), (-40., -30.));
        assert_eq!(
            panel_alpha(&blurred),
            [(255. * config().blur_opacity) as u8]
        );

        let (plain, _) = runner(vec![dmenu("a\nb")], None);
        assert!(
            plain
                .find(|_, element| freya_core::elements::image::Image::try_downcast(element))
                .is_none()
        );
        assert_eq!(panel_alpha(&plain), [255]);
    }

    #[test]
    fn tints_the_desktop_with_the_tint_color() {
        let tint = Color::from_argb(200, 90, 20, 140);
        let mut config = config();
        config.colors.tint = Some(tint);
        let window = striped_desktop((0, 0));
        let data = LauncherData {
            modules: vec![dmenu("a")],
            tabs: vec![Tab::Module(0)],
            initial: 0,
            icons: IconResolver::new(Some("hicolor"), 32),
            rounded: false,
            dmenu: None,
            host: Box::new(Rc::new(Recorder::default())),
            window: Rc::new(window),
            config: config.clone(),
        };
        let size = (config.width, config.window_height(false));
        let app = freya_core::element::AppComponent::new(Launcher {
            data: Rc::new(data),
        });
        let (mut runner, ()) = TestingRunner::new(app, size.into(), |_| {}, 1.);
        settle(&mut runner);
        let expected = 200. * config.blur_opacity;
        let alphas = fill_alphas(&runner, tint);
        assert!(
            alphas.len() == 1 && (alphas[0] as f32 - expected).abs() <= 1.,
            "{alphas:?}"
        );
        assert!(panel_alpha(&runner).is_empty());
    }

    #[test]
    fn opaque_tint_hides_the_desktop() {
        let tint = Color::from_rgb(60, 30, 110);
        let mut config = config();
        config.colors.tint = Some(tint);
        config.blur_opacity = 1.;
        let data = LauncherData {
            modules: vec![dmenu("a")],
            tabs: vec![Tab::Module(0)],
            initial: 0,
            icons: IconResolver::new(Some("hicolor"), 32),
            rounded: false,
            dmenu: None,
            host: Box::new(Rc::new(Recorder::default())),
            window: Rc::new(striped_desktop((0, 0))),
            config: config.clone(),
        };
        let size = (config.width, config.window_height(false));
        let app = freya_core::element::AppComponent::new(Launcher {
            data: Rc::new(data),
        });
        let (mut runner, ()) = TestingRunner::new(app, size.into(), |_| {}, 1.);
        // Rendering needs the entrance fade to have run.
        runner.poll(Duration::from_millis(5), Duration::from_millis(100));
        let png = runner.render();
        let pixmap = resvg::tiny_skia::Pixmap::decode_png(png.as_bytes()).unwrap();
        // Below the only row, where nothing but the panel covers the desktop.
        let (x, y) = (size.0 as u32 / 2, size.1 as u32 - 20);
        let pixel = pixmap.pixel(x, y).unwrap().demultiply();
        assert_eq!(
            (pixel.red(), pixel.green(), pixel.blue()),
            (tint.r(), tint.g(), tint.b())
        );
    }
}
