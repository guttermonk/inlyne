use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::image::ImageData;

use comrak::adapters::SyntaxHighlighterAdapter;
use comrak::plugins::syntect::{SyntectAdapter, SyntectAdapterBuilder};
use comrak::{markdown_to_html_with_plugins, ComrakOptions};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde::Deserialize;
use syntect::highlighting::{Theme as SyntectTheme, ThemeSet as SyntectThemeSet};
use syntect::parsing::SyntaxSet;
use winit::window::CursorIcon;

pub fn format_title(file_path: &Path) -> String {
    match root_filepath_to_vcs_dir(file_path) {
        Some(path) => format!("Inlyne - {}", path.to_string_lossy()),
        None => "Inlyne".to_owned(),
    }
}

/// Gets a relative path extending from the repo root falling back to the full path
fn root_filepath_to_vcs_dir(path: &Path) -> Option<PathBuf> {
    let mut full_path = path.canonicalize().ok()?;
    let mut parts = vec![full_path.file_name()?.to_owned()];

    full_path.pop();
    loop {
        full_path.push(".git");
        let is_git = full_path.exists();
        full_path.pop();
        full_path.push(".hg");
        let is_mercurial = full_path.exists();
        full_path.pop();

        let is_vcs_dir = is_git || is_mercurial;

        match full_path.file_name() {
            Some(name) => parts.push(name.to_owned()),
            // We've searched the full path and didn't find a vcs dir
            None => return Some(path.to_owned()),
        }
        if is_vcs_dir {
            let mut rooted = PathBuf::new();
            for part in parts.into_iter().rev() {
                rooted.push(part);
            }
            return Some(rooted);
        }

        full_path.pop();
    }
}

pub(crate) fn default<T: Default>() -> T {
    Default::default()
}

pub fn usize_in_mib(num: usize) -> f32 {
    num as f32 / 1_024.0 / 1_024.0
}

pub type Point = (f32, f32);

pub fn dist_between_points(p1: &Point, p2: &Point) -> f32 {
    f32::sqrt((p2.0 - p1.0).powf(2.0) + (p2.1 - p1.1).powf(2.0))
}

pub type Size = (f32, f32);
pub type ImageCache = Arc<Mutex<HashMap<String, Arc<Mutex<Option<ImageData>>>>>>;

#[derive(Debug, Clone)]
pub struct Line {
    pub min: Point,
    pub max: Point,
    pub color: [f32; 4],
}

