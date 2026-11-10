//! The launcher's rune view and its interaction state: one query, one active tab, per-module
//! fuzzy rankings (nucleo, identical scoring to the Freya launch), an "All" tab that merges
//! them, and the keyboard model (Enter, Shift+Enter, Tab, arrows, dmenu multi-select).

use std::cell::RefCell;
use std::collections::HashMap;

use rune::tok::type_;
use rune::{Color, El, Key, KeyEvent, Tx, col, el, row, spacer, text, text_input};

use crate::config::Config;
use crate::entries::{Action, Entry, EntryIcon};
use crate::icons::IconResolver;
use crate::modules::{Hit, Module, Ranked, Tab, merge};

/// Readout for dmenu mode: flagged from the CLI.
#[derive(Clone, Debug, Default)]
pub struct DmenuOptions {
    /// Shown left of the input.
    pub prompt: Option<String>,
    /// Print the index of the picked line instead of the line.
    pub index: bool,
    /// Shift+Enter marks lines, Enter prints every marked one.
    pub multi: bool,
    /// Never print typed text when nothing matches.
    pub no_custom: bool,
}

/// Where activation lands. Print goes to real stdout outside tests; `exit` kills the process
/// only outside headless runs and tests, so `--snap`/`--tree`/`--script` stay deterministic
/// and unit tests can assert on what was printed and which exit code was chosen.
pub struct Host {
    printed: RefCell<Vec<String>>,
    exit_code: RefCell<Option<i32>>,
    fake: bool,
}

impl Host {
    pub fn new() -> Self {
        Self {
            printed: RefCell::new(Vec::new()),
            exit_code: RefCell::new(None),
            fake: false,
        }
    }

    /// A host for unit tests: captures output and never exits the process.
    #[cfg(test)]
    pub fn fake() -> Self {
        Self {
            printed: RefCell::new(Vec::new()),
            exit_code: RefCell::new(None),
            fake: true,
        }
    }

    fn headless(&self) -> bool {
        rune::tooling::headless_run().is_some()
    }

    pub fn print(&self, line: &str) {
        self.printed.borrow_mut().push(line.to_string());
        // The pick is the dmenu contract: write it even in headless runs. Only the fake
        // test host captures without emitting.
        if !self.fake {
            use std::io::Write;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = writeln!(lock, "{line}");
        }
    }

    pub fn exit(&self, code: i32) {
        *self.exit_code.borrow_mut() = Some(code);
        if self.fake || self.headless() {
            return;
        }
        std::process::exit(code);
    }

    #[cfg(test)]
    fn printed(&self) -> Vec<String> {
        self.printed.borrow().clone()
    }

