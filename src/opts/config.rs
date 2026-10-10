use std::fs::{create_dir_all, read_to_string};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{Position, Size, ThemeType};
use crate::color;
use crate::keybindings::Keybindings;

use anyhow::Context;
use serde::Deserialize;
use syntect::highlighting::Theme as SyntectTheme;

#[derive(Deserialize, Debug, PartialEq, Eq, Default, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct FontOptions {
    #[serde(default)]
    pub regular_font: Option<String>,
    #[serde(default)]
    pub monospace_font: Option<String>,
}

#[derive(Deserialize, Debug, Default, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct OptionalTheme {
    pub text_color: Option<u32>,
    pub background_color: Option<u32>,
    pub code_color: Option<u32>,
    pub quote_block_color: Option<u32>,
    pub link_color: Option<u32>,
    pub select_color: Option<u32>,
    pub checkbox_color: Option<u32>,
    pub header_color: Option<u32>,
    pub scrollbar_width: Option<u32>,
    pub scrollbar_color: Option<u32>,
    pub page_margin: Option<u32>,
    /// Omit to let the status bar follow `background-color`.
    pub status_bar_color: Option<u32>,
    /// Search field border when the query has matches / has none.
    pub search_match_color: Option<u32>,
    pub search_no_match_color: Option<u32>,
    pub code_highlighter: Option<color::SyntaxTheme>,
}

impl OptionalTheme {
    pub fn merge(self, other: color::Theme) -> anyhow::Result<color::Theme> {
        let code_highlighter = match self.code_highlighter {
            Some(theme) => SyntectTheme::try_from(theme)?,
            None => other.code_highlighter,
        };

        Ok(color::Theme {
            text_color: self.text_color.unwrap_or(other.text_color),
            background_color: self.background_color.unwrap_or(other.background_color),
            code_color: self.code_color.unwrap_or(other.code_color),
            quote_block_color: self.quote_block_color.unwrap_or(other.quote_block_color),
            link_color: self.link_color.unwrap_or(other.link_color),
            select_color: self.select_color.unwrap_or(other.select_color),
            checkbox_color: self.checkbox_color.unwrap_or(other.checkbox_color),
            header_color: self.header_color.unwrap_or(other.header_color),
            scrollbar_width: self.scrollbar_width.unwrap_or(other.scrollbar_width),
            scrollbar_color: self.scrollbar_color.unwrap_or(other.scrollbar_color),
            page_margin: self.page_margin.unwrap_or(other.page_margin),
            // Stays `None` when unset so the bar keeps following the background.
            status_bar_color: self.status_bar_color.or(other.status_bar_color),
            search_match_color: self.search_match_color.unwrap_or(other.search_match_color),
            search_no_match_color: self
                .search_no_match_color
                .unwrap_or(other.search_no_match_color),
            code_highlighter,
        })
    }
}

#[derive(Deserialize, Debug, PartialEq)]
pub struct LinesToScroll(pub f32);

impl From<LinesToScroll> for f32 {
    fn from(value: LinesToScroll) -> Self {
        value.0
    }
}

impl Default for LinesToScroll {
    fn default() -> Self {
        Self(3.0)
    }
}

/// Whether the bottom status bar is shown. Defaults to shown.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusBar(pub bool);

impl From<StatusBar> for bool {
    fn from(value: StatusBar) -> Self {
        value.0
    }
}

impl Default for StatusBar {
    fn default() -> Self {
        Self(true)
    }
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
pub struct KeybindingsSection {
    #[serde(default)]
    pub base: Keybindings,
    pub extra: Option<Keybindings>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum MetricsExporter {
    Log,
    #[cfg(inlyne_tcp_metrics)]
    Tcp,
}

#[derive(Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct DebugSection {
    pub metrics: Option<MetricsExporter>,
    pub render_element_bounds: bool,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Window {
    pub position: Option<Position>,
    pub size: Option<Size>,
}

#[derive(Deserialize, Debug, Default, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub theme: Option<ThemeType>,
    pub decorations: Option<bool>,
    pub scale: Option<f32>,
    pub page_width: Option<f32>,
    pub lines_to_scroll: LinesToScroll,
    pub light_theme: Option<OptionalTheme>,
    pub dark_theme: Option<OptionalTheme>,
    pub font_options: Option<FontOptions>,
    pub keybindings: KeybindingsSection,
    pub debug: DebugSection,
    pub window: Option<Window>,
    /// Whether to show the status bar along the bottom of the window.
    /// Defaults to shown; set `status-bar = false` to hide it.
    pub status_bar: StatusBar,
}

impl Config {
    pub fn load_from_str(s: &str) -> anyhow::Result<Self> {
        let config = toml::from_str(s)?;
        Ok(config)
    }

    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let config_content = read_to_string(path).context(format!(
            "Failed to read configuration file at '{}'",
            path.display()
        ))?;