impl Line {
    pub fn with_color(min: Point, max: Point, color: [f32; 4]) -> Self {
        Self { min, max, color }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rect {
    pub pos: Point,
    pub size: Point,
}

impl Rect {
    pub fn new(pos: Point, size: Point) -> Rect {
        Rect { pos, size }
    }

    pub fn from_min_max(min: Point, max: Point) -> Rect {
        Rect {
            pos: min,
            size: (max.0 - min.0, max.1 - min.1),
        }
    }

    pub fn max(&self) -> Point {
        (self.pos.0 + self.size.0, self.pos.1 + self.size.1)
    }

    pub fn contains(&self, loc: Point) -> bool {
        self.pos.0 <= loc.0 && loc.0 <= self.max().0 && self.pos.1 <= loc.1 && loc.1 <= self.max().1
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

impl Align {
    pub fn new(s: &str) -> Option<Self> {
        let align = match s {
            "left" => Self::Left,
            "center" => Self::Center,
            "right" => Self::Right,
            _ => return None,
        };

        Some(align)
    }
}

#[derive(Default)]
pub struct HoverInfo {
    pub cursor_icon: CursorIcon,
    pub jump: Option<f32>,
}

impl From<CursorIcon> for HoverInfo {
    fn from(cursor_icon: CursorIcon) -> Self {
        Self {
            cursor_icon,
            ..Default::default()
        }
    }
}

// TODO(cosmic): Remove after `comrak` supports code block info strings that have a comma
//     (like ```rust,ignore)
//     https://github.com/kivikakk/comrak/issues/246
struct CustomSyntectAdapter(SyntectAdapter);

impl SyntaxHighlighterAdapter for CustomSyntectAdapter {
    fn write_highlighted(
        &self,
        output: &mut dyn io::Write,
        lang: Option<&str>,
        code: &str,
    ) -> io::Result<()> {
        let norm_lang = lang.map(|l| l.split_once(',').map(|(lang, _)| lang).unwrap_or(l));
        self.0.write_highlighted(output, norm_lang, code)
    }

    fn write_pre_tag(
        &self,
        output: &mut dyn io::Write,
        attributes: HashMap<String, String>,
    ) -> io::Result<()> {
        self.0.write_pre_tag(output, attributes)
    }

    fn write_code_tag(
        &self,
        output: &mut dyn io::Write,
        attributes: HashMap<String, String>,
    ) -> io::Result<()> {
        self.0.write_code_tag(output, attributes)
    }
}

pub fn markdown_to_html(md: &str, syntax_theme: SyntectTheme) -> String {
    let mut options = ComrakOptions::default();
    options.extension.autolink = true;
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    // options.extension.footnotes = true;
    options.extension.front_matter_delimiter = Some("---".to_owned());
    options.extension.shortcodes = true;
    options.parse.smart = true;
    options.render.unsafe_ = true;

    // TODO(cosmic): gonna send a PR so that a plugin can pass in a single theme too
    let dummy_name = "theme";
    let mut theme_set = SyntectThemeSet::new();
    theme_set
        .themes
        .insert(String::from(dummy_name), syntax_theme);
    static CACHED_SYN_SET: OnceLock<SyntaxSet> = OnceLock::new();
    // Initializing this is non-trivial. Cache so it only runs once
    let syn_set = CACHED_SYN_SET
        .get_or_init(two_face::syntax::extra_no_newlines)
        .to_owned();
    let adapter = SyntectAdapterBuilder::new()
        .syntax_set(syn_set)
        .theme_set(theme_set)
        .theme(dummy_name)
        .build();

    let mut plugins = comrak::ComrakPlugins::default();
    let custom = CustomSyntectAdapter(adapter);
    plugins.render.codefence_syntax_highlighter = Some(&custom);

    let mut htmlified = markdown_to_html_with_plugins(md, &options, &plugins);
    
    // Post-process HTML to support pandoc-style table captions
    htmlified = add_pandoc_table_captions(htmlified);

    // Comrak doesn't support converting the front matter to HTML, so we have to convert it to an
    // HTML table ourselves. Front matter is found like so
    // ---
    // {YAML value}
    // ---
    // {Markdown}
    let html_front_matter = if md.starts_with("---") {
        let mut parts = md.split("---");
        let _ = parts.next();
        parts
            .next()
            .and_then(
                |front_matter| match serde_yaml::from_str::<FrontMatter>(front_matter) {
                    Ok(front_matter) => Some(front_matter.to_table()),
                    Err(err) => {
                        tracing::warn!(
                            "Failed parsing front matter. Error: {}\n{}",
                            err,
                            front_matter
                        );
                        None
                    }
                },
            )
            .unwrap_or_default()
    } else {
        String::new()
    };

    format!("{html_front_matter}{htmlified}")
}

/// Post-processes HTML to convert pandoc-style table captions into HTML <caption> tags.
/// 
/// Supports two pandoc caption styles:
/// 1. "Table: caption text" before a table -> converts to <caption> at start of table
/// 2. ": caption text" after a table -> converts to <caption> at start of table
fn add_pandoc_table_captions(html: String) -> String {
    let mut result = String::with_capacity(html.len());
    let mut lines = html.lines().peekable();
    let mut pending_caption: Option<String> = None;
    
    while let Some(line) = lines.next() {
        // Check for caption before table (Table: caption text)
        if line.starts_with("<p>Table: ") && line.ends_with("</p>") {
            // Check if next non-empty line is a table
            let mut temp_lines = Vec::new();
            let mut found_table = false;
            
            while let Some(next_line) = lines.peek() {
                if next_line.trim().is_empty() {
                    temp_lines.push(lines.next().unwrap());
                } else if next_line.starts_with("<table>") {
                    found_table = true;
                    break;
                } else {
                    break;
                }
            }
            
            if found_table {
                // Extract caption text
                let caption_text = &line[10..line.len() - 4]; // Remove "<p>Table: " and "</p>"
                pending_caption = Some(caption_text.to_string());
                // Add the empty lines we consumed
                for temp_line in temp_lines {
                    result.push_str(temp_line);
                    result.push('\n');
                }
            } else {
                // Not followed by a table, keep as regular paragraph
                result.push_str(line);
                result.push('\n');
                // Add back the lines we consumed
                for temp_line in temp_lines {
                    result.push_str(temp_line);
                    result.push('\n');
                }
            }
        } 
        // Check if we're at a table opening tag
        else if line.starts_with("<table>") {
            result.push_str("<table>\n");
            
            // If we have a pending caption, add it now
            if let Some(caption) = pending_caption.take() {
                result.push_str("<caption>");
                result.push_str(&caption);
                result.push_str("</caption>\n");
            }
        }
        // Check for caption after table (: caption text)
        else if line.starts_with("</table>") {
            result.push_str(line);
            result.push('\n');
            
            // Look ahead for a caption after the table
            let mut temp_lines = Vec::new();
            let mut found_caption = false;
            
            while let Some(next_line) = lines.peek() {
                if next_line.trim().is_empty() {
                    temp_lines.push(lines.next().unwrap());
                } else if next_line.starts_with("<p>: ") && next_line.ends_with("</p>") {
                    // Found a caption after the table
                    let caption_line = lines.next().unwrap();
                    let caption_text = &caption_line[5..caption_line.len() - 4]; // Remove "<p>: " and "</p>"
                    
                    // We need to insert the caption at the beginning of the table
                    // Find the table start in our result
                    if let Some(table_start) = result.rfind("<table>") {
                        let table_start_end = table_start + 7; // Length of "<table>"
                        let caption_html = format!("\n<caption>{}</caption>", caption_text);
                        result.insert_str(table_start_end, &caption_html);
                    }
                    
                    found_caption = true;
                    break;
                } else {
                    break;
                }
            }
            
            // Add back any empty lines we consumed
            if !found_caption {
                for temp_line in temp_lines {
                    result.push_str(temp_line);
                    result.push('\n');
                }
            }
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    
    // Remove trailing newline if present
    if result.ends_with('\n') {
        result.pop();
    }
    
    result
}

#[derive(Deserialize, Debug)]
struct FrontMatter(IndexMap<String, Cell>);

impl FrontMatter {
    fn to_table(&self) -> String {
        let mut table = String::from("<table>\n");

        table.push_str("<thead>\n<tr>\n");
        for key in self.0.keys() {
            table.push_str("<th align=\"center\">");
            html_escape::encode_safe_to_string(key, &mut table);
            table.push_str("</th>\n");
        }
        table.push_str("</tr>\n</thead>\n");

        table.push_str("<tbody>\n<tr>\n");
        for cell in self.0.values() {
            table.push_str("<td align=\"center\">");
            cell.render_into(&mut table);
            table.push_str("</td>\n");
        }
        table.push_str("</tr>\n</tbody>\n");

        table.push_str("</table>\n");
        table
    }
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum Cell {
    Str(String),
    Table(Vec<String>),
}

impl Cell {
    fn render_into(&self, buf: &mut String) {
        match self {
            Self::Str(s) => {
                html_escape::encode_safe_to_string(s, buf);
            }
            Self::Table(_v) => {
                tracing::warn!("Nested tables aren't supported yet. Skipping");
                buf.push_str("{Skipped nested table}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pandoc_caption_before_table() {
        let md = r#"Table: This is a caption before the table

| Column 1 | Column 2 | Column 3 |
|----------|----------|----------|
| Data 1   | Data 2   | Data 3   |
| Data 4   | Data 5   | Data 6   |"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("Caption before table HTML:\n{}", html);
        
        // Check that the caption was converted to an HTML caption tag
        assert!(html.contains("<caption>This is a caption before the table</caption>"));
        assert!(html.contains("<table>"));
        assert!(!html.contains("<p>Table: This is a caption before the table</p>"));
    }

    #[test]
    fn test_pandoc_caption_after_table() {
        let md = r#"| Header A | Header B | Header C |
|----------|----------|----------|
| Value 1  | Value 2  | Value 3  |
| Value 4  | Value 5  | Value 6  |

: This is a caption after the table"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("Caption after table HTML:\n{}", html);
        
        // Check that the caption was converted to an HTML caption tag
        assert!(html.contains("<caption>This is a caption after the table</caption>"));
        assert!(html.contains("<table>"));
        assert!(!html.contains("<p>: This is a caption after the table</p>"));
    }

    #[test]
    fn test_html_table_with_caption() {
        let md = r#"<table>
<caption>This is an HTML caption</caption>
<tr>
  <th>Name</th>
  <th>Age</th>
  <th>City</th>
</tr>
<tr>
  <td>Alice</td>
  <td>30</td>
  <td>New York</td>
</tr>
</table>"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("HTML table with caption:\n{}", html);
        
        // HTML captions should be preserved
        assert!(html.contains("<caption>"));
        assert!(html.contains("This is an HTML caption"));
    }

    #[test]
    fn test_regular_table_without_caption() {
        let md = r#"| Fruit    | Color  | Price |
|----------|--------|-------|
| Apple    | Red    | $1.00 |
| Banana   | Yellow | $0.50 |
| Orange   | Orange | $0.75 |"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("Regular table HTML:\n{}", html);
        
        // Should have a table but no caption
        assert!(html.contains("<table>"));
        assert!(!html.contains("<caption>"));
    }

    #[test]
    fn test_empty_caption() {
        let md = r#"<table>
<caption></caption>
<tr>
  <th>Header</th>
  <th>Value</th>
</tr>
<tr>
  <td>Row 1</td>
  <td>Data 1</td>
</tr>
</table>"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("Empty caption HTML:\n{}", html);
        
        // Empty caption should not be in the output
        assert!(html.contains("<table>"));
        assert!(html.contains("<th>Header</th>"));
        // The empty caption tag should still be present in HTML but won't create visual space
        assert!(html.contains("<caption></caption>"));
    }

    #[test]
    fn test_whitespace_only_caption() {
        let md = r#"<table>
<caption>   </caption>
<tr>
  <th>Name</th>
  <th>Age</th>
</tr>
<tr>
  <td>Alice</td>
  <td>30</td>
</tr>
</table>"#;

        let html = markdown_to_html(md, SyntectTheme::default());
        println!("Whitespace-only caption HTML:\n{}", html);
        
        // Whitespace-only caption should be preserved in HTML
        assert!(html.contains("<table>"));
        assert!(html.contains("<caption>   </caption>"));
    }


}