    #[cfg(test)]
    fn exit_code(&self) -> Option<i32> {
        *self.exit_code.borrow()
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

/// Everything the launcher window shows and how the user drives it.
pub struct State {
    pub modules: Vec<Module>,
    pub tabs: Vec<Tab>,
    pub config: Config,
    pub icons: IconResolver,
    pub dmenu: Option<DmenuOptions>,
    pub host: Host,
    /// Whether a compositor is up, so the window can be transparent and rounded.
    pub rounded: bool,
    /// The captured desktop behind the window, blurred and tinted when `config.blur > 0`;
    /// headless runs inject the `glass` scene's synthetic desktop instead.
    pub backdrop: Option<rune::Pixels>,

    pub query: rune::TextEdit,
    /// Bumped on remove/forget so rankings recompute even when the query is unchanged.
    pub query_rev: u32,
    /// Index into `tabs`.
    pub active: usize,
    /// Index into the active tab's `Vec<Hit>`.
    pub selected: usize,
    /// First visible row.
    pub offset: usize,
    /// Entry awaiting a second Enter.
    pub armed: Option<Hit>,
    /// dmenu multi-select marks, in pick order.
    pub marked: Vec<Hit>,
    /// Whether the window currently has keyboard focus (for close-on-unfocus).
    pub focused: bool,
    /// Rankings for the last query, keyed by (query revision, query) so recomputation only
    /// happens when one of them changes. A `RefCell` lets the read-only `view` share it.
    ranked_cache: RefCell<Option<(u32, String, Vec<Ranked>)>>,
    /// Decoded icons by resolver identity, so a row reuses its texture across renders.
    pixels: RefCell<HashMap<String, rune::Pixels>>,
}

impl State {
    pub fn new(
        modules: Vec<Module>,
        tabs: Vec<Tab>,
        initial: usize,
        config: Config,
        icons: IconResolver,
        dmenu: Option<DmenuOptions>,
        rounded: bool,
        backdrop: Option<rune::Pixels>,
    ) -> Self {
        let mut query = rune::TextEdit::new("");
        query.focused = true;
        Self {
            modules,
            tabs,
            config,
            icons,
            dmenu,
            host: Host::new(),
            rounded,
            backdrop,
            query,
            query_rev: 0,
            active: initial,
            selected: 0,
            offset: 0,
            armed: None,
            marked: Vec::new(),
            focused: false,
            ranked_cache: RefCell::new(None),
            pixels: RefCell::new(HashMap::new()),
        }
    }

    /// Resets navigation when the query changes.
    pub fn on_query_changed(&mut self) {
        self.selected = 0;
        self.offset = 0;
        self.armed = None;
    }

    /// Sets the query programmatically (scenes, tests).
    pub fn set_query(&mut self, text: &str) {
        self.query.clear();
        self.query.insert(text);
        self.query.set_caret(self.query.text.len());
        self.on_query_changed();
    }

    fn has_query(&self) -> bool {
        !self.query.text.trim().is_empty()
    }

    fn len(&self) -> usize {
        self.tab_hits().len()
    }

    /// The hits of the active tab: the merged "All" list, or one module's ranking.
    fn tab_hits(&self) -> Vec<Hit> {
        let ranked = self.ranked();
        match self.tabs[self.active] {
            Tab::All => merge(&self.modules, &ranked, self.has_query()),
            Tab::Module(m) => ranked[m]
                .hits
                .iter()
                .map(|&(_, i)| (m, i))
                .collect(),
        }
    }

    /// Per-module rankings for the query, cached until the query or a revision changes.
    fn ranked(&self) -> Vec<Ranked> {
        let query = self.query.text.clone();
        let cached = {
            let cache = self.ranked_cache.borrow();
            cache
                .as_ref()
                .filter(|(rev, text, _)| *rev == self.query_rev && *text == query)
                .map(|(_, _, ranked)| ranked.clone())
        };
        if let Some(ranked) = cached {
            return ranked;
        }
        let ranked: Vec<Ranked> = self.modules.iter().map(|module| module.rank(&query)).collect();
        *self.ranked_cache.borrow_mut() = Some((self.query_rev, query, ranked.clone()));
        ranked
    }

    fn hit_at(&self, position: usize) -> Option<Hit> {
        let hits = self.tab_hits();
        if hits.is_empty() {
            return None;
        }
        hits.get(position.min(hits.len() - 1)).copied()
    }

    fn visible_rows(&self) -> usize {
        self.config.rows.max(1)
    }

    /// The visible entry under `position`, owned (a clone), with its module index.
    fn entry_at(&self, position: usize) -> Option<(usize, Entry)> {
        let hit = self.hit_at(position)?;
        self.entry_at_hit(hit)
    }

    /// The entry a hit points at, whether from a module's own list or its computed (calc)
    /// results, owned, with its module index.
    fn entry_at_hit(&self, (m, i): Hit) -> Option<(usize, Entry)> {
        let module = &self.modules[m];
        let ranked = self.ranked();
        let entry = ranked[m].entry(module, i).clone();
        Some((m, entry))
    }

    fn step_dir(&self, forward: bool) -> usize {
        let len = self.len();
        if len == 0 {
            return 0;
        }
        let current = self.current();
        if forward {
            (current + 1) % len
        } else {
            (current + len - 1) % len
        }
    }

    fn current(&self) -> usize {
        self.selected.min(self.len().saturating_sub(1))
    }

    fn select(&mut self, index: usize) {
        let len = self.len().saturating_sub(1);
        self.selected = index.min(len);
        self.armed = None;
        let visible = self.visible_rows();
        let top = self.offset;
        if self.selected < top {
            self.offset = self.selected;
        } else if self.selected >= top + visible {
            self.offset = self.selected + 1 - visible;
        }
    }

    fn remove_current(&mut self) {
        let Some((m, entry)) = self.entry_at(self.current()) else {
            return;
        };
        let module = &self.modules[m];
        let changed = match &entry.action {
            Action::Focus { window, .. } => match crate::windows::close(*window) {
                Ok(()) => {
                    module.hide(&entry);
                    true
                }
                Err(err) => {
                    eprintln!("launch: cannot close {:?}: {err}", entry.name);
                    false
                }
            },
            Action::Clipboard(text) => match crate::clipboard::delete(text) {
                Ok(()) => {
                    module.hide(&entry);
                    true
                }
                Err(err) => {
                    eprintln!("launch: cannot delete from the clipboard history: {err}");
                    false
                }
            },
            _ => module.forget(&entry),
        };
        if changed {
            self.query_rev += 1;
        }
    }

    /// Runs the entry's action. Recorded first; history already captured.
    fn run(&mut self, module_index: usize, entry: &Entry) -> Result<(), Box<dyn std::error::Error>> {
        self.modules[module_index].record(entry);
        let copy_command = self.config.copy_command.as_deref();
        // Headless runs never execute side effects: spawning apps or writing the clipboard
        // would escape the snapshot. The action logic itself is unit-tested with a recorder.
        let headless = self.host.headless();
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
                if headless {
                    Ok(())
                } else {
                    crate::entries::spawn(command, *terminal, self.config.terminal.as_deref())
                        .map_err(Into::into)
                }
            }
            Action::Focus { window, desktop } => {
                if headless {
                    Ok(())
                } else {
                    crate::windows::focus(*window, *desktop).map_err(Into::into)
                }
            }
            Action::Emoji(emoji) => {
                if headless {
                    Ok(())
                } else {
                    crate::emoji::deliver(
                        emoji,
                        self.config.emoji.action,
                        self.config.emoji.copy_command.as_deref().or(copy_command),
                    )
                    .map_err(Into::into)
                }
            }
            Action::Copy(text) | Action::Clipboard(text) => {
                if headless {
                    Ok(())
                } else {
                    crate::entries::copy(text, copy_command).map_err(Into::into)
                }
            }
        }
    }

