use std::cell::RefCell;

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde::Deserialize;

use crate::{
    emoji,
    entries::{self, Entry, History},
    windows,
};

/// The modules that can be listed in the config's `modules` array.
#[derive(Deserialize, clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    Apps,
    Windows,
    Emoji,
}

/// One tab of the launcher: a list of entries and how to rank them.
pub struct Module {
    pub title: &'static str,
    pub placeholder: &'static str,
    pub entries: Vec<Entry>,
    /// Usage history; modules without one keep their natural order (e.g. window stacking).
    pub history: Option<RefCell<History>>,
    /// Whether the whole list shows up in the "All" tab before anything is typed. Large
    /// modules (emoji) only contribute recently used entries there.
    pub browse_in_all: bool,
}

impl Module {
    pub fn load(kind: ModuleKind, icon_size: u32) -> Self {
        match kind {
            ModuleKind::Apps => Self {
                title: "Apps",
                placeholder: "Search applications…",
                entries: entries::desktop_entries(),
                history: Some(RefCell::new(History::load("apps"))),
                browse_in_all: true,
            },
            ModuleKind::Windows => Self {
                title: "Windows",
                placeholder: "Switch to window…",
                entries: windows::window_entries(icon_size),
                history: None,
                browse_in_all: true,
            },
            ModuleKind::Emoji => Self {
                title: "Emoji",
                placeholder: "Search emoji…",
                entries: emoji::emoji_entries(),
                history: Some(RefCell::new(History::load("emoji"))),
                browse_in_all: false,
            },
        }
    }

    pub fn dmenu(input: &str) -> Self {
        Self {
            title: "dmenu",
            placeholder: "Filter…",
            entries: entries::dmenu_entries(input),
            history: Some(RefCell::new(History::load("dmenu"))),
            browse_in_all: true,
        }
    }

    pub fn record(&self, entry: &Entry) {
        if let Some(history) = &self.history {
            history.borrow_mut().record(&entry.id);
        }
    }

    /// `(strong, index into entries)`, best match first. A match is strong when every query
    /// word appears as-is in the name or keywords, rather than as scattered letters. Without a
    /// query everything counts as strong.
    pub fn rank(&self, query: &str) -> Vec<(bool, usize)> {
        let history = self.history.as_ref().map(RefCell::borrow);
        let used = |entry: &Entry| {
            history
                .as_ref()
                .map(|history| history.get(&entry.id))
                .unwrap_or_default()
        };

        // Without a query: most recently used first, then the rest in natural order
        // (alphabetical for modules with history).
        if query.trim().is_empty() {
            let mut indices: Vec<usize> = (0..self.entries.len()).collect();
            if history.is_some() {
                indices.sort_by_cached_key(|&i| {
                    let entry = &self.entries[i];
                    let used = used(entry);
                    (
                        std::cmp::Reverse(used.last),
                        std::cmp::Reverse(used.count),
                        entry.name.to_lowercase(),
                    )
                });
            }
            return indices.into_iter().map(|i| (true, i)).collect();
        }

        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();

        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let mut buf = Vec::new();
        let now = entries::now();
        let mut score = |text: &str| pattern.score(Utf32Str::new(text, &mut buf), &mut matcher);

        let mut scored: Vec<(bool, u32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let name = score(&entry.name);
                // Keywords only count for half so a name match always wins.
                let keywords = score(&entry.keywords).map(|score| score / 2);
                let best = name.max(keywords)?;
                // Recently and frequently used entries get a small boost.
                let used = used(entry);
                let age = now.saturating_sub(used.last);
                let recency = match age {
                    _ if used.last == 0 => 0,
                    0..3_600 => 24,
                    3_600..86_400 => 16,
                    86_400..604_800 => 8,
                    _ => 0,
                };
                // Fuzzy scores ignore trailing characters, so "fire" would tie with "Firefox".
                let exact = if entry.name.eq_ignore_ascii_case(query.trim()) {
                    64
                } else {
                    0
                };
                let boost = recency + used.count.min(20) * 2 + exact;
                let name_lower = entry.name.to_lowercase();
                let keywords_lower = entry.keywords.to_lowercase();
                let strong = words
                    .iter()
                    .all(|word| name_lower.contains(word) || keywords_lower.contains(word));
                Some((strong, best + boost, i))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        scored
            .into_iter()
            .map(|(strong, _, i)| (strong, i))
            .collect()
    }

    /// Whether `entry` was ever used, for modules that only show recent entries in "All".
    pub fn was_used(&self, entry: &Entry) -> bool {
        self.history
            .as_ref()
            .is_some_and(|history| history.borrow().get(&entry.id).last > 0)
    }
}

/// A tab in the launcher: every module merged, or a single module.
#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    All,
    Module(usize),
}

/// A result row: `(module index, entry index)`.
pub type Hit = (usize, usize);

/// Merges the per-module rankings for the "All" tab: strong matches before fuzzy ones, and
/// within each tier modules keep their configured order (apps, windows, emoji by default).
pub fn merge(modules: &[Module], ranked: &[Vec<(bool, usize)>], has_query: bool) -> Vec<Hit> {
    let mut hits: Vec<(bool, Hit)> = ranked
        .iter()
        .enumerate()
        .flat_map(|(m, hits)| {
            let module = &modules[m];
            hits.iter()
                .filter(move |(_, i)| {
                    has_query || module.browse_in_all || module.was_used(&module.entries[*i])
                })
                .map(move |&(strong, i)| (strong, (m, i)))
        })
        .collect();
    // Stable, so module order and per-module rank are kept within each tier.
    hits.sort_by_key(|(strong, _)| !strong);
    hits.into_iter().map(|(_, hit)| hit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(names: &[&str], browse_in_all: bool) -> Module {
        Module {
            title: "test",
            placeholder: "",
            entries: entries::dmenu_entries(&names.join("\n")),
            history: None,
            browse_in_all,
        }
    }

    #[test]
    fn merges_in_module_order() {
        let apps = module(&["LibreOffice Writer", "Firefox"], true);
        let windows = module(&["x", "Inbox — Mozilla Firefox"], true);
        let emoji = module(&["Fire", "y"], false);
        let modules = [apps, windows, emoji];
        let ranked: Vec<_> = modules.iter().map(|module| module.rank("fire")).collect();
        // Substring matches in module order (the exact emoji match still comes after the app
        // and the window), then the scattered-letter match in "LibreOffice Writer".
        assert_eq!(
            merge(&modules, &ranked, true),
            vec![(0, 1), (1, 1), (2, 0), (0, 0)]
        );
    }

    #[test]
    fn hides_unused_large_modules_without_query() {
        let modules = [module(&["a", "b"], true), module(&["c"], false)];
        let ranked: Vec<_> = modules.iter().map(|module| module.rank("")).collect();
        assert_eq!(merge(&modules, &ranked, false), vec![(0, 0), (0, 1)]);
    }
}
