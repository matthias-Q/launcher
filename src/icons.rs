use std::{
    cell::{OnceCell, RefCell},
    collections::{HashMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

use freya::prelude::Bytes;

use crate::entries::data_dirs;

/// A resolved icon, ready to hand to an image viewer.
#[derive(Clone)]
pub enum Icon {
    /// SVG rasterized to PNG with resvg. Skia's SVG support chokes on many real-world icons
    /// (compact transform syntax, `xlink:href` gradients, missing `viewBox`).
    Svg(PathBuf, Bytes),
    Raster(PathBuf),
}

/// Resolves freedesktop `Icon=` values to files, following the icon theme spec loosely:
/// the configured (or GTK) theme, the themes it inherits, `hicolor`, then `pixmaps`.
pub struct IconResolver {
    /// Candidate directories, best first.
    dirs: Vec<PathBuf>,
    /// Directories of every other installed theme, built on the first miss. Not in the spec,
    /// but apps often only ship icons for one particular theme.
    fallback_dirs: OnceCell<Vec<PathBuf>>,
    bases: Vec<PathBuf>,
    themes: Vec<String>,
    size: u32,
    cache: RefCell<HashMap<String, Option<Icon>>>,
}

const EXTENSIONS: [&str; 3] = ["svg", "svgz", "png"];

impl IconResolver {
    /// `size` is the desired icon size in physical pixels.
    pub fn new(theme: Option<&str>, size: u32) -> Self {
        let bases = base_dirs();
        let theme = theme.map(str::to_string).or_else(gtk_icon_theme);

        let mut themes = Vec::new();
        let mut seen = HashSet::new();
        if let Some(theme) = theme {
            collect_themes(&bases, &theme, &mut themes, &mut seen);
        }
        collect_themes(&bases, "hicolor", &mut themes, &mut seen);

        let mut dirs: Vec<PathBuf> = themes
            .iter()
            .flat_map(|theme| theme_dirs(&bases, theme, size))
            .collect();
        dirs.extend(bases.iter().map(|base| base.with_file_name("pixmaps")));
        dirs.push(PathBuf::from("/usr/share/pixmaps"));

        Self {
            dirs,
            fallback_dirs: OnceCell::new(),
            bases,
            themes,
            size,
            cache: RefCell::default(),
        }
    }

    pub fn resolve(&self, icon: &str) -> Option<Icon> {
        if let Some(cached) = self.cache.borrow().get(icon) {
            return cached.clone();
        }

        let path = if icon.starts_with('/') {
            Some(PathBuf::from(icon)).filter(|path| path.is_file())
        } else {
            let name = icon
                .strip_suffix(".png")
                .or_else(|| icon.strip_suffix(".svg"))
                .or_else(|| icon.strip_suffix(".svgz"))
                .unwrap_or(icon);
            let find = |dirs: &[PathBuf]| {
                dirs.iter().find_map(|dir| {
                    EXTENSIONS
                        .iter()
                        .map(|ext| dir.join(format!("{name}.{ext}")))
                        .find(|path| path.is_file())
                })
            };
            find(&self.dirs).or_else(|| find(self.fallback_dirs()))
        };

        let icon_data = path.and_then(|path| {
            if path
                .extension()
                .is_some_and(|ext| ext == "svg" || ext == "svgz")
            {
                let png = rasterize_svg(&path, self.size)?;
                Some(Icon::Svg(path, Bytes::from(png)))
            } else {
                Some(Icon::Raster(path))
            }
        });

        self.cache
            .borrow_mut()
            .insert(icon.to_string(), icon_data.clone());
        icon_data
    }

    fn fallback_dirs(&self) -> &[PathBuf] {
        self.fallback_dirs.get_or_init(|| {
            let mut others: Vec<String> = self
                .bases
                .iter()
                .filter_map(|base| fs::read_dir(base).ok())
                .flat_map(|dir| dir.flatten())
                .filter(|entry| entry.path().join("index.theme").is_file())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|theme| !self.themes.contains(theme))
                .collect();
            others.sort();
            others.dedup();
            others
                .iter()
                .flat_map(|theme| theme_dirs(&self.bases, theme, self.size))
                .collect()
        })
    }
}

/// Renders the SVG into a `size`×`size` PNG, preserving its aspect ratio.
fn rasterize_svg(path: &Path, size: u32) -> Option<Vec<u8>> {
    let data = fs::read(path).ok()?;
    let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
    let svg_size = tree.size();
    let scale = size as f32 / svg_size.width().max(svg_size.height());
    let offset_x = (size as f32 - svg_size.width() * scale) / 2.;
    let offset_y = (size as f32 - svg_size.height() * scale) / 2.;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let transform =
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(offset_x, offset_y);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().ok()
}

/// `icons` directories in lookup order.
fn base_dirs() -> Vec<PathBuf> {
    env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".icons"))
        .into_iter()
        .chain(data_dirs().into_iter().map(|dir| dir.join("icons")))
        .filter(|dir| dir.is_dir())
        .collect()
}