    fn activate(&mut self, keep_open: bool) {
        let multi = self.dmenu.as_ref().is_some_and(|dmenu| dmenu.multi);
        if multi && keep_open {
            if let Some(hit) = self.hit_at(self.current()) {
                match self.marked.iter().position(|h| *h == hit) {
                    Some(i) => {
                        self.marked.remove(i);
                    }
                    None => self.marked.push(hit),
                }
                self.select(self.step_dir(true));
            }
            return;
        }
        if multi && !self.marked.is_empty() {
            self.print_marked();
            return;
        }

        let Some((m, entry)) = self.entry_at(self.current()) else {
            let custom = self
                .dmenu
                .as_ref()
                .is_some_and(|dmenu| !dmenu.no_custom && !dmenu.index);
            if custom && !self.query.text.is_empty() {
                self.host.print(&self.query.text);
                if !keep_open {
                    self.host.exit(0);
                }
            } else if !keep_open {
                self.host.exit(1);
            }
            return;
        };

        let hit = self.hit_at(self.current()).unwrap();
        if entry.confirm && self.armed != Some(hit) {
            self.armed = Some(hit);
            return;
        }
        let result = self.run(m, &entry);
        if let Err(err) = &result {
            eprintln!("launch: {:?} failed: {err}", entry.name);
        }
        if !keep_open || result.is_err() {
            self.host.exit(if result.is_ok() { 0 } else { 1 });
        }
    }

    /// Prints every marked line in input order and exits (dmenu multi-select).
    fn print_marked(&mut self) {
        let mut marked = self.marked.clone();
        marked.sort();
        for (m, i) in marked {
            if let Some(entry) = self.entry_at_hit((m, i)).map(|(_, entry)| entry) {
                let _ = self.run(m, &entry);
            }
        }
        self.host.exit(0);
    }