        Self::load_from_str(&config_content)
    }

    pub fn load_from_system() -> anyhow::Result<Self> {
        let config_dir =
            dirs::config_dir().context("Failed to find the configuration directory")?;

        let config_path = config_dir.join("inlyne").join("inlyne.toml");

        if !config_path.is_file() {
            Self::create_default_config(&config_path)?
        }

        Self::load_from_file(&config_path)
    }

    pub fn create_default_config(path: &PathBuf) -> anyhow::Result<()> {
        create_dir_all(
            path.parent()
                .context("Could not find parent directory of path.")?,
        )?;

        let mut file = std::fs::File::create(path)?;
        file.write_all(Self::default_config().as_bytes())?;
        Ok(())
    }

    pub const fn default_config() -> &'static str {
        include_str!("../../inlyne.default.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_color_is_optional_and_follows_background() {
        // Unset: stays `None` so the renderer falls back to `background_color`,
        // including a customised one.
        let config =
            Config::load_from_str("[dark-theme]\nbackground-color = 0x102030\n").unwrap();
        let theme = config
            .dark_theme
            .unwrap()
            .merge(color::Theme::dark_default())
            .unwrap();
        assert_eq!(theme.background_color, 0x102030);
        assert_eq!(theme.status_bar_color, None);

        // Set: overrides, leaving the background alone.
        let config = Config::load_from_str(
            "[dark-theme]\nbackground-color = 0x102030\nstatus-bar-color = 0xaa3366\n",
        )
        .unwrap();
        let theme = config
            .dark_theme
            .unwrap()
            .merge(color::Theme::dark_default())
            .unwrap();
        assert_eq!(theme.background_color, 0x102030);
        assert_eq!(theme.status_bar_color, Some(0xaa3366));

        // A bad value must be rejected, not silently dropped as an unknown key.
        assert!(Config::load_from_str("[dark-theme]\nstatus-bar-color = \"blue\"\n").is_err());
    }

    /// Match/no-match colours come from the palette and are overridable,
    /// so a theme can restyle the search field without a code change.
    #[test]
    fn search_border_colors_are_themeable() {
        let defaults = color::Theme::dark_default();
        assert_ne!(defaults.search_match_color, defaults.search_no_match_color);

        let config = Config::load_from_str(
            "[dark-theme]\nsearch-match-color = 0x112233\nsearch-no-match-color = 0x445566\n",
        )
        .unwrap();
        let theme = config
            .dark_theme
            .unwrap()
            .merge(color::Theme::dark_default())
            .unwrap();
        assert_eq!(theme.search_match_color, 0x112233);
        assert_eq!(theme.search_no_match_color, 0x445566);

        // Unset falls back to the theme default, not to zero.
        let config = Config::load_from_str("[dark-theme]\n").unwrap();
        let theme = config
            .dark_theme
            .unwrap()
            .merge(color::Theme::dark_default())
            .unwrap();
        assert_eq!(theme.search_match_color, defaults.search_match_color);
    }

    #[test]
    fn defaults_file_is_in_sync() {
        // Load the provided default toml file and compare with what we generate to make sure the
        // defaults stay in sync
        let mut config = Config::load_from_file(Path::new("inlyne.default.toml")).unwrap();

        // Swap out some of the values to compare
        let theme = config.theme.take().unwrap();
        let _ = config.font_options.take();
        let dark_theme = config.dark_theme.take().unwrap();
        let light_theme = config.light_theme.take().unwrap();

        assert_eq!(config, Config::default());
        assert_eq!(theme, ThemeType::Auto);
        assert_eq!(
            dark_theme.merge(color::Theme::dark_default()).unwrap(),
            color::Theme::dark_default()
        );
        assert_eq!(
            light_theme.merge(color::Theme::light_default()).unwrap(),
            color::Theme::light_default()
        );
    }
}
