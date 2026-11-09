use std::{cell::RefCell, collections::HashSet};

use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde::Deserialize;

use crate::{
    calc,
    config::Config,
    emoji,
    entries::{self, Entry, History},
    power, windows,
};

/// The modules that can be listed in the config's `modules` array.
#[derive(Deserialize, clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    Calc,
    Apps,
    Windows,
    Emoji,
    Power,
}

/// One tab of the launcher: a list of entries and how to rank them.
pub struct Module {
    pub title: &'static str,
    pub placeholder: &'static str,
    pub entries: Vec<Entry>,
    /// Builds the entries from the query instead of filtering `entries` (calculator).
    pub compute: Option<fn(&str) -> Vec<Entry>>,
    /// Usage history; modules without one keep their natural order (e.g. window stacking).
    pub history: Option<RefCell<History>>,
    /// Whether the whole list shows up in the "All" tab before anything is typed. Large
    /// modules (emoji) only contribute recently used entries there.
    pub browse_in_all: bool,
    /// Ids removed during this session, such as closed windows.
    pub hidden: RefCell<HashSet<String>>,
}

impl Module {
    pub fn new(title: &'static str, placeholder: &'static str, entries: Vec<Entry>) -> Self {
        Self {
            title,
            placeholder,
            entries,
            compute: None,
            history: None,
            browse_in_all: true,
            hidden: RefCell::default(),
        }
    }

    fn with_history(mut self, namespace: &str) -> Self {
        self.history = Some(RefCell::new(History::load(namespace)));
        self
    }

    pub fn load(kind: ModuleKind, icon_size: u32, config: &Config) -> Self {
        match kind {
            ModuleKind::Calc => Self {
                compute: Some(calc::entries),
                ..Self::new("Calc", "Calculate…", Vec::new())
            },
            ModuleKind::Apps => {
                Self::new("Apps", "Search applications…", entries::desktop_entries())
                    .with_history("apps")
            }
            ModuleKind::Windows => Self::new(
                "Windows",
                "Switch to window…",
                windows::window_entries(icon_size),
            ),
            ModuleKind::Emoji => Self {
                browse_in_all: false,
                ..Self::new("Emoji", "Search emoji…", emoji::emoji_entries())
            }
            .with_history("emoji"),
            ModuleKind::Power => Self {
                browse_in_all: false,
                ..Self::new(
                    "Power",
                    "Lock, suspend, reboot…",
                    power::power_entries(&config.power),
                )
            }
            .with_history("power"),
        }
    }

    pub fn dmenu(input: &str) -> Self {
        Self::new("dmenu", "Filter…", entries::dmenu_entries(input)).with_history("dmenu")
    }

    pub fn record(&self, entry: &Entry) {
        if let Some(history) = &self.history {
            history.borrow_mut().record(&entry.id);
        }
    }

    /// Drops the entry from the usage history. Returns whether there was a history to change.
    pub fn forget(&self, entry: &Entry) -> bool {
        let Some(history) = &self.history else {
            return false;
        };
        history.borrow_mut().forget(&entry.id);
        true
    }

    pub fn hide(&self, entry: &Entry) {
        self.hidden.borrow_mut().insert(entry.id.clone());
    }

    /// Best match first. A match is strong when every query word appears as-is in the name or
    /// keywords, rather than as scattered letters. Without a query everything counts as strong.
    pub fn rank(&self, query: &str) -> Ranked {
        if let Some(compute) = self.compute {
            let computed = compute(query);
            let base = self.entries.len();
            return Ranked {
                hits: (base..base + computed.len()).map(|i| (true, i)).collect(),
                computed,
            };
        }
        Ranked {
            hits: self.rank_entries(query),
            computed: Vec::new(),
        }
    }

    fn rank_entries(&self, query: &str) -> Vec<(bool, usize)> {
        let hidden = self.hidden.borrow();
        let visible = |entry: &Entry| !hidden.contains(&entry.id);
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
            let mut indices: Vec<usize> = (0..self.entries.len())
                .filter(|&i| visible(&self.entries[i]))
                .collect();
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
            .filter(|(_, entry)| visible(entry))
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

/// A module's results for one query.
#[derive(PartialEq)]
pub struct Ranked {
    /// `(strong, entry index)`, best first. Indices past the module's own entries point into
    /// `computed`.
    pub hits: Vec<(bool, usize)>,
    pub computed: Vec<Entry>,
}

impl Ranked {
    pub fn entry<'a>(&'a self, module: &'a Module, index: usize) -> &'a Entry {
        module
            .entries
            .get(index)
            .unwrap_or_else(|| &self.computed[index - module.entries.len()])
    }
}

/// Merges the per-module rankings for the "All" tab: strong matches before fuzzy ones, and
/// within each tier modules keep their configured order (apps, windows, emoji by default).
pub fn merge(modules: &[Module], ranked: &[Ranked], has_query: bool) -> Vec<Hit> {
    let mut hits: Vec<(bool, Hit)> = ranked
        .iter()
        .enumerate()
        .flat_map(|(m, ranked)| {
            let module = &modules[m];
            ranked
                .hits
                .iter()
                .filter(move |(_, i)| {
                    has_query || module.browse_in_all || module.was_used(ranked.entry(module, *i))
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
            browse_in_all,
            ..Module::new("test", "", entries::dmenu_entries(&names.join("\n")))
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

    #[test]
    fn hidden_entries_are_not_ranked() {
        let module = module(&["a", "b"], true);
        module.hide(&module.entries[0]);
        assert_eq!(module.rank("").hits, vec![(true, 1)]);
        assert!(module.rank("a").hits.is_empty());
    }

    #[test]
    fn computed_entries_follow_the_static_ones() {
        let calc = Module {
            compute: Some(calc::entries),
            ..Module::new("Calc", "", Vec::new())
        };
        let ranked = calc.rank("1+1");
        assert_eq!(ranked.hits, vec![(true, 0)]);
        assert_eq!(ranked.entry(&calc, 0).name, "2");
    }
}