    fn on_key(&mut self, ev: &KeyEvent, _tx: &mut Tx) -> bool {
        let shift = ev.mods.shift;
        let ctrl = ev.mods.ctrl;
        let alt = ev.mods.alt;
        let tab_count = self.tabs.len();

        let handled = match &ev.key {
            Key::Escape => {
                self.host.exit(1);
                true
            }
            Key::Enter => {
                let keep_open = shift;
                self.activate(keep_open);
                true
            }
            Key::Delete if shift => {
                self.remove_current();
                true
            }
            Key::Command('d') => {
                self.remove_current();
                true
            }
            Key::Text(t) if ctrl && t == "d" => {
                self.remove_current();
                true
            }
            Key::Tab if tab_count > 1 => {
                self.active = if shift {
                    (self.active + tab_count - 1) % tab_count
                } else {
                    (self.active + 1) % tab_count
                };
                self.on_query_changed();
                true
            }
            Key::Tab => {
                self.select(self.step_dir(!shift));
                true
            }
            Key::Down => {
                self.select(self.step_dir(true));
                true
            }
            Key::Up => {
                self.select(self.step_dir(false));
                true
            }
            Key::PageDown => {
                let len = self.len().saturating_sub(1);
                self.select((self.current() + self.visible_rows()).min(len));
                true
            }
            Key::PageUp => {
                self.select(self.current().saturating_sub(self.visible_rows()));
                true
            }
            Key::Command('n') | Key::Command('j') => {
                self.select(self.step_dir(true));
                true
            }
            Key::Command('p') | Key::Command('k') => {
                self.select(self.step_dir(false));
                true
            }
            Key::Text(t) if alt => match t.parse::<usize>() {
                Ok(n @ 1..=9) if n <= tab_count => {
                    self.active = n - 1;
                    self.on_query_changed();
                    true
                }
                _ => false,
            },
            _ => false,
        };
        handled
    }

    /// The icon for `entry`, decoded once and cached, or None when it can't be shown.
    fn entry_icon(&self, entry: &Entry) -> Option<rune::Pixels> {
        const SIZE: u32 = 64;
        let mut pixels = self.pixels.borrow_mut();
        let mut decode = |key: String, png: Vec<u8>| -> Option<rune::Pixels> {
            if let Some(p) = pixels.get(&key) {
                return Some(p.clone());
            }
            let (w, h, rgba) = decode_png(&png)?;
            let p = rune::Pixels::new();
            p.set(w, h, rgba);
            pixels.insert(key, p.clone());
            Some(p)
        };
        match &entry.icon {
            None => None,
            // Glyphs render as text in the row, not as pixels.
            Some(EntryIcon::Glyph(_)) => None,
            Some(EntryIcon::Name(name)) => self.icons.resolve(name).and_then(|icon| match icon {
                crate::icons::Icon::Svg(_, png) => decode(format!("{name}@{SIZE}"), png),
                crate::icons::Icon::Raster(path) => {
                    decode(format!("{name}@{SIZE}"), std::fs::read(&path).ok()?)
                }
            }),
            Some(EntryIcon::Image(png)) => decode(format!("image-{}@{}", smhash(png), SIZE), png.clone()),
        }
    }
}

fn smhash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// PNG bytes → straight-alpha RGBA, rows top first (rune `Pixels::set`'s format).
fn decode_png(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let w = info.width;
    let h = info.height;
    buf.truncate(w as usize * h as usize * 4);
    Some((w, h, buf))
}

/// The rows the window shows: `visible_rows` starting at `offset`, plus the empty state.
fn rows_el(s: &State) -> El {
    let hits = s.tab_hits();
    let visible = s.visible_rows();
    let current = s.current();
    let len = hits.len();
    let all_tab = s.tabs[s.active] == Tab::All;

    if len == 0 {
        return empty_el(s);
    }

    let mut column = col().w_full().h(visible as f32 * s.config.row_height);
    for visual in 0..visible {
        let position = s.offset + visual;
        if position >= len {
            continue;
        }
        let on = position == current;
        let hit = hits[position];
        let (m, i) = hit;
        let module = &s.modules[m];
        let ranked = s.ranked();
        let entry: &Entry = ranked[m].entry(module, i);
        column = column.child(row_el(
            s,
            module,
            entry,
            on,
            s.armed == Some(hit),
            s.marked.contains(&hit),
            all_tab,
            position,
        ));
    }
    column
}