fn gtk_icon_theme() -> Option<String> {
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    ["gtk-4.0", "gtk-3.0"].iter().find_map(|gtk| {
        let content = fs::read_to_string(config.join(gtk).join("settings.ini")).ok()?;
        content.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "gtk-icon-theme-name")
                .then(|| value.trim().trim_matches('"').to_string())
        })
    })
}

fn find_index(bases: &[PathBuf], theme: &str) -> Option<String> {
    bases
        .iter()
        .find_map(|base| fs::read_to_string(base.join(theme).join("index.theme")).ok())
}

/// Adds `theme` and everything it inherits, depth first.
fn collect_themes(
    bases: &[PathBuf],
    theme: &str,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(theme.to_string()) {
        return;
    }
    let Some(index) = find_index(bases, theme) else {
        return;
    };
    out.push(theme.to_string());
    let inherits = ini_value(&index, "Icon Theme", "Inherits").unwrap_or_default();
    for parent in inherits.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        collect_themes(bases, parent, out, seen);
    }
}

/// The theme's directories across all bases, ordered by how close they are to `size`.
fn theme_dirs(bases: &[PathBuf], theme: &str, size: u32) -> Vec<PathBuf> {
    let Some(index) = find_index(bases, theme) else {
        return Vec::new();
    };
    let directories = ini_value(&index, "Icon Theme", "Directories").unwrap_or_default();

    let mut ranked: Vec<(u32, &str)> = directories
        .split(',')
        .map(str::trim)
        .filter(|dir| !dir.is_empty())
        .map(|dir| {
            let section = |key| ini_value(&index, dir, key);
            let dir_size: u32 = section("Size").and_then(|s| s.parse().ok()).unwrap_or(0);
            let scale: u32 = section("Scale").and_then(|s| s.parse().ok()).unwrap_or(1);
            let effective = dir_size * scale;
            let distance = if section("Type").as_deref() == Some("Scalable") {
                0
            } else if effective >= size {
                effective - size
            } else {
                // Upscaling looks worse than downscaling.
                (size - effective) * 2
            };
            (distance, dir)
        })
        .collect();
    ranked.sort_by_key(|(distance, _)| *distance);

    ranked
        .into_iter()
        .flat_map(|(_, dir)| bases.iter().map(move |base| base.join(theme).join(dir)))
        .filter(|dir| dir.is_dir())
        .collect()
}

fn ini_value(content: &str, section: &str, key: &str) -> Option<String> {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == header;
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ini_sections() {
        let index =
            "[Icon Theme]\nInherits=hicolor\nDirectories=48x48/apps\n\n[48x48/apps]\nSize=48\n";
        assert_eq!(
            ini_value(index, "Icon Theme", "Inherits").as_deref(),
            Some("hicolor")
        );
        assert_eq!(
            ini_value(index, "48x48/apps", "Size").as_deref(),
            Some("48")
        );
        assert_eq!(ini_value(index, "48x48/apps", "Inherits"), None);
    }
}