fn row_el(
    s: &State,
    module: &Module,
    entry: &Entry,
    on: bool,
    _armed: bool,
    marked: bool,
    all_tab: bool,
    position: usize,
) -> El {
    let config = &s.config;
    let colors = &s.config.colors;
    let icon = if config.show_icons {
        s.entry_icon(entry)
    } else {
        None
    };
    let name_color = if on { colors.text } else { colors.text };
    let comment = if config.show_comments {
        entry.comment.as_deref()
    } else {
        None
    };
    let mut row = row()
        .key(format!("result-{position}"))
        .w_full()
        .h(config.row_height)
        .px(8.0)
        .gap(10.0)
        .center_y()
        .radius(6.0)
        .fill(if on { colors.selected } else { Color::TRANSPARENT })
        .on_press(move |s: &mut State| s.select(position));

    match &entry.icon {
        Some(EntryIcon::Glyph(glyph)) => {
            row = row.child(
                el()
                    .size(config.icon_size, config.icon_size)
                    .center()
                    .child(text(glyph).font(type_::BODY)),
            );
        }
        _ => {
            if let Some(pixels) = &icon {
                row = row.child(el().size(config.icon_size, config.icon_size).pixels(pixels));
            }
        }
    }
    if all_tab {
        row = row.child(
            el()
                .h(18.0)
                .px(6.0)
                .center_y()
                .radius(5.0)
                .fill(colors.input)
                .child(
                    text(module.title)
                        .font(type_::LABEL)
                        .color(colors.muted),
                ),
        );
    }
    let mut names = col().grow(1.0).center_y().gap(1.0);
    names = names
        .child(text(&entry.name).font(type_::BODY).color(name_color).truncate());
    if let Some(comment) = comment {
        names = names.child(
            text(comment)
                .font(type_::LABEL)
                .color(colors.muted)
                .truncate(),
        );
    }
    row = row.child(names);
    row = row.child(spacer());
    if marked {
        row = row.child(text("✓").font(type_::BODY).color(colors.accent));
    }
    row
}

fn empty_el(s: &State) -> El {
    let colors = &s.config.colors;
    let text_value = match s.tabs[s.active] {
        Tab::Module(m) if s.modules[m].compute.is_some() => {
            "Type a calculation, like 2^10 / 3".to_string()
        }
        Tab::Module(m) if s.modules[m].entries().is_empty() => s.modules[m].empty.to_string(),
        Tab::Module(_) | Tab::All if s.tab_hits().is_empty() => "No matches".to_string(),
        _ => "Nothing here".to_string(),
    };
    col()
        .w_full()
        .h(s.visible_rows() as f32 * s.config.row_height)
        .center()
        .child(text(&text_value).font(type_::BODY).color(colors.muted))
}

fn input_el(s: &State) -> El {
    let colors = &s.config.colors;
    let placeholder = s
        .config
        .placeholder
        .clone()
        .unwrap_or_else(|| match s.tabs[s.active] {
            Tab::All => "Search apps, windows, emoji…".to_string(),
            Tab::Module(m) => s.modules[m].placeholder.to_string(),
        });
    let field = text_input(&s.query, type_::BODY)
        .key("query")
        .single_line()
        .grow(1.0)
        .color(colors.text)
        .placeholder(&placeholder, colors.muted)
        .on_edit(|s: &mut State| &mut s.query)
        .on_input_change(|s: &mut State, _| s.on_query_changed())
        .on_input_key(|s: &mut State, ev: &KeyEvent, tx: &mut Tx| s.on_key(ev, tx));

    let prompt = s.dmenu.as_ref().and_then(|dmenu| dmenu.prompt.clone());
    let mut row = row()
        .key("input-row")
        .w_full()
        .h(s.config.input_height)
        .px(12.0)
        .gap(10.0)
        .center_y()
        .radius(s.config.corner_radius)
        .fill(colors.input);
    if let Some(prompt) = prompt {
        row = row.child(text(&prompt).font(type_::BODY).color(colors.accent));
    }
    row.child(field)
}

fn tabs_el(s: &State) -> El {
    let colors = &s.config.colors;
    let has_query = s.has_query();
    let counts = {
        let ranked = s.ranked();
        s.tabs
            .iter()
            .map(|tab| match tab {
                Tab::All => merge(&s.modules, &ranked, has_query).len(),
                Tab::Module(m) => ranked[*m].hits.len(),
            })
            .collect::<Vec<_>>()
    };
    let mut tabs = row().key("tabs").w_full().h(crate::config::TAB_HEIGHT).gap(6.0);
    for (i, tab) in s.tabs.iter().enumerate() {
        let on = i == s.active;
        let title = match tab {
            Tab::All => "All",
            Tab::Module(m) => s.modules[*m].title,
        };
        let mut tab = row()
            .key(format!("tab-{i}"))
            .h(crate::config::TAB_HEIGHT)
            .px(12.0)
            .center_y()
            .gap(6.0)
            .radius(6.0)
            .fill(if on { colors.selected } else { Color::TRANSPARENT })
            .child(text(title).font(type_::BODY).color(colors.text))
            .on_press(move |s: &mut State| {
                s.active = i;
                s.on_query_changed();
            });
        if has_query {
            tab = tab.child(
                el()
                    .h(16.0)
                    .px(6.0)
                    .center_y()
                    .radius(8.0)
                    .fill(colors.input)
                    .child(text(&counts[i].to_string()).font(type_::LABEL).color(colors.muted)),
            );
        }
        tabs = tabs.child(tab);
    }
    tabs
}

pub fn view(s: &State, _: &mut rune::Cx<State>) -> El {
    let colors = &s.config.colors;
    // Frosted glass: the captured desktop is drawn under the panel, which blurs what it covers
    // and tints it at `blur_opacity`; the transparent window keeps the desktop sharp outside
    // the panel. `blur = 0` (or no capture, as in headless runs) draws the plain background.
    let glass = s.backdrop.is_some() && s.config.blur > 0.;
    let mut column = col()
        .key("launcher")
        .w_full()
        .h_full()
        .padding(s.config.padding)
        .gap(s.config.padding);
    if glass {
        let mut tint = colors.tint.unwrap_or(colors.background);
        tint.a = s.config.blur_opacity.clamp(0., 1.);
        column = column.backdrop_blur(s.config.blur).fill(tint);
    } else {
        column = column.fill(colors.background);
    }
    if s.rounded {
        column = column.radius(s.config.corner_radius);
    }
    if s.tabs.len() > 1 {
        column = column.child(tabs_el(s));
    }
    column = column.child(input_el(s));
    column = column.child(rows_el(s));
    match &s.backdrop {
        Some(backdrop) if glass => col()
            .key("glass")
            .w_full()
            .h_full()
            .child(
                el()
                    .key("backdrop")
                    .w_full()
                    .h_full()
                    .pixels_fit(backdrop, rune::SurfaceFit::Fill),
            )
            // Out of flow so it overlays the backdrop instead of stacking beside it.
            .child(column.absolute(0.0, 0.0)),
        _ => column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::ModuleKind;
    use rune::{Env, Key, KeyEvent, Mods, Runtime};

    fn dmenu_state(lines: &str, dmenu: DmenuOptions) -> State {
        let config = Config::default();
        let icons = IconResolver::new(None, 64);
        let mut state = State::new(
            vec![Module::dmenu(lines)],
            vec![Tab::Module(0)],
            0,
            config,
            icons,
            Some(dmenu),
            false,
            None,
        );
        state.host = Host::fake();
        state
    }

    fn runner(lines: &str, dmenu: DmenuOptions) -> Runtime<State> {
        let mut rt = Runtime::new(
            dmenu_state(lines, dmenu),
            view,
            (680.0, 500.0),
            Env { system_dark: true, scale: 1.0 },
        );
        rt.window_focus(true);
        rt.frame(0.0);
        rt
    }

    fn key(rt: &mut Runtime<State>, key: Key, mods: Mods) {
        rt.key(KeyEvent { key, mods });
        rt.frame(1.0 / 60.0);
    }

    fn type_text(rt: &mut Runtime<State>, text: &str) {
        for c in text.chars() {
            key(rt, Key::Text(c.to_string()), Mods::default());
        }
    }

    fn enter(rt: &mut Runtime<State>, mods: Mods) {
        key(rt, Key::Enter, mods);
    }

    fn printed(rt: &Runtime<State>) -> Vec<String> {
        rt.state.host.printed()
    }

    fn exit_code(rt: &Runtime<State>) -> Option<i32> {
        rt.state.host.exit_code()
    }

    fn first_hit_name(rt: &Runtime<State>) -> String {
        rt.state
            .entry_at(0)
            .map(|(_, entry)| entry.name.clone())
            .unwrap_or_default()
    }

    #[test]
    fn types_and_filters() {
        let mut rt = runner("apple\nbanana\ncherry", DmenuOptions::default());
        type_text(&mut rt, "ban");
        // Only "banana" contains b+a+n: the hit list narrows to one entry.
        assert_eq!(first_hit_name(&rt), "banana");
        assert_eq!(rt.state.len(), 1);
        // Out-of-range positions clamp to the last hit, matching launch.
        assert_eq!(
            rt.state.entry_at(1).map(|(_, e)| e.name),
            Some("banana".to_string())
        );
    }

    #[test]
    fn dmenu_prints_the_filtered_line() {
        let mut rt = runner("apple\nbanana\ncherry", DmenuOptions::default());
        type_text(&mut rt, "ban");
        enter(&mut rt, Mods::default());
        assert_eq!(printed(&rt), ["banana"]);
        assert_eq!(exit_code(&rt), Some(0));
    }

    #[test]
    fn dmenu_prints_index_and_custom_text() {
        let mut rt = runner("apple\nbanana", DmenuOptions { index: true, ..DmenuOptions::default() });
        key(&mut rt, Key::Down, Mods::default());
        enter(&mut rt, Mods::default());
        assert_eq!(printed(&rt), ["1"]);
        assert_eq!(exit_code(&rt), Some(0));

        // Custom text: with --index, no match exits 1 with nothing printed.
        let mut rt = runner("apple", DmenuOptions { index: true, ..DmenuOptions::default() });
        type_text(&mut rt, "zzz");
        enter(&mut rt, Mods::default());
        assert!(printed(&rt).is_empty());
        assert_eq!(exit_code(&rt), Some(1));

        // Without --index, the query itself is printed.
        let mut rt = runner("apple", DmenuOptions::default());
        type_text(&mut rt, "zzz");
        enter(&mut rt, Mods::default());
        assert_eq!(printed(&rt), ["zzz"]);
        assert_eq!(exit_code(&rt), Some(0));
    }

    #[test]
    fn dmenu_multi_select_prints_marked_lines_in_order() {
        let options = DmenuOptions { multi: true, ..DmenuOptions::default() };
        let mut rt = runner("apple\nbanana\ncherry", options);
        // Shift+Enter marks the selection and advances it, so with an empty query three
        // presses mark all lines and leave the list order intact.
        for _ in 0..3 {
            key(&mut rt, Key::Enter, Mods { shift: true, ..Mods::default() });
        }
        enter(&mut rt, Mods::default());
        assert_eq!(printed(&rt), ["apple", "banana", "cherry"]);
        assert_eq!(exit_code(&rt), Some(0));
    }

    #[test]
    fn keep_open_prints_without_exiting() {
        let mut rt = runner("a\nb", DmenuOptions::default());
        type_text(&mut rt, "b");
        key(&mut rt, Key::Enter, Mods { shift: true, ..Mods::default() });
        assert_eq!(printed(&rt), ["b"]);
        assert_eq!(exit_code(&rt), None);
    }

    #[test]
    fn escape_exits_one() {
        let mut rt = runner("a\nb", DmenuOptions::default());
        key(&mut rt, Key::Escape, Mods::default());
        assert_eq!(exit_code(&rt), Some(1));
    }

    #[test]
    fn confirm_arms_on_first_enter() {
        let config = Config::default();
        let icons = IconResolver::new(None, 64);
        let entry = Entry {
            id: "destructive".to_string(),
            name: "Destroy".to_string(),
            comment: None,
            icon: None,
            keywords: String::new(),
            action: Action::Print(7),
            confirm: true,
            expires: None,
        };
        let mut state = State::new(
            vec![Module::new("test", "", vec![entry])],
            vec![Tab::Module(0)],
            0,
            config,
            icons,
            None,
            false,
            None,
        );
        state.host = Host::fake();
        let mut rt = Runtime::new(
            state,
            view,
            (680.0, 500.0),
            Env { system_dark: true, scale: 1.0 },
        );
        rt.window_focus(true);
        rt.frame(0.0);

        enter(&mut rt, Mods::default());
        assert!(printed(&rt).is_empty(), "first Enter only arms");
        assert_eq!(exit_code(&rt), None);

        enter(&mut rt, Mods::default());
        // The armed run prints — outside dmenu mode a Print action prints the entry name.
        assert_eq!(printed(&rt), ["Destroy"]);
        assert_eq!(exit_code(&rt), Some(0));
    }

    #[test]
    fn calc_result_shows_and_ranks() {
        let config = Config::default();
        let icons = IconResolver::new(None, 64);
        let module = Module::load(ModuleKind::Calc, 64, &config);
        let mut state = State::new(
            vec![module],
            vec![Tab::Module(0)],
            0,
            config,
            icons,
            None,
            false,
            None,
        );
        state.host = Host::fake();
        let mut rt = Runtime::new(
            state,
            view,
            (680.0, 500.0),
            Env { system_dark: true, scale: 1.0 },
        );
        rt.window_focus(true);
        rt.frame(0.0);
        type_text(&mut rt, "6*7");
        assert_eq!(first_hit_name(&rt), "42");
    }

    #[test]
    fn tab_keys_switches_tabs() {
        let config = Config::default();
        let icons = IconResolver::new(None, 64);
        let mut state = State::new(
            vec![
                Module::new("One", "", vec![]),
                Module::new("Two", "", vec![]),
            ],
            vec![Tab::Module(0), Tab::Module(1)],
            0,
            config,
            icons,
            None,
            false,
            None,
        );
        state.host = Host::fake();
        let mut rt = Runtime::new(
            state,
            view,
            (680.0, 500.0),
            Env { system_dark: true, scale: 1.0 },
        );
        rt.window_focus(true);
        rt.frame(0.0);
        assert_eq!(rt.state.active, 0);

        // Tab cycles forward, Alt+N jumps to a tab.
        key(&mut rt, Key::Tab, Mods::default());
        assert_eq!(rt.state.active, 1);
        key(
            &mut rt,
            Key::Text("1".to_string()),
            Mods { alt: true, ..Mods::default() },
        );
        assert_eq!(rt.state.active, 0);
        key(
            &mut rt,
            Key::Text("2".to_string()),
            Mods { alt: true, ..Mods::default() },
        );
        assert_eq!(rt.state.active, 1);
    }

    #[test]
    fn arrow_keys_move_the_selection() {
        fn selected_name(rt: &Runtime<State>) -> String {
            rt.state
                .entry_at(rt.state.selected)
                .map(|(_, e)| e.name)
                .unwrap_or_default()
        }
        let mut rt = runner("apple\nbanana\ncherry", DmenuOptions::default());
        type_text(&mut rt, "a");
        assert_eq!(selected_name(&rt), "apple");
        key(&mut rt, Key::Down, Mods::default());
        assert_eq!(selected_name(&rt), "banana");
        key(&mut rt, Key::Up, Mods::default());
        assert_eq!(selected_name(&rt), "apple");
    }
}
