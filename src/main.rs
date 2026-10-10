#![allow(
    // I don't really care enough about the names here to fix things atm
    clippy::enum_variant_names,
)]
#![warn(
    // Generally we don't want this sneaking into `main`
    clippy::todo,
    // This should be used very sparingly compared between logging and clap
    clippy::print_stdout, clippy::print_stderr,
)]

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

mod clipboard;
pub mod color;
mod debug_impls;
mod file_watcher;
pub mod fonts;
pub mod history;
pub mod image;
pub mod interpreter;
mod keybindings;
mod metrics;
pub mod opts;
mod panic_hook;
pub mod positioner;
pub mod renderer;
pub mod selection;
pub mod table;
#[cfg(test)]
pub mod test_utils;
pub mod text;
pub mod utils;

use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::read_to_string;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, channel};
use std::sync::Arc;
use std::time::Instant;

use file_watcher::Watcher;
use image::{Image, ImageData};
use interpreter::HtmlInterpreter;
use keybindings::action::{Action, HistDirection, VertDirection, Zoom};
use keybindings::{Key, KeyCombos, ModifiedKey};
use metrics::{histogram, HistTag};
use opts::{Cli, Config, Opts};
use parking_lot::Mutex;
use positioner::{Positioned, Row, Section, Spacer, DEFAULT_MARGIN};
use raw_window_handle::HasRawDisplayHandle;
use renderer::Renderer;
use table::Table;
use text::{Text, TextBox, TextSystem};
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;
use utils::{ImageCache, Point, Rect, Size};

use crate::opts::{Commands, ConfigCmd, MetricsExporter};
use crate::selection::Selection;
use anyhow::Context;
use clap::Parser;
use taffy::Taffy;
use winit::event::{
    ElementState, Event, KeyboardInput, ModifiersState, MouseButton, MouseScrollDelta, 
    VirtualKeyCode, WindowEvent,
};
use winit::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use winit::window::{CursorIcon, Window, WindowBuilder};

pub enum InlyneEvent {
    LoadedImage(String, Arc<Mutex<Option<ImageData>>>),
    FileReload,
    FileChange { contents: String },
    Reposition,
    PositionQueue,
}

impl Debug for InlyneEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Inlyne Event")
    }
}

pub enum Hoverable<'a> {
    Image(&'a Image),
    Text(&'a Text),
    Summary(&'a Section),
}

#[derive(Debug, PartialEq)]
pub enum Element {
    TextBox(TextBox),
    Spacer(Spacer),
    Image(Image),
    Table(Table),
    Row(Row),
    Section(Section),
}

impl From<Section> for Element {
    fn from(section: Section) -> Self {
        Element::Section(section)
    }
}

impl From<Row> for Element {
    fn from(row: Row) -> Self {
        Element::Row(row)
    }
}

impl From<Image> for Element {
    fn from(image: Image) -> Self {
        Element::Image(image)
    }
}

impl From<Spacer> for Element {
    fn from(spacer: Spacer) -> Self {
        Element::Spacer(spacer)
    }
}

impl From<TextBox> for Element {
    fn from(text_box: TextBox) -> Self {
        Element::TextBox(text_box)
    }
}

impl From<Table> for Element {
    fn from(table: Table) -> Self {
        Element::Table(table)
    }
}

pub struct Inlyne {
    opts: Opts,
    window: Arc<Window>,
    // HACK: `Option<_>` is used here to keep `Inlyne` valid while running the event loop. Consider
    // splitting this out from the rest of the state
    event_loop: Option<EventLoop<InlyneEvent>>,
    renderer: Renderer,
    element_queue: Arc<Mutex<Vec<Element>>>,
    elements: Vec<Positioned<Element>>,
    lines_to_scroll: f32,
    image_cache: ImageCache,
    interpreter_sender: mpsc::Sender<String>,
    keycombos: KeyCombos,
    need_repositioning: bool,
    watcher: Watcher,
    selection: Selection,
    help_visible: bool,
    help_elements: Vec<Positioned<Element>>,
    help_element_queue: Arc<Mutex<Vec<Element>>>,
    saved_scroll_y: f32,
    current_file_content: String,
    event_loop_proxy: EventLoopProxy<InlyneEvent>,
    // Search state
    search_active: bool,
    search_navigating: bool, // True when Enter pressed, allows easier navigation
    search_query: String,
    search_matches: Vec<(usize, usize, usize)>, // (element_index, line_index, byte_offset_in_line)
    current_match: Option<usize>,
    search_display_text: String,
    /// URL of the link currently under the cursor, shown in the status bar.
    hovered_link: Option<String>,
    /// Set when a key press was consumed as a search-navigation command, so the
    /// `ReceivedCharacter` that follows it isn't also typed into the query.
    /// Winit emits `KeyboardInput` before `ReceivedCharacter` for a press, and
    /// this is reset on every press, so it cannot leak to a later key.
    suppress_next_char: bool,
    /// Keybinding hints for the status bar. Derived from the user's actual
    /// keybindings once at startup rather than hardcoded, so the hints stay
    /// correct when those are customised.
    status_bar_hints: String,
    /// Hints shown instead while the help screen is open, where a different
    /// set of keys applies.
    status_bar_hints_help: String,
    /// Hints shown while a search query is being typed.
    status_bar_hints_search: String,
    /// Hints shown once the search has been run and match-stepping is live.
    status_bar_hints_search_nav: String,
    /// Page movement that still works mid-query, appended to the search
    /// hints so the bar keeps showing what the screen can still do.
    status_bar_hints_search_extra: String,
}

impl Inlyne {
    pub fn new(opts: Opts) -> anyhow::Result<Self> {
        let keycombos = KeyCombos::new(opts.keybindings.clone())?;

        let file_path = opts.history.get_path().to_owned();

        let event_loop = EventLoopBuilder::<InlyneEvent>::with_user_event().build();

        let window = {
            let mut wb = WindowBuilder::new().with_title(utils::format_title(&file_path));

            if let Some(decorations) = opts.decorations {
                wb = wb.with_decorations(decorations);
            }
            if let Some(ref pos) = opts.position {
                wb = wb.with_position(winit::dpi::PhysicalPosition::new(pos.x, pos.y));
            }
            if let Some(ref size) = opts.size {
                wb = wb.with_inner_size(winit::dpi::PhysicalSize::new(size.width, size.height));
            }
            #[cfg(all(feature = "wayland", not(any(target_os = "macos", windows))))]
            {
                use winit::platform::wayland::WindowBuilderExtWayland;
                wb = wb.with_name("inlyne", "");
            }

            Arc::new(wb.build(&event_loop).unwrap())
        };

        let mut renderer = pollster::block_on(Renderer::new(
            &window,
            opts.theme.clone(),
            opts.scale.unwrap_or(window.scale_factor() as f32),
            opts.page_width.unwrap_or(f32::MAX),
            opts.font_opts.clone(),
        ))?;

        let element_queue = Arc::new(Mutex::new(Vec::new()));
        let image_cache = Arc::new(Mutex::new(HashMap::new()));
        let md_string = read_to_string(&file_path)
            .with_context(|| format!("Could not read file at '{}'", file_path.display()))?;

        let event_loop_proxy = event_loop.create_proxy();
        // Set element padding from options
        renderer.element_padding = opts.element_padding;
        renderer.status_bar_visible = opts.status_bar;
        
        let interpreter = HtmlInterpreter::new(
            window.clone(),
            element_queue.clone(),
            renderer.theme.clone(),
            renderer.surface_format,
            renderer.hidpi_scale,
            image_cache.clone(),
            event_loop_proxy.clone(),
            opts.color_scheme,
            true,   // Add spacers before headers for separation from previous content
            false,  // NO spacers after headers - keep tables close
            false,  // NO spacers before tables - keep close to headers
            true,   // Add spacers after tables for separation
            true,   // Add spacers after paragraphs for better flow
            true,   // Add spacers after lists for better flow
        );

        let (interpreter_sender, interpreter_receiver) = channel();
        std::thread::spawn(move || interpreter.interpret_md(interpreter_receiver));

        interpreter_sender.send(md_string.clone())?;

        let lines_to_scroll = opts.lines_to_scroll;

        let watcher = Watcher::spawn(event_loop_proxy.clone(), file_path.clone());

        let _ = file_path.parent().map(std::env::set_current_dir);

        // Built before `opts` is moved into the struct below.
        let hint_keybindings: keybindings::Keybindings = opts.keybindings.clone().into();
        let status_bar_hints = Self::build_status_bar_hints(&hint_keybindings);
        let status_bar_hints_help = Self::build_help_status_bar_hints(&hint_keybindings);
        let status_bar_hints_search = Self::build_search_status_bar_hints(&hint_keybindings);
        let status_bar_hints_search_nav =
            Self::build_search_nav_status_bar_hints(&hint_keybindings);
        let status_bar_hints_search_extra =
            Self::build_search_extra_hints(&hint_keybindings);

        Ok(Self {
            opts,
            window,
            event_loop: Some(event_loop),
            renderer,
            element_queue,
            elements: Vec::new(),
            lines_to_scroll,
            interpreter_sender,
            image_cache,
            keycombos,
            need_repositioning: false,
            watcher,
            selection: Selection::new(),
            help_visible: false,
            help_elements: Vec::new(),
            help_element_queue: Arc::new(Mutex::new(Vec::new())),
            saved_scroll_y: 0.0,
            current_file_content: md_string,
            event_loop_proxy,
            search_active: false,
            search_navigating: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            current_match: None,
            search_display_text: String::new(),
            hovered_link: None,
            suppress_next_char: false,
            status_bar_hints,
            status_bar_hints_help,
            status_bar_hints_search,
            status_bar_hints_search_nav,
            status_bar_hints_search_extra,
        })
    }

    /// The glyph a Shift-ed key actually produces, so a binding reads the way
    /// it is typed: `?` rather than `Shift+/`, `G` rather than `Shift+g`.
    ///
    /// The punctuation half assumes a US layout, which is the same assumption
    /// the stock config already makes when it writes `"G"` and documents Help
    /// as `?`. Keys with no shifted form return `None` and fall back to the
    /// plain `Shift+<key>` rendering.
    fn shifted_glyph(virt_key: VirtualKeyCode) -> Option<&'static str> {
        use VirtualKeyCode as K;
        Some(match virt_key {
            K::A => "A", K::B => "B", K::C => "C", K::D => "D", K::E => "E",
            K::F => "F", K::G => "G", K::H => "H", K::I => "I", K::J => "J",
            K::K => "K", K::L => "L", K::M => "M", K::N => "N", K::O => "O",
            K::P => "P", K::Q => "Q", K::R => "R", K::S => "S", K::T => "T",
            K::U => "U", K::V => "V", K::W => "W", K::X => "X", K::Y => "Y",
            K::Z => "Z",
            K::Key1 => "!", K::Key2 => "@", K::Key3 => "#", K::Key4 => "$",
            K::Key5 => "%", K::Key6 => "^", K::Key7 => "&", K::Key8 => "*",
            K::Key9 => "(", K::Key0 => ")",
            K::Slash => "?",
            K::Minus => "_",
            K::Equals => "+",
            K::LBracket => "{",
            K::RBracket => "}",
            K::Backslash => "|",
            K::Semicolon => ":",
            K::Apostrophe => "\"",
            K::Comma => "<",
            K::Period => ">",
            K::Grave => "~",
            _ => return None,
        })
    }

    /// Formats a key combo for display in the status bar and help screen.
    ///
    /// `KeyCombo`'s `Display` wraps non-printing and modified keys in angle
    /// brackets (`<Up>`, `<Ctrl+f>`) and runs chords together
    /// (`Shift+zShift+q`), neither of which reads well. This strips the
    /// brackets, spaces out chords, abbreviates long key names, and renders
    /// Shift-ed keys as the glyph they produce.
    fn format_key_combo(combo: &keybindings::KeyCombo) -> String {
        let rendered = combo
            .0
            .iter()
            .map(|modified_key| {
                // Shift alone: show the glyph that key actually types.
                if let ModifiedKey(Key::Resolved(virt_key), mods) = modified_key {
                    if *mods == ModifiersState::SHIFT {
                        if let Some(glyph) = Self::shifted_glyph(*virt_key) {
                            return glyph.to_string();
                        }
                    }
                }
                format!("{}", modified_key)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let stripped: String = rendered.chars().filter(|c| !matches!(c, '<' | '>')).collect();

        for (long, short) in [
            ("PageUp", "PgUp"),
            ("PageDown", "PgDn"),
            ("Escape", "Esc"),
            ("Return", "Enter"),
            ("Back", "Bksp"),
        ] {
            if stripped.contains(long) {
                return stripped.replace(long, short);
            }
        }
        stripped
    }

    /// How many alternative bindings per action the status bar will show. The
    /// bar is one line, and some actions carry a lot of bindings (vim-style
    /// chords especially); the help screen remains the full reference.
    const MAX_HINT_ALTERNATIVES: usize = 2;

    /// Whether a key press will also arrive as a `ReceivedCharacter`, i.e.
    /// whether it would be typed into an open search query.
    ///
    /// Only such keys are ambiguous during search: binding `j` to NextMatch
    /// would make "json" untypeable. Keys that never produce text -- Enter,
    /// Tab, the arrows, F-keys, anything with Ctrl/Alt/Super -- can navigate
    /// immediately without stealing input.
    fn key_produces_text(
        virtual_keycode: Option<VirtualKeyCode>,
        modifiers: ModifiersState,
    ) -> bool {
        // Ctrl/Alt/Super combos don't emit characters.
        if modifiers.ctrl() || modifiers.alt() || modifiers.logo() {
            return false;
        }

        match virtual_keycode {
            Some(
                VirtualKeyCode::Return
                | VirtualKeyCode::NumpadEnter
                | VirtualKeyCode::Tab
                | VirtualKeyCode::Escape
                | VirtualKeyCode::Back
                | VirtualKeyCode::Delete
                | VirtualKeyCode::Insert
                | VirtualKeyCode::Home
                | VirtualKeyCode::End
                | VirtualKeyCode::PageUp
                | VirtualKeyCode::PageDown
                | VirtualKeyCode::Up
                | VirtualKeyCode::Down
                | VirtualKeyCode::Left
                | VirtualKeyCode::Right
                | VirtualKeyCode::F1
                | VirtualKeyCode::F2
                | VirtualKeyCode::F3
                | VirtualKeyCode::F4
                | VirtualKeyCode::F5
                | VirtualKeyCode::F6
                | VirtualKeyCode::F7
                | VirtualKeyCode::F8
                | VirtualKeyCode::F9
                | VirtualKeyCode::F10
                | VirtualKeyCode::F11
                | VirtualKeyCode::F12,
            ) => false,
            _ => true,
        }
    }

    /// Whether a binding still fires while a search query is being typed.
    /// Character-producing keys are swallowed by the query, so advertising
    /// them in the search-mode hints would be wrong.
    fn combo_works_during_search(combo: &keybindings::KeyCombo) -> bool {
        match combo.0.first() {
            Some(ModifiedKey(Key::Resolved(virt_key), mods)) => {
                !Self::key_produces_text(Some(*virt_key), *mods)
            }
            // Scan-code bindings can't be classified; assume they type.
            _ => false,
        }
    }

    /// Like [`Self::keys_for`], but only the bindings that survive text entry.
    fn keys_for_during_search(
        keybindings: &keybindings::Keybindings,
        want: Action,
    ) -> Vec<String> {
        keybindings
            .iter()
            .filter(|(action, combo)| {
                *action == want && Self::combo_works_during_search(combo)
            })
            .map(|(_, combo)| Self::format_key_combo(combo))
            .take(Self::MAX_HINT_ALTERNATIVES)
            .collect()
    }

    /// Every key combo bound to `want`, in the order the user listed them.
    fn keys_for(keybindings: &keybindings::Keybindings, want: Action) -> Vec<String> {
        keybindings
            .iter()
            .filter(|(action, _)| *action == want)
            .map(|(_, combo)| Self::format_key_combo(combo))
            .take(Self::MAX_HINT_ALTERNATIVES)
            .collect()
    }

    /// Hint for a direction pair, e.g. `Up/Down, i/e scroll`. Alternatives are
    /// zipped positionally so each up key is shown beside its matching down
    /// key, which is how they are listed in the config.
    fn pair_hint(
        keybindings: &keybindings::Keybindings,
        up: Action,
        down: Action,
        label: &str,
    ) -> Option<String> {
        let ups = Self::keys_for(keybindings, up);
        let downs = Self::keys_for(keybindings, down);
        if ups.is_empty() && downs.is_empty() {
            return None;
        }

        // Pair them off; any surplus on either side is listed on its own.
        let paired: Vec<String> = ups
            .iter()
            .zip(downs.iter())
            .map(|(u, d)| format!("{}/{}", u, d))
            .chain(ups.iter().skip(downs.len()).cloned())
            .chain(downs.iter().skip(ups.len()).cloned())
            .collect();

        Some(format!("{} {}", paired.join(", "), label))
    }

    /// Hint for a single action, listing every binding, e.g. `?/h help`.
    fn single_hint(
        keybindings: &keybindings::Keybindings,
        action: Action,
        label: &str,
    ) -> Option<String> {
        let keys = Self::keys_for(keybindings, action);
        // Comma-separated, not slash: a slash would be ambiguous against both
        // the up/down pairs and the `/` key itself.
        (!keys.is_empty()).then(|| format!("{} {}", keys.join(", "), label))
    }

    /// Builds the keybinding hints shown in the status bar, e.g.
    /// `Up/Down, i/e scroll │ PgUp/PgDn page │ Ctrl+f search │ ?/h help │ Esc quit`.
    ///
    /// Every binding for an action is listed, so custom keys show up alongside
    /// the defaults rather than being hidden behind whichever came first.
    fn build_status_bar_hints(keybindings: &keybindings::Keybindings) -> String {
        let mut hints: Vec<String> = Vec::new();
        hints.extend(Self::pair_hint(
            keybindings,
            Action::Scroll(VertDirection::Up),
            Action::Scroll(VertDirection::Down),
            "scroll",
        ));
        hints.extend(Self::pair_hint(
            &keybindings,
            Action::Page(VertDirection::Up),
            Action::Page(VertDirection::Down),
            "page",
        ));
        for (action, label) in [
            (Action::Search, "search"),
            (Action::Copy, "copy"),
            (Action::Help, "help"),
            (Action::Quit, "quit"),
        ] {
            hints.extend(Self::single_hint(&keybindings, action, label));
        }

        hints.join(crate::renderer::STATUS_BAR_SEPARATOR)
    }

    /// Hints for while search mode is active. The page keys don't apply --
    /// typing goes into the query -- so only moving between matches and
    /// leaving search are worth advertising.
    fn build_search_status_bar_hints(keybindings: &keybindings::Keybindings) -> String {
        // Only keys that survive text entry are listed: a character key bound
        // to one of these is typed into the query instead of firing. Enter is
        // what runs the query and lands on the first match.
        let mut hints: Vec<String> = Vec::new();
        let run_keys = Self::keys_for_during_search(keybindings, Action::NextMatch);
        if run_keys.is_empty() {
            hints.push("Enter run search".to_string());
        } else {
            hints.push(format!("{} run search", run_keys.join(", ")));
        }
        hints.push(Self::search_exit_hint(keybindings));

        hints.join(crate::renderer::STATUS_BAR_SEPARATOR)
    }

    /// Hints for once a search has been run and match-stepping is live. Unlike
    /// while typing, character keys bound to next/prev now work, so they are
    /// listed too.
    fn build_search_nav_status_bar_hints(keybindings: &keybindings::Keybindings) -> String {
        let mut hints: Vec<String> = Vec::new();
        for (action, label) in [
            (Action::NextMatch, "next match"),
            (Action::PrevMatch, "prev match"),
        ] {
            let keys = Self::keys_for(keybindings, action);
            if !keys.is_empty() {
                hints.push(format!("{} {}", keys.join(", "), label));
            }
        }
        hints.push(Self::search_exit_hint(keybindings));

        hints.join(crate::renderer::STATUS_BAR_SEPARATOR)
    }

    /// Page movement that still works while a query is being typed, i.e. keys
    /// that emit no text. Appended to the search hints so the bar keeps showing
    /// what the current screen can still do, rather than only search itself.
    fn build_search_extra_hints(keybindings: &keybindings::Keybindings) -> String {
        let mut hints: Vec<String> = Vec::new();
        for (up, down, label) in [
            (
                Action::Scroll(VertDirection::Up),
                Action::Scroll(VertDirection::Down),
                "scroll",
            ),
            (
                Action::Page(VertDirection::Up),
                Action::Page(VertDirection::Down),
                "page",
            ),
        ] {
            let ups = Self::keys_for_during_search(keybindings, up);
            let downs = Self::keys_for_during_search(keybindings, down);
            if ups.is_empty() && downs.is_empty() {
                continue;
            }
            let paired: Vec<String> = ups
                .iter()
                .zip(downs.iter())
                .map(|(u, d)| format!("{}/{}", u, d))
                .chain(ups.iter().skip(downs.len()).cloned())
                .chain(downs.iter().skip(ups.len()).cloned())
                .collect();
            hints.push(format!("{} {}", paired.join(", "), label));
        }

        hints.join(crate::renderer::STATUS_BAR_SEPARATOR)
    }

    /// How to leave search. Exiting is never taken over by match-stepping, so
    /// only keys that survive text entry qualify in either sub-state.
    fn search_exit_hint(keybindings: &keybindings::Keybindings) -> String {
        let mut exit_keys = Self::keys_for_during_search(keybindings, Action::CancelSearch);
        exit_keys.extend(Self::keys_for_during_search(keybindings, Action::Quit));
        exit_keys.extend(Self::keys_for_during_search(keybindings, Action::Search));
        exit_keys.truncate(Self::MAX_HINT_ALTERNATIVES);
        if exit_keys.is_empty() {
            "Esc exit search".to_string()
        } else {
            format!("{} exit search", exit_keys.join(", "))
        }
    }

    /// Hints for while the help screen is open. Only scrolling and closing do
    /// anything there, and `Quit` closes the help rather than exiting, so
    /// advertising "quit" would be wrong.
    fn build_help_status_bar_hints(keybindings: &keybindings::Keybindings) -> String {
        let mut hints: Vec<String> = Vec::new();
        hints.extend(Self::pair_hint(
            keybindings,
            Action::Scroll(VertDirection::Up),
            Action::Scroll(VertDirection::Down),
            "scroll",
        ));
        hints.extend(Self::pair_hint(
            keybindings,
            Action::Page(VertDirection::Up),
            Action::Page(VertDirection::Down),
            "page",
        ));

        // Both the help key and the quit key close the help screen.
        let mut close_keys = Self::keys_for(keybindings, Action::Help);
        close_keys.extend(Self::keys_for(keybindings, Action::Quit));
        close_keys.truncate(Self::MAX_HINT_ALTERNATIVES);
        if !close_keys.is_empty() {
            hints.push(format!("{} close help", close_keys.join(", ")));
        }

        hints.join(crate::renderer::STATUS_BAR_SEPARATOR)
    }


    fn get_help_html(&self) -> String {
        use keybindings::action::{Action, HistDirection, VertDirection, Zoom};
        
        // Convert keybindings to markdown/HTML hybrid
        let keybindings: keybindings::Keybindings = self.opts.keybindings.clone().into();
        
        // Group keybindings by action
        let mut action_map: HashMap<String, Vec<String>> = HashMap::new();
        
        for (action, combo) in keybindings.iter() {
            let action_name = match action {
                Action::Scroll(VertDirection::Up) => "Scroll Up",
                Action::Scroll(VertDirection::Down) => "Scroll Down",
                Action::Page(VertDirection::Up) => "Page Up",
                Action::Page(VertDirection::Down) => "Page Down",
                Action::ToEdge(VertDirection::Up) => "Go to Top",
                Action::ToEdge(VertDirection::Down) => "Go to Bottom",
                Action::Zoom(Zoom::In) => "Zoom In",
                Action::Zoom(Zoom::Out) => "Zoom Out",
                Action::Zoom(Zoom::Reset) => "Reset Zoom",
                Action::History(HistDirection::Next) => "Next File",
                Action::History(HistDirection::Prev) => "Previous File",
                Action::Copy => "Copy Selection",
                Action::Help => "Toggle Help",
                // Not "Toggle Search": the key opens search, and `Cancel
                // Search` below covers closing it, so "toggle" only muddies
                // what the key is for.
                Action::Search => "Search",
                Action::CancelSearch => "Cancel Search",
                Action::NextMatch => "Next Search Match",
                Action::PrevMatch => "Previous Search Match",
                Action::Quit => "Quit",
            };
            
            // Same rendering as the status bar, so a binding is described
            // identically wherever it appears: `?`, not `<Shift+/>`.
            let combo_str = Self::format_key_combo(combo);
            action_map.entry(action_name.to_string())
                .or_insert_with(Vec::new)
                .push(combo_str);
        }

        // `CancelSearch` has no binding of its own: cancelling is done by the
        // quit key, which closes the search instead of exiting while a search
        // is open (see the `CancelSearch | Quit` arm in the key handler).
        // Report what actually works rather than "Not configured".
        if !action_map.contains_key("Cancel Search") {
            if let Some(quit_keys) = action_map.get("Quit").cloned() {
                action_map.insert("Cancel Search".to_string(), quit_keys);
            }
        }

        // Build HTML content - use table rows with section names to avoid spacer insertion
        let mut content = String::from("<h2>⌨️ Keyboard Shortcuts</h2>\n\n");
        
        // Debug: log the action map
        tracing::debug!("Help action_map has {} entries", action_map.len());
        for (action, keys) in &action_map {
            tracing::debug!("  {} -> {:?}", action, keys);
        }
        
        // Navigation section
        content.push_str("### Navigation\n| Action | Keys |\n");
        content.push_str("|--------|------|\n");
        
        let nav_actions = [
            "Scroll Up", "Scroll Down", "Page Up", "Page Down", 
            "Go to Top", "Go to Bottom"
        ];
        for action in &nav_actions {
            content.push_str("| ");
            content.push_str(action);
            content.push_str(" | ");
            if let Some(keys) = action_map.get(*action) {
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 { content.push_str(" or "); }
                    content.push_str(&format!("`{}`", key));
                }
            } else {
                content.push_str("*Not configured*");
            }
            content.push_str(" |\n");
        }
        content.push_str("\n");
        
        // Zoom section
        content.push_str("### Zoom\n| Action | Keys |\n");
        content.push_str("|--------|------|\n");
        
        let zoom_actions = ["Zoom In", "Zoom Out", "Reset Zoom"];
        for action in &zoom_actions {
            content.push_str("| ");
            content.push_str(action);
            content.push_str(" | ");
            if let Some(keys) = action_map.get(*action) {
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 { content.push_str(" or "); }
                    content.push_str(&format!("`{}`", key));
                }
            } else {
                content.push_str("*Not configured*");
            }
            content.push_str(" |\n");
        }
        content.push_str("\n");
        
        // File Operations section
        content.push_str("### File Operations\n| Action | Keys |\n");
        content.push_str("|--------|------|\n");
        
        let file_actions = ["Next File", "Previous File", "Copy Selection"];
        for action in &file_actions {
            content.push_str("| ");
            content.push_str(action);
            content.push_str(" | ");
            if let Some(keys) = action_map.get(*action) {
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 { content.push_str(" or "); }
                    content.push_str(&format!("`{}`", key));
                }
            } else {
                content.push_str("*Not configured*");
            }
            content.push_str(" |\n");
        }
        content.push_str("\n");
        
        // Search section
        content.push_str("### Search\n| Action | Keys |\n");
        content.push_str("|--------|------|\n");

        let search_actions = [
            "Search", "Next Search Match", "Previous Search Match",
            "Cancel Search"
        ];
        for action in &search_actions {
            content.push_str("| ");
            content.push_str(action);
            content.push_str(" | ");
            if let Some(keys) = action_map.get(*action) {
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 { content.push_str(" or "); }
                    content.push_str(&format!("`{}`", key));
                }
            } else {
                content.push_str("*Not configured*");
            }
            content.push_str(" |\n");
        }
        content.push_str("\n");

        // Application section
        content.push_str("### Application\n| Action | Keys |\n");
        content.push_str("|--------|------|\n");

        let app_actions = ["Toggle Help", "Quit"];
        for action in &app_actions {
            content.push_str("| ");
            content.push_str(action);
            content.push_str(" | ");
            if let Some(keys) = action_map.get(*action) {
                for (i, key) in keys.iter().enumerate() {
                    if i > 0 { content.push_str(" or "); }
                    content.push_str(&format!("`{}`", key));
                }
            } else {
                content.push_str("*Not configured*");
            }
            content.push_str(" |\n");
        }
        content.push_str("\n");
        
        // No footer: the status bar carries the "how to close this" hint, and
        // it lists the real keybindings rather than assuming the defaults.

        tracing::debug!("Generated help content length: {} chars", content.len());
        content
    }

    /// An uncaptioned table separated from the heading above it by nothing but
    /// spacers belongs to that heading, so the spacers come out and the table
    /// sits tight underneath.
    ///
    /// Only spacers, and only when they are the whole gap. Anything else in
    /// between -- a blockquote, an intro paragraph, a second heading -- means
    /// the table is its own block and that content has to stay. This used to
    /// mark every element in the gap and scan past prose for a table
    /// arbitrarily far below, so a sheet opening with a title, a
    /// generated-from note and an intro lost all three plus its first
    /// subheading, leaving the table hard under the title.
    ///
    /// Runs per drained batch, so it only ever sees elements the interpreter
    /// has already handed over. A heading and its table landing in different
    /// batches is the reason the old version destroyed content in release
    /// builds and not in debug ones: the faster build delivered the whole
    /// document at once, and the slower one split it.
    fn tighten_tables_under_headings(elements: &mut Vec<Element>) {
        let mut to_remove: Vec<usize> = Vec::new();

        for i in 0..elements.len() {
            if !matches!(&elements[i], Element::TextBox(tb) if tb.is_header) {
                continue;
            }

            let mut gap: Vec<usize> = Vec::new();
            for j in (i + 1)..elements.len() {
                match &elements[j] {
                    Element::Spacer(_) => gap.push(j),
                    Element::Table(table) => {
                        let has_caption = table.caption.as_ref()
                            .map(|c| !c.texts.is_empty() && c.texts.iter().any(|t| !t.text.trim().is_empty()))
                            .unwrap_or(false);

                        if !has_caption {
                            to_remove.append(&mut gap);
                        }
                        break;
                    }
                    _ => break,
                }
            }
        }

        // Removal is back to front so earlier indices stay valid. A heading
        // stops the scan, so no two headings can claim the same gap and the
        // indices arrive ascending; the sort and dedup make that a property of
        // the removal rather than of the search above it.
        to_remove.sort_unstable();
        to_remove.dedup();
        for &idx in to_remove.iter().rev() {
            elements.remove(idx);
        }
    }

    pub fn position_queued_elements(
        element_queue: &Arc<Mutex<Vec<Element>>>,
        renderer: &mut Renderer,
        elements: &mut Vec<Positioned<Element>>,
    ) {
        let positioning_start = Instant::now();

        let mut elements_vec: Vec<Element> = element_queue.lock().drain(..).collect();

        Self::tighten_tables_under_headings(&mut elements_vec);

        // SECOND: Position remaining elements normally
        // We need to check ahead before consuming elements, so we'll iterate while checking first
        while !elements_vec.is_empty() {
            // Now remove and position the element
            let element = elements_vec.remove(0);
            let mut positioned_element = Positioned::new(element);
            
            // Ensure minimum 12px spacing before headers (except at document start)
            if matches!(&positioned_element.inner, Element::TextBox(tb) if tb.is_header) {
                if renderer.positioner.reserved_height > renderer.element_padding * renderer.hidpi_scale {
                    // Not at document start, ensure minimum spacing
                    let min_spacing = 12.0 * renderer.hidpi_scale * renderer.zoom;
                    let last_element_bottom = renderer.positioner.reserved_height;
                    
                    // Add spacing to ensure header has breathing room
                    renderer.positioner.reserved_height = last_element_bottom + min_spacing;
                }
            }
            
            // Position the element
            renderer
                .positioner
                .position(
                    &mut renderer.text_system,
                    &mut positioned_element,
                    renderer.zoom,
                    renderer.element_padding,
                )
                .unwrap();
            
            let element_bounds = positioned_element.bounds.as_ref().unwrap();
            let element_height = element_bounds.size.1;
            
            renderer.positioner.reserved_height += element_height;
            
            // Determine padding based on element type
            let padding = if let Element::TextBox(ref tb) = positioned_element.inner {
                if tb.is_header {
                    // Headers always get at least 2px padding after them
                    let min_header_padding = 2.0 * renderer.hidpi_scale * renderer.zoom;
                    let normal_padding = renderer.element_padding * renderer.hidpi_scale * renderer.zoom;
                    normal_padding.max(min_header_padding)
                } else {
                    // Normal padding for non-header text
                    renderer.element_padding * renderer.hidpi_scale * renderer.zoom
                }
            } else if matches!(&positioned_element.inner, Element::Table(_)) {
                // Tables always get at least 6px padding for consistency
                let min_table_padding = 6.0 * renderer.hidpi_scale * renderer.zoom;
                let normal_padding = renderer.element_padding * renderer.hidpi_scale * renderer.zoom;
                normal_padding.max(min_table_padding)
            } else {
                // Normal padding for other elements
                renderer.element_padding * renderer.hidpi_scale * renderer.zoom
            };
            
            if padding > 0.0 {
                renderer.positioner.reserved_height += padding;
            }
            
            elements.push(positioned_element);
        }

        histogram!(HistTag::Positioner).record(positioning_start.elapsed());
    }

    fn load_file(&mut self, contents: String) {
        self.current_file_content = contents.clone();
        self.element_queue.lock().clear();
        self.elements.clear();
        self.renderer.positioner.reserved_height = self.opts.element_padding * self.renderer.hidpi_scale;
        self.renderer.positioner.anchors.clear();
        self.interpreter_sender.send(contents).unwrap();
    }
    
    fn show_help(&mut self) {
        // Save current scroll position
        self.saved_scroll_y = self.renderer.scroll_y;
        
        // Clear and reset for help view
        self.help_element_queue.lock().clear();
        self.help_elements.clear();
        
        // Create help interpreter with separate element queue
        let help_interpreter = HtmlInterpreter::new(
            Arc::clone(&self.window),
            Arc::clone(&self.help_element_queue),
            self.opts.theme.clone(),
            self.renderer.surface_format,
            self.renderer.hidpi_scale,
            Arc::clone(&self.image_cache),
            self.event_loop_proxy.clone(),
            self.opts.color_scheme,
            true,   // Add spacers before headers in help
            false,  // NO spacers after headers in help
            false,  // NO spacers before tables in help
            true,   // Add spacers after tables in help
            true,   // Add spacers after paragraphs in help
            true,   // Add spacers after lists in help
        );
        
        // Use same element padding as regular documents (from opts)
        self.renderer.element_padding = self.opts.element_padding;
        
        // Load help content in separate thread
        let help_content = self.get_help_html();
        let (help_sender, help_receiver) = channel();
        std::thread::spawn(move || help_interpreter.interpret_md(help_receiver));
        help_sender.send(help_content).unwrap();
        
        // Existing matches index into the document's element list, which is
        // being swapped out for the help screen's. Drop them so nothing is
        // highlighted at the wrong place; typing re-runs the search against
        // the help content once it has loaded.
        self.clear_search_matches();

        // Reset scroll and positioning for help view
        self.renderer.scroll_y = 0.0;
        self.renderer.positioner.reserved_height = self.opts.element_padding * self.renderer.hidpi_scale;
        self.window.request_redraw();
    }

    /// Drops search results without leaving search mode. Used when the element
    /// list being searched is replaced, since matches are stored as indices
    /// into that list.
    fn clear_search_matches(&mut self) {
        self.search_matches.clear();
        self.current_match = None;
        self.search_navigating = false;
    }

    fn hide_help(&mut self) {
        // Clear help elements
        self.help_elements.clear();
        self.help_element_queue.lock().clear();
        
        // Restore document state
        // Restore padding for normal document (same as help - from opts)
        self.renderer.element_padding = self.opts.element_padding;
        // Need to recalculate the document's reserved height since it was reset for help
        let mut total_height = self.renderer.element_padding * self.renderer.hidpi_scale;
        for element in &self.elements {
            if let Some(bounds) = &element.bounds {
                total_height += bounds.size.1 + self.renderer.element_padding * self.renderer.hidpi_scale * self.renderer.zoom;
            }
        }
        self.renderer.positioner.reserved_height = total_height;
        
        // Same as in `show_help`, in the other direction. The document's
        // elements are already loaded, so an active query can be re-run
        // immediately against them.
        self.clear_search_matches();
        if self.search_active && !self.search_query.is_empty() {
            self.perform_search();
        }

        // Restore scroll position
        self.renderer.scroll_y = self.saved_scroll_y;
        self.window.request_redraw();
    }

    fn toggle_search(&mut self) {
        self.search_active = !self.search_active;
        if self.search_active {
            // Clear previous search state
            self.search_query.clear();
            self.search_matches.clear();
            self.current_match = None;
            self.search_navigating = false;
            self.search_display_text = "Type to search...".to_string();
            
            // Update window title to show search mode
            self.window.set_title(&format!("[SEARCH MODE] Type to search..."));
        } else {
            tracing::info!("Search deactivated");
            self.search_display_text.clear();
            self.search_navigating = false;
            // Restore original window title
            self.window.set_title(&utils::format_title(&self.opts.history.get_path()));
        }
        self.window.request_redraw();
    }

    fn cancel_search(&mut self) {
        if self.search_active {
            self.search_active = false;
            self.search_navigating = false;
            self.search_query.clear();
            self.search_matches.clear();
            self.current_match = None;
            self.search_display_text.clear();
            // Restore original window title
            self.window.set_title(&utils::format_title(&self.opts.history.get_path()));
            tracing::info!("Search cancelled");
            self.window.request_redraw();
        }
    }

    fn update_search_query(&mut self, c: char) {
        if !self.search_active {
            return;
        }
        
        // Reset navigation mode when typing new text
        self.search_navigating = false;
        
        // Handle backspace
        if c == '\u{8}' {
            self.search_query.pop();
        } else if !c.is_control() {
            self.search_query.push(c);
        }
        
        // Update display text and window title
        if self.search_query.is_empty() {
            self.search_display_text = "Type to search...".to_string();
            self.window.set_title("[SEARCH MODE] Type to search...");
        } else {
            self.search_display_text = self.search_query.clone();
            self.window.set_title(&format!("[SEARCH] {}", self.search_query));
        }
        
        // Perform search with updated query
        self.perform_search();
        self.window.request_redraw();
    }

    fn perform_search(&mut self) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.current_match = None;
            return;
        }
        
        self.search_matches.clear();
        // Normalize the search query for consistent Unicode handling
        let query_normalized = self.search_query.nfc().collect::<String>();
        let query_lower = query_normalized.to_lowercase();
        // Also get grapheme count for the query for accurate positioning
        let _query_grapheme_count = query_lower.graphemes(true).count();
        
        // Collect search results in a temporary vector to avoid borrow issues
        let mut temp_matches = Vec::new();
        
        // Buffer-aware search: search in the actual rendered text buffers
        let search_in_buffer = |elem_idx: usize, text_box: &TextBox, renderer: Option<&Renderer>, matches: &mut Vec<(usize, usize, usize)>| {
            // Try to get the buffer from cache using the renderer's text system
            if let Some(renderer) = renderer {
                // Get the buffer for this text box if it exists in cache
                let bounds = (f32::INFINITY, f32::INFINITY); // Use same bounds as rendering
                let zoom = renderer.zoom;
                let key = text_box.key(bounds, zoom);
                
                let cache = renderer.text_system.text_cache.lock();
                let key_hash = cache.calculate_hash(&key);
                
                // Check if buffer exists in cache
                if let Some(buffer) = cache.get(&key_hash) {
                    // Search within the buffer's actual laid-out lines
                    for (line_idx, buffer_line) in buffer.lines.iter().enumerate() {
                        let line_text = buffer_line.text();
                        // Normalize the line text for consistent Unicode handling
                        let line_normalized = line_text.nfc().collect::<String>();
                        let line_lower = line_normalized.to_lowercase();
                        
                        // Find all occurrences in this line using grapheme-aware search
                        let mut byte_start = 0;
                        while let Some(byte_pos) = line_lower[byte_start..].find(&query_lower) {
                            let absolute_byte_pos = byte_start + byte_pos;
                            
                            // Verify this is a grapheme boundary
                            let before = &line_lower[..absolute_byte_pos];
                            let is_grapheme_boundary = before.graphemes(true).count() == 
                                                       before.chars().count();
                            
                            if is_grapheme_boundary {
                                matches.push((elem_idx, line_idx, absolute_byte_pos));
                            }
                            byte_start = absolute_byte_pos + query_lower.len();
                        }
                    }
                    return; // Successfully searched in buffer
                }
            }
            
            // Fallback: if buffer not in cache, search the raw text
            // This ensures we still get results even if buffer isn't cached yet
            let mut line_idx = 0;
            let mut current_line = String::new();
            
            for text in &text_box.texts {
                // Process text by lines
                for ch in text.text.chars() {
                    if ch == '\n' {
                        // Search in the completed line
                        let line_normalized = current_line.nfc().collect::<String>();
                        let line_lower = line_normalized.to_lowercase();
                        let mut byte_start = 0;
                        while let Some(byte_pos) = line_lower[byte_start..].find(&query_lower) {
                            let absolute_byte_pos = byte_start + byte_pos;
                            
                            // Verify this is a grapheme boundary
                            let before = &line_lower[..absolute_byte_pos];
                            let is_grapheme_boundary = before.graphemes(true).count() == 
                                                       before.chars().count();
                            
                            if is_grapheme_boundary {
                                matches.push((elem_idx, line_idx, absolute_byte_pos));
                            }
                            byte_start = absolute_byte_pos + query_lower.len();
                        }
                        current_line.clear();
                        line_idx += 1;
                    } else {
                        current_line.push(ch);
                    }
                }
            }
            
            // Don't forget the last line if it doesn't end with newline
            if !current_line.is_empty() {
                let line_normalized = current_line.nfc().collect::<String>();
                let line_lower = line_normalized.to_lowercase();
                let mut byte_start = 0;
                while let Some(byte_pos) = line_lower[byte_start..].find(&query_lower) {
                    let absolute_byte_pos = byte_start + byte_pos;
                    
                    // Verify this is a grapheme boundary
                    let before = &line_lower[..absolute_byte_pos];
                    let is_grapheme_boundary = before.graphemes(true).count() == 
                                               before.chars().count();
                    
                    if is_grapheme_boundary {
                        matches.push((elem_idx, line_idx, absolute_byte_pos));
                    }
                    byte_start = absolute_byte_pos + query_lower.len();
                }
            }
        };
        
        // Search whatever is actually on screen. The help screen renders its
        // own element list in place of the document, and the highlights are
        // drawn against that same list, so searching `self.elements` here
        // would both match invisible text and misplace the highlights.
        let searched_elements: &[Positioned<Element>] = if self.help_visible {
            &self.help_elements
        } else {
            &self.elements
        };

        // Search through all elements recursively
        let mut elem_count = 0;
        for (_pos_idx, positioned_element) in searched_elements.iter().enumerate() {
            match &positioned_element.inner {
                Element::TextBox(text_box) => {
                    search_in_buffer(elem_count, text_box, Some(&self.renderer), &mut temp_matches);
                    elem_count += 1;
                }
                Element::Section(section) => {
                    for sub_element in section.elements.iter() {
                        if let Element::TextBox(text_box) = &sub_element.inner {
                            search_in_buffer(elem_count, text_box, Some(&self.renderer), &mut temp_matches);
                            elem_count += 1;
                        }
                    }
                }
                Element::Table(table) => {
                    // Search through table rows and cells
                    for row in &table.rows {
                        for text_box in row {
                            search_in_buffer(elem_count, text_box, Some(&self.renderer), &mut temp_matches);
                            elem_count += 1;
                        }
                    }
                }
                Element::Row(row) => {
                    // Handle standalone rows (shouldn't normally happen, but just in case)
                    for cell_element in &row.elements {
                        if let Element::TextBox(text_box) = &cell_element.inner {
                            search_in_buffer(elem_count, text_box, Some(&self.renderer), &mut temp_matches);
                            elem_count += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        
        // Move results to search_matches
        self.search_matches = temp_matches;
        
        // Typing only stages the query; Enter executes it. Selecting a match
        // here is what made the first Enter land on the *second* result,
        // because `next_match` would advance off this one. Leaving it unset
        // lets `next_match`'s `None => Some(0)` arm pick the first match.
        self.current_match = None;
        if !self.search_matches.is_empty() {
            self.search_display_text = format!(
                "{} ({} matches)",
                self.search_query,
                self.search_matches.len()
            );
        } else if !self.search_query.is_empty() {
            self.search_display_text = format!("{} (No matches)", self.search_query);
        }
        
        // Update window title with search results
        if self.search_active {
            self.window.set_title(&format!("[SEARCH] {}", self.search_display_text));
        }
    }

    fn next_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        
        self.current_match = match self.current_match {
            Some(idx) => Some((idx + 1) % self.search_matches.len()),
            None => Some(0),
        };
        
        self.jump_to_current_match();
        // Update display with current match
        let nav_indicator = if self.search_navigating { " [NAV]" } else { "" };
        self.search_display_text = format!("{} ({}/{}){}", 
            self.search_query, self.current_match.unwrap_or(0) + 1, self.search_matches.len(), nav_indicator);
        // Update window title
        self.window.set_title(&format!("[SEARCH] {}", self.search_display_text));
        self.window.request_redraw();
    }

    fn prev_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        
        self.current_match = match self.current_match {
            Some(0) => Some(self.search_matches.len() - 1),
            Some(idx) => Some(idx - 1),
            None => Some(self.search_matches.len() - 1),
        };
        
        self.jump_to_current_match();
        // Update display with current match
        let nav_indicator = if self.search_navigating { " [NAV]" } else { "" };
        self.search_display_text = format!("{} ({}/{}){}", 
            self.search_query, self.current_match.unwrap_or(0) + 1, self.search_matches.len(), nav_indicator);
        // Update window title
        self.window.set_title(&format!("[SEARCH] {}", self.search_display_text));
        self.window.request_redraw();
    }

    fn jump_to_current_match(&mut self) {
        if let Some(current) = self.current_match {
            if let Some(&(elem_idx, _, _)) = self.search_matches.get(current) {
                // Find the element using the same indexing as search -- which
                // means walking the same list `perform_search` walked, or the
                // index would refer to a different element entirely.
                let searched_elements: &[Positioned<Element>] = if self.help_visible {
                    &self.help_elements
                } else {
                    &self.elements
                };

                let mut current_idx = 0;
                let mut target_element = None;

                for positioned_element in searched_elements {
                    match &positioned_element.inner {
                        Element::TextBox(_) => {
                            if current_idx == elem_idx {
                                target_element = Some(positioned_element);
                                break;
                            }
                            current_idx += 1;
                        }
                        Element::Section(section) => {
                            for sub_element in &section.elements {
                                if let Element::TextBox(_) = &sub_element.inner {
                                    if current_idx == elem_idx {
                                        target_element = Some(sub_element);
                                        break;
                                    }
                                    current_idx += 1;
                                }
                            }
                            if target_element.is_some() {
                                break;
                            }
                        }
                        Element::Table(table) => {
                            // Search through table cells
                            for row in &table.rows {
                                for _text_box in row {
                                    if current_idx == elem_idx {
                                        // Can't return the TextBox directly since it's not a Positioned<Element>
                                        // This is a limitation - table cells can't be jumped to directly
                                        // TODO: Fix this by storing table cell positions separately
                                        break;
                                    }
                                    current_idx += 1;
                                }
                            }
                        }
                        Element::Row(row) => {
                            for cell in &row.elements {
                                if let Element::TextBox(_) = &cell.inner {
                                    if current_idx == elem_idx {
                                        target_element = Some(cell);
                                        break;
                                    }
                                    current_idx += 1;
                                }
                            }
                            if target_element.is_some() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                
                if let Some(element) = target_element {
                    if let Some(bounds) = &element.bounds {
                        // Scroll to make the match visible, keeping it centered if possible
                        let screen_height = self.renderer.config.height as f32;
                        let element_y = bounds.pos.1;
                        let element_height = bounds.size.1;
                        
                        // Try to center the match in the viewport
                        let ideal_scroll = element_y - (screen_height / 2.0) + (element_height / 2.0);
                        
                        // But ensure we don't scroll past the bounds
                        let min_scroll = element_y - screen_height + element_height + 50.0; // Keep 50px margin at bottom
                        let max_scroll = element_y - 50.0; // Keep 50px margin at top
                        
                        let new_scroll = ideal_scroll.max(min_scroll).min(max_scroll).max(0.0);
                        self.renderer.set_scroll_y(new_scroll);
                    }
                }
            }
        }
    }



    fn update_file(&mut self, path: &Path, contents: String) {
        self.window.set_title(&utils::format_title(path));
        self.watcher.update_file(path, contents);
        self.renderer.set_scroll_y(0.0);
    }

    pub fn run(mut self) {
        let mut pending_resize = None;
        let mut scrollbar_held = None;
        let mut mouse_down = false;
        let mut modifiers = ModifiersState::empty();
        let mut mouse_position: Point = Point::default();

        let event_loop = self.event_loop.take().unwrap();
        let event_loop_proxy = event_loop.create_proxy();
        // SAFETY: Since this takes a pointer to the winit event loop, it MUST be dropped first,
        // which is done by `move` into event loop.
        let mut clipboard = unsafe { clipboard::Clipboard::new(event_loop.raw_display_handle()) };
        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;

            match event {
                Event::UserEvent(inlyne_event) => match inlyne_event {
                    InlyneEvent::LoadedImage(src, image_data) => {
                        self.image_cache.lock().insert(src, image_data);
                        self.need_repositioning = true;
                    }
                    InlyneEvent::FileReload => match read_to_string(self.opts.history.get_path()) {
                        Ok(contents) => {
                            // Always update the content
                            self.current_file_content = contents.clone();
                            // Only reload if help isn't visible
                            if !self.help_visible {
                                self.load_file(contents);
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                "Failed reloading file at {}\nError: {}",
                                self.opts.history.get_path().display(),
                                err
                            );
                        }
                    },
                    InlyneEvent::FileChange { contents } => {
                        // Always update the content
                        self.current_file_content = contents.clone();
                        // Only reload if help isn't visible
                        if !self.help_visible {
                            self.load_file(contents);
                        }
                    }
                    InlyneEvent::Reposition => {
                        self.need_repositioning = true;
                    }
                    InlyneEvent::PositionQueue => {
                        Self::position_queued_elements(
                            &self.element_queue,
                            &mut self.renderer,
                            &mut self.elements,
                        );
                        self.window.request_redraw()
                    }
                },
                Event::RedrawRequested(_) => {
                    let redraw_start = Instant::now();
                    
                    // Position the appropriate elements based on what's visible
                    if self.help_visible {
                        Self::position_queued_elements(
                            &self.help_element_queue,
                            &mut self.renderer,
                            &mut self.help_elements,
                        );
                    } else {
                        Self::position_queued_elements(
                            &self.element_queue,
                            &mut self.renderer,
                            &mut self.elements,
                        );
                    }
                    
                    self.renderer.set_scroll_y(self.renderer.scroll_y);
                    
                    // Render the appropriate elements
                    let elements_to_render = if self.help_visible {
                        &mut self.help_elements
                    } else {
                        &mut self.elements
                    };
                    
                    // Pass search state to redraw for integrated rendering
                    let status_text = if !self.opts.status_bar {
                        String::new()
                    } else if let Some(err) = &self.opts.config_error {
                        // Outranks the hints: the keys being advertised are the
                        // defaults, not the user's, and saying so is the only
                        // on-screen clue that the config was dropped.
                        err.clone()
                    } else if self.search_active {
                        // Search takes priority over the help screen: searching
                        // works there too, so the search keys are what matters.
                        // Once the query has been run, match-stepping becomes
                        // live and the hints expand to include it.
                        let mode = if self.search_navigating {
                            &self.status_bar_hints_search_nav
                        } else {
                            &self.status_bar_hints_search
                        };
                        // Page movement keeps working mid-query, so keep
                        // advertising it alongside the search keys.
                        if self.status_bar_hints_search_extra.is_empty() {
                            mode.clone()
                        } else {
                            format!(
                                "{}{}{}",
                                mode,
                                crate::renderer::STATUS_BAR_SEPARATOR,
                                self.status_bar_hints_search_extra
                            )
                        }
                    } else if self.help_visible {
                        // Links on the page behind the help screen aren't
                        // reachable, so only the help hints are relevant.
                        self.status_bar_hints_help.clone()
                    } else {
                        match &self.hovered_link {
                            Some(link) => link.clone(),
                            None => self.status_bar_hints.clone(),
                        }
                    };
                    self.renderer
                        .redraw(
                            elements_to_render,
                            &mut self.selection,
                            self.search_active,
                            &self.search_matches,
                            self.current_match,
                            &self.search_query,
                            self.search_matches.len(),
                            &status_text,
                            self.opts.config_error.is_some(),
                        )
                        .context("Renderer failed to redraw the screen")
                        .unwrap();

                    histogram!(HistTag::Redraw).record(redraw_start.elapsed());
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::Resized(size) => pending_resize = Some(size),
                    WindowEvent::CloseRequested => *control_flow = ControlFlow::Exit,
                    WindowEvent::MouseWheel { delta, .. } => match delta {
                        MouseScrollDelta::PixelDelta(pos) => {
                            Self::scroll_pixels(&mut self.renderer, &self.window, pos.y as f32)
                        }
                        MouseScrollDelta::LineDelta(_, y_delta) => Self::scroll_lines(
                            &mut self.renderer,
                            &self.window,
                            self.lines_to_scroll,
                            y_delta,
                        ),
                    },
                    WindowEvent::CursorMoved { position, .. } => {
                        let screen_size = self.renderer.screen_size();
                        let loc = (
                            position.x as f32,
                            position.y as f32 + self.renderer.scroll_y,
                        );

                        // Resolve the cursor shape and the hovered link in one
                        // pass; the link is cloned out so the borrow on
                        // `self.elements` ends with the match.
                        let (cursor_icon, hovered_link) = match Self::find_hoverable(
                            &mut self.renderer.text_system,
                            &mut self.renderer.positioner.taffy,
                            &self.elements,
                            loc,
                            screen_size,
                            self.renderer.zoom,
                        ) {
                            Some(Hoverable::Image(Image { is_link: None, .. })) => {
                                (CursorIcon::Default, None)
                            }
                            Some(Hoverable::Text(Text { link: None, .. })) => {
                                (CursorIcon::Text, None)
                            }
                            Some(Hoverable::Image(Image { is_link: Some(link), .. }))
                            | Some(Hoverable::Text(Text { link: Some(link), .. })) => {
                                (CursorIcon::Hand, Some(link.clone()))
                            }
                            Some(_summary) => (CursorIcon::Hand, None),
                            None => (CursorIcon::Default, None),
                        };
                        self.window.set_cursor_icon(cursor_icon);

                        // CursorMoved fires continuously, so only repaint when
                        // the link under the cursor actually changes -- and not
                        // at all when there is no status bar to show it in.
                        if self.opts.status_bar && self.hovered_link != hovered_link {
                            self.hovered_link = hovered_link;
                            self.window.request_redraw();
                        }

                        let scrollbar_width = self.renderer.scrollbar_width();
                        if scrollbar_held.is_some()
                            || (Rect::new(
                                (screen_size.0 - scrollbar_width, 0.),
                                (scrollbar_width, screen_size.1),
                            )
                            .contains(position.into())
                                && mouse_down)
                        {
                            let scrollbar_height = self.renderer.scrollbar_height();
                            if scrollbar_held.is_none() {
                                if Rect::new(
                                    (
                                        screen_size.0 - scrollbar_width,
                                        ((self.renderer.scroll_y
                                            / self.renderer.positioner.reserved_height)
                                            * screen_size.1),
                                    ),
                                    (scrollbar_width, scrollbar_height),
                                )
                                .contains(position.into())
                                {
                                    // If we click in the bounds of the scrollbar, maintain the difference between the
                                    // center of the scrollbar and the mouse
                                    scrollbar_held = Some(
                                        position.y as f32
                                            - (((self.renderer.scroll_y
                                                / self.renderer.positioner.reserved_height)
                                                * screen_size.1)
                                                + scrollbar_height / 2.),
                                    );
                                } else {
                                    scrollbar_held = Some(0.);
                                }
                            }

                            let pos_y = if let Some(diff) = scrollbar_held {
                                position.y as f32 - diff
                            } else {
                                position.y as f32
                            };
                            let target_scroll = ((pos_y - scrollbar_height / 2.) / screen_size.1)
                                * self.renderer.positioner.reserved_height;
                            self.renderer.set_scroll_y(target_scroll);
                            self.window.request_redraw();
                        } else if mouse_down && self.selection.handle_drag(loc) {
                            self.window.request_redraw();
                        }
                        mouse_position = loc;
                    }
                    WindowEvent::MouseInput {
                        state,
                        button: MouseButton::Left,
                        ..
                    } => match state {
                        ElementState::Pressed => {
                            // Try to click a link
                            let screen_size = self.renderer.screen_size();

                            let y = mouse_position.1 - self.renderer.scroll_y;
                            let scrollbar_width = self.renderer.scrollbar_width();
                            if Rect::new(
                                (screen_size.0 - scrollbar_width, 0.),
                                (scrollbar_width, screen_size.1),
                            ).contains((mouse_position.0, y)) {
                                let scrollbar_height = self.renderer.scrollbar_height();

                                let target_scroll = ((y - scrollbar_height / 2.) / screen_size.1)
                                    * self.renderer.positioner.reserved_height;

                                self.renderer.set_scroll_y(target_scroll);
                                self.window.request_redraw();
                            }

                            if let Some(hoverable) = Self::find_hoverable(
                                &mut self.renderer.text_system,
                                &mut self.renderer.positioner.taffy,
                                &self.elements,
                                mouse_position,
                                screen_size,
                                self.renderer.zoom,
                            ) {
                                match hoverable {
                                    Hoverable::Image(Image { is_link: Some(link), .. }) |
                                    Hoverable::Text(Text { link: Some(link), .. }) => {
                                        let path = PathBuf::from(link);

                                        if  path.extension().is_some_and(|ext| ext == "md")
                                            && !path.to_str().is_some_and(|s| s.starts_with("http")) {
                                            // Open them in a new window, akin to what a browser does
                                            if modifiers.shift() {
                                                std::thread::spawn(move || {
                                                    Command::new(
                                                        std::env::current_exe()
                                                            .unwrap_or_else(|_| "inlyne".into()),
                                                    )
                                                        .args(Opts::program_args(&path))
                                                        .spawn()
                                                        .expect("Couldn't spawn inlyne instance")
                                                        .wait()
                                                        .expect("Failed waiting on child");
                                                });
                                            } else {
                                                match read_to_string(&path) {
                                                    Ok(contents) => {
                                                        self.update_file(&path, contents);
                                                        self.opts.history.make_next(path);
                                                    }
                                                    Err(err) => {
                                                        tracing::warn!(
                                                        "Failed loading markdown file at {}\nError: {}",
                                                        path.display(),
                                                        err,
                                                    );
                                                    }
                                                }
                                            }
                                        } else if let Some(anchor_pos) =
                                            self.renderer.positioner.anchors.get(&link.to_lowercase())
                                        {
                                            self.renderer.set_scroll_y(*anchor_pos);
                                            self.window.request_redraw();
                                            self.window.set_cursor_icon(CursorIcon::Default);
                                        } else if let Err(e) = open::that(link) {
                                            tracing::error!("Could not open link: {e} from {:?}", std::env::current_dir())
                                        }
                                    },
                                    Hoverable::Summary(summary) => {
                                        let mut hidden = summary.hidden.borrow_mut();
                                        *hidden = !*hidden;
                                        event_loop_proxy
                                            .send_event(InlyneEvent::Reposition)
                                            .unwrap();
                                        self.selection.add_position(mouse_position);
                                    },
                                    _ => {
                                        self.selection.add_position(mouse_position);
                                        self.window.request_redraw();
                                    }
                                };
                            } else {
                                self.selection.add_position(mouse_position);
                                self.window.request_redraw()
                            }
                            mouse_down = true;
                        }
                        ElementState::Released => {
                            scrollbar_held = None;
                            mouse_down = false;
                        }
                    },
                    WindowEvent::ModifiersChanged(new_state) => modifiers = new_state,
                    WindowEvent::ReceivedCharacter(c) => {
                        if self.search_active {
                            // Only process text input in ReceivedCharacter event
                            // All navigation and control keys are handled in KeyboardInput event
                            
                            // Skip control characters and special keys
                            if c.is_control() && c != '\u{8}' {
                                // Skip all control characters except backspace
                                return;
                            }

                            // The matching `KeyboardInput` already used this key
                            // to move between matches, so don't also type it.
                            // Replaces a hardcoded 'n'/'N' check so that
                            // whatever the user bound is handled.
                            if self.suppress_next_char {
                                self.suppress_next_char = false;
                                return;
                            }

                            // Process regular text input
                            self.update_search_query(c);
                        } else if c == '?' {
                            // Handle '?' character for help when not searching
                            if !self.help_visible {
                                self.help_visible = true;
                                self.show_help();
                            } else {
                                self.help_visible = false;
                                self.hide_help();
                            }
                        }
                    }
                    WindowEvent::KeyboardInput {
                        input:
                            KeyboardInput {
                                state: ElementState::Pressed,
                                virtual_keycode,
                                scancode,
                                ..
                            },
                        ..
                    } => {
                        // Each press starts with no suppression; it is only set
                        // if this press is consumed as search navigation.
                        self.suppress_next_char = false;

                        // Enter and Tab are built-in fallbacks for stepping
                        // through matches. They emit no text, so they are
                        // never ambiguous against the query, and keeping them
                        // here means they work even when a config replaces
                        // `base` without rebinding them. Everything else --
                        // including custom next/prev keys -- goes through the
                        // keybinding path below.
                        //
                        // The old version also hardcoded 'n'/'Shift+n'; those
                        // are character keys and are now handled there, where
                        // the text-vs-navigation rule applies.
                        let search_nav_handled = if self.search_active
                            && !self.search_matches.is_empty()
                        {
                            let step = match virtual_keycode {
                                Some(VirtualKeyCode::Return | VirtualKeyCode::Tab) => {
                                    if modifiers.is_empty() {
                                        Some(true)
                                    } else if modifiers == ModifiersState::SHIFT {
                                        Some(false)
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            };

                            match step {
                                Some(forwards) => {
                                    // Stepping means the query is finished, so
                                    // enter navigation mode; that is what makes
                                    // character keys like `j` start working.
                                    self.search_navigating = true;
                                    if forwards {
                                        self.next_match();
                                    } else {
                                        self.prev_match();
                                    }
                                    true
                                }
                                None => false,
                            }
                        } else {
                            false
                        };

                        if !search_nav_handled {
                            let key = Key::new(virtual_keycode, scancode);
                            let modified_key = ModifiedKey(key, modifiers);
                            
                            // Process keyboard input for other actions
                            let action_result = self.keycombos.munch(modified_key);
                            
                            if let Some(action) = action_result {
                                // When search is active, only allow search-related actions
                                if self.search_active {
                                    // While a query is being typed, a key that
                                    // emits a character belongs to the query,
                                    // not to its action -- otherwise `q`
                                    // (Quit), `/` (Search) and `y` (Copy) would
                                    // fire instead of being typed, making
                                    // "query" unsearchable.
                                    //
                                    // The exception is stepping through matches
                                    // once Enter has entered navigation mode,
                                    // where match-stepping takes over the keys.
                                    let types_text =
                                        Self::key_produces_text(virtual_keycode, modifiers);
                                    let navigating_matches = self.search_navigating
                                        && matches!(
                                            action,
                                            Action::NextMatch | Action::PrevMatch
                                        );

                                    if types_text && !navigating_matches {
                                        // Fall through; `ReceivedCharacter`
                                        // types it into the query.
                                        tracing::debug!(
                                            "Action {:?} left to text input while searching",
                                            action
                                        );
                                    } else {
                                        match action {
                                            Action::Search => {
                                                // Toggle search off
                                                self.toggle_search();
                                            }
                                            Action::CancelSearch | Action::Quit => {
                                                // Cancel search
                                                self.cancel_search();
                                            }
                                            Action::NextMatch | Action::PrevMatch => {
                                                if !self.search_matches.is_empty() {
                                                    // Stepping to a match means
                                                    // the query is finished, so
                                                    // enter navigation mode --
                                                    // this is what lets
                                                    // character keys like `j`
                                                    // start working.
                                                    self.search_navigating = true;
                                                    if action == Action::NextMatch {
                                                        self.next_match();
                                                    } else {
                                                        self.prev_match();
                                                    }
                                                }
                                                // Consumed as navigation, so
                                                // don't also type the key.
                                                self.suppress_next_char = true;
                                            }
                                            Action::Copy => {
                                                // Allow copying while searching
                                                clipboard.set_contents(self.selection.text.trim().to_owned())
                                            }
                                            // Moving around the page does not
                                            // disturb the query, and only keys
                                            // that emit no text reach this arm,
                                            // so there is nothing to conflict
                                            // with. Keeping these live is also
                                            // what lets the status bar list
                                            // them honestly while searching.
                                            Action::Scroll(direction) => {
                                                let lines = match direction {
                                                    VertDirection::Up => 1.0,
                                                    VertDirection::Down => -1.0,
                                                };
                                                Self::scroll_lines(
                                                    &mut self.renderer,
                                                    &self.window,
                                                    self.lines_to_scroll,
                                                    lines,
                                                );
                                            }
                                            Action::Page(direction) => {
                                                let embiggen = match direction {
                                                    VertDirection::Up => -1.0,
                                                    VertDirection::Down => 1.0,
                                                };
                                                let scroll = self.renderer.scroll_y
                                                    + embiggen * self.renderer.screen_height();
                                                self.renderer.set_scroll_y(scroll);
                                                self.window.request_redraw();
                                            }
                                            Action::ToEdge(direction) => {
                                                let scroll = match direction {
                                                    VertDirection::Up => 0.0,
                                                    VertDirection::Down => f32::INFINITY,
                                                };
                                                self.renderer.set_scroll_y(scroll);
                                                self.window.request_redraw();
                                            }
                                            _ => {
                                                // Ignore all other actions while search is active
                                                tracing::debug!("Action {:?} ignored while search is active", action);
                                            }
                                        }
                                    }
                                } else {
                                    // Normal action processing when search is not active
                                    match action {
                                    Action::ToEdge(direction) => {
                                        let scroll = match direction {
                                            VertDirection::Up => 0.0,
                                            VertDirection::Down => f32::INFINITY,
                                        };
                                        self.renderer.set_scroll_y(scroll);
                                        self.window.request_redraw();
                                    }
                                    Action::Scroll(direction) => {
                                        let lines = match direction {
                                            VertDirection::Up => 1.0,
                                            VertDirection::Down => -1.0,
                                        };

                                        Self::scroll_lines(
                                            &mut self.renderer,
                                            &self.window,
                                            self.lines_to_scroll,
                                            lines,
                                        )
                                    }
                                    Action::Page(direction) => {
                                        // Move 90% of current page height
                                        let scroll_amount = self.renderer.config.height as f32 * 0.9;
                                        let scroll_with_direction = match direction {
                                            VertDirection::Up => scroll_amount,
                                            VertDirection::Down => -scroll_amount,
                                        };

                                        Self::scroll_pixels(
                                            &mut self.renderer,
                                            &self.window,
                                            scroll_with_direction,
                                        );
                                    }
                                    Action::Zoom(zoom_action) => {
                                        let zoom = match zoom_action {
                                            Zoom::In => self.renderer.zoom * 1.1,
                                            Zoom::Out => self.renderer.zoom * 0.9,
                                            Zoom::Reset => 1.0,
                                        };

                                        self.renderer.zoom = zoom;
                                        let old_reserved = self.renderer.positioner.reserved_height;
                                        self.renderer.reposition(&mut self.elements).unwrap();
                                        let new_reserved = self.renderer.positioner.reserved_height;
                                        self.renderer.set_scroll_y(
                                            self.renderer.scroll_y * (new_reserved / old_reserved),
                                        );
                                        self.window.request_redraw();
                                    }
                                    Action::Copy => clipboard
                                        .set_contents(self.selection.text.trim().to_owned()),
                                    Action::Help => {
                                        if !self.help_visible {
                                            self.help_visible = true;
                                            self.show_help();
                                        } else {
                                            self.help_visible = false;
                                            self.hide_help();
                                        }
                                    }
                                    Action::Search => {
                                        self.toggle_search();
                                        // Opening search flips `search_active`
                                        // before this press's `ReceivedCharacter`
                                        // arrives, so a character key like `/`
                                        // would otherwise be typed straight into
                                        // the query it just opened.
                                        self.suppress_next_char = true;
                                    }
                                    Action::CancelSearch => {
                                        // This action doesn't do anything when search is not active
                                        tracing::debug!("CancelSearch action ignored - search not active");
                                    }
                                    Action::NextMatch | Action::PrevMatch => {
                                        // These actions don't do anything when search is not active
                                        tracing::debug!("Match navigation action ignored - search not active");
                                    }
                                    Action::Quit => {
                                        if self.help_visible {
                                            self.help_visible = false;
                                            self.hide_help();
                                        } else {
                                            *control_flow = ControlFlow::Exit;
                                        }
                                    }
                                    Action::History(hist_dir) => {
                                        let changed_path = match hist_dir {
                                            HistDirection::Next => self.opts.history.next(),
                                            HistDirection::Prev => self.opts.history.previous(),
                                        }.map(ToOwned::to_owned);
                                        let Some(file_path) = changed_path else {
                                            return;
                                        };
                                        match read_to_string(&file_path) {
                                            Ok(contents) => {
                                                self.update_file(&file_path, contents);
                                                let parent = file_path.parent().expect("File should have parent directory");
                                                std::env::set_current_dir(parent).expect("Could not set current directory.");
                                            }
                                            Err(err) => {
                                                tracing::warn!(
                                                    "Failed loading markdown file at {}\nError: {}",
                                                    file_path.display(),
                                                    err,
                                                );
                                            }
                                        }
                                    }
                                    } // End of match action (non-search mode)
                                }
                            } // End of if let Some(action) = action_result
                        } // End of if !search_nav_handled
                    }
                    _ => {}
                },
                Event::MainEventsCleared => {
                    // We lazily store the size and only reposition elements and request a redraw when
                    // we receive a `MainEventsCleared`.  This prevents us from clogging up the queue
                    // with a bunch of costly resizes. (https://github.com/Inlyne-Project/inlyne/issues/25)
                    if let Some(size) = pending_resize.take() {
                        if size.width > 0 && size.height > 0 {
                            self.renderer.config.width = size.width;
                            self.renderer.config.height = size.height;
                            self.renderer.positioner.screen_size = size.into();
                            self.renderer
                                .surface
                                .configure(&self.renderer.device, &self.renderer.config);
                            let old_reserved = self.renderer.positioner.reserved_height;
                            if self.help_visible {
                                self.renderer.reposition(&mut self.help_elements).unwrap();
                            } else {
                                self.renderer.reposition(&mut self.elements).unwrap();
                            }
                            let new_reserved = self.renderer.positioner.reserved_height;
                            self.renderer.set_scroll_y(
                                self.renderer.scroll_y * (new_reserved / old_reserved),
                            );
                            self.window.request_redraw();
                        }
                    }

                    if self.need_repositioning {
                        if self.help_visible {
                            self.renderer.reposition(&mut self.help_elements).unwrap();
                        } else {
                            self.renderer.reposition(&mut self.elements).unwrap();
                        }
                        self.window.request_redraw();
                        self.need_repositioning = false;
                    }
                }
                _ => {}
            }
        });
    }

    fn scroll_lines(
        renderer: &mut Renderer,
        window: &Window,
        lines_to_scroll: f32,
        num_lines: f32,
    ) {
        let num_pixels = num_lines * 16.0 * lines_to_scroll * renderer.hidpi_scale * renderer.zoom;
        Self::scroll_pixels(renderer, window, num_pixels);
    }

    fn scroll_pixels(renderer: &mut Renderer, window: &Window, num_pixels: f32) {
        renderer.set_scroll_y(renderer.scroll_y - num_pixels);
        window.request_redraw();
    }

    fn find_hoverable<'a>(
        text_system: &mut TextSystem,
        taffy: &mut Taffy,
        elements: &'a [Positioned<Element>],
        loc: Point,
        screen_size: Size,
        zoom: f32,
    ) -> Option<Hoverable<'a>> {
        let screen_pos = |screen_size: Size, bounds_offset: f32| {
            (
                screen_size.0 - bounds_offset - DEFAULT_MARGIN,
                screen_size.1,
            )
        };

        elements
            .iter()
            .find(|&e| e.contains(loc) && !matches!(e.inner, Element::Spacer(_)))
            .and_then(|element| match &element.inner {
                Element::TextBox(text_box) => {
                    let bounds = element.bounds.as_ref().unwrap();
                    text_box
                        .find_hoverable(
                            text_system,
                            loc,
                            bounds.pos,
                            screen_pos(screen_size, bounds.pos.0),
                            zoom,
                        )
                        .map(Hoverable::Text)
                }
                Element::Table(table) => {
                    let bounds = element.bounds.as_ref().unwrap();
                    table
                        .find_hoverable(
                            text_system,
                            taffy,
                            loc,
                            bounds.pos,
                            screen_pos(screen_size, bounds.pos.0),
                            zoom,
                        )
                        .map(Hoverable::Text)
                }
                Element::Image(image) => Some(Hoverable::Image(image)),
                Element::Spacer(_) => unreachable!("Spacers are filtered"),
                Element::Row(row) => {
                    Self::find_hoverable(text_system, taffy, &row.elements, loc, screen_size, zoom)
                }
                Element::Section(section) => {
                    if let Some(ref summary) = *section.summary {
                        if let Some(ref bounds) = summary.bounds {
                            if bounds.contains(loc) {
                                return Some(Hoverable::Summary(section));
                            }
                        }
                    }
                    if !*section.hidden.borrow() {
                        Self::find_hoverable(
                            text_system,
                            taffy,
                            &section.elements,
                            loc,
                            screen_size,
                            zoom,
                        )
                    } else {
                        None
                    }
                }
            })
    }
}

/// Condenses a config load failure into one line for the status bar.
///
/// `toml` reports errors across several lines -- a location, an excerpt of the
/// offending line, a caret, then the reason. The location and the reason are
/// the parts worth showing; the excerpt is already on screen in the user's
/// editor. The full error still goes to the log.
fn summarize_config_error(err: &anyhow::Error) -> String {
    let rendered = format!("{err:#}");
    let mut lines = rendered.lines().map(str::trim).filter(|l| !l.is_empty());

    let Some(first) = lines.next() else {
        return "config failed to load, using defaults".to_string();
    };
    // Drop the excerpt/caret lines that `toml` draws with `|`.
    let reason = rendered
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('|') && !l.contains(" | "))
        .next_back()
        .filter(|last| *last != first);

    let detail = match reason {
        Some(reason) => format!("{first}: {reason}"),
        None => first.to_string(),
    };
    format!("config error -- using defaults -- {detail}")
}

fn main() -> anyhow::Result<()> {
    setup_panic!();

    let env_filter = tracing_subscriber::EnvFilter::builder()
        .with_default_directive("inlyne=info".parse()?)
        .with_env_var("INLYNE_LOG")
        .from_env()?;
    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().compact())
        .init();

    let command = Cli::parse().into_commands();

    match command {
        Commands::View(view) => {
            // An explicitly requested config is a hard error; the default one
            // falls back so a single typo cannot stop inlyne from opening the
            // very file that explains how to fix it. The failure is recorded
            // and shown in the status bar, because a GUI launch has no
            // terminal for the log warning to land in.
            let mut config_error = None;
            let config = match &view.config {
                Some(config_path) => Config::load_from_file(config_path)?,
                None => Config::load_from_system().unwrap_or_else(|err| {
                    tracing::warn!(
                        "Failed reading config file. Falling back to defaults. Error: {}",
                        err
                    );
                    config_error = Some(summarize_config_error(&err));
                    Config::default()
                }),
            };
            let mut opts = Opts::parse_and_load_from(view, config)?;
            opts.config_error = config_error;

            if let Some(exporter) = &opts.metrics {
                match exporter {
                    MetricsExporter::Log => {
                        let recorder = metrics::LogRecorder::default();
                        metrics::set_global_recorder(recorder)
                            .expect("Failed setting metrics recorder");
                    }
                    #[cfg(inlyne_tcp_metrics)]
                    MetricsExporter::Tcp => metrics_exporter_tcp::TcpBuilder::new()
                        .install()
                        .expect("Failed to install TCP metrics server"),
                };
            }

            for tag in HistTag::iter() {
                tag.set_global_description();
            }

            let inlyne = Inlyne::new(opts)?;
            inlyne.run();
        }
        Commands::Config(ConfigCmd::Open) => {
            let config_path = dirs::config_dir()
                .context("Failed to find the configuration directory")?
                .join("inlyne")
                .join("inlyne.toml");

            let config = std::fs::read_to_string(&config_path)
                .unwrap_or_else(|_| Config::default_config().to_string());

            let new_config = edit::edit_with_builder(
                &config,
                edit::Builder::new()
                    .prefix("inlyne_temp")
                    .suffix(".toml")
                    .disable_cleanup(true),
            )?;

            _ = Config::load_from_str(&new_config)?;

            std::fs::write(config_path, new_config)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod status_bar_tests {
    use super::*;
    use crate::opts::KeybindingsSection;

    fn hints_from_toml(toml: &str) -> String {
        let section: KeybindingsSection = toml::from_str(toml).unwrap();
        let keybindings: keybindings::Keybindings = section.into();
        Inlyne::build_status_bar_hints(&keybindings)
    }

    fn help_hints_from_toml(toml: &str) -> String {
        let section: KeybindingsSection = toml::from_str(toml).unwrap();
        let keybindings: keybindings::Keybindings = section.into();
        Inlyne::build_help_status_bar_hints(&keybindings)
    }

    /// Custom bindings must show up alongside the defaults, not be hidden
    /// behind whichever happens to be listed first.
    #[test]
    fn lists_every_binding_for_an_action() {
        let hints = hints_from_toml(
            r#"
            base = [
                ["ScrollUp", "Up"],
                ["ScrollDown", "Down"],
                ["ScrollUp", "i"],
                ["ScrollDown", "e"],
                ["Quit", "Escape"],
            ]
            "#,
        );
        // Both pairs are advertised, each up key beside its matching down key.
        assert!(hints.contains("Up/Down"), "missing default pair: {hints}");
        assert!(hints.contains("i/e"), "missing custom pair: {hints}");
        assert!(hints.contains("scroll"), "missing label: {hints}");
    }

    #[test]
    fn single_actions_list_all_their_keys() {
        let hints = hints_from_toml(
            r#"
            base = [
                ["Help", "h"],
                ["Help", { key = "/", mod = ["Shift"] }],
                ["Quit", "Escape"],
            ]
            "#,
        );
        assert!(hints.contains("h, ? help"), "{hints}");
    }

    /// The help screen closes on both the help key and the quit key, and does
    /// not quit, so it must not advertise "quit".
    #[test]
    fn help_hints_describe_closing_not_quitting() {
        let hints = help_hints_from_toml(
            r#"
            base = [
                ["ScrollUp", "Up"],
                ["ScrollDown", "Down"],
                ["Help", { key = "/", mod = ["Shift"] }],
                ["Quit", "Escape"],
            ]
            "#,
        );
        assert!(hints.contains("close help"), "{hints}");
        assert!(hints.contains("?"), "{hints}");
        assert!(hints.contains("Esc"), "{hints}");
        assert!(!hints.contains("quit"), "should not offer quit: {hints}");
    }

    /// An unbalanced pair must not silently drop the surplus key.
    #[test]
    fn unpaired_directions_still_listed() {
        let hints = hints_from_toml(
            r#"
            base = [
                ["ScrollUp", "Up"],
                ["ScrollDown", "Down"],
                ["ScrollUp", "i"],
            ]
            "#,
        );
        assert!(hints.contains("Up/Down"), "{hints}");
        assert!(hints.contains('i'), "surplus key dropped: {hints}");
    }
}



#[cfg(test)]
mod help_coverage_tests {
    use super::*;

    /// Every action the help screen lists must resolve to at least one key
    /// under the stock keybindings, otherwise the help shows
    /// "*Not configured*" for something the user has no way to discover.
    #[test]
    fn every_help_action_has_a_default_binding() {
        let kb = keybindings::Keybindings::default();
        let listed = [
            (Action::Scroll(VertDirection::Up), "Scroll Up"),
            (Action::Scroll(VertDirection::Down), "Scroll Down"),
            (Action::Page(VertDirection::Up), "Page Up"),
            (Action::Page(VertDirection::Down), "Page Down"),
            (Action::ToEdge(VertDirection::Up), "Go to Top"),
            (Action::ToEdge(VertDirection::Down), "Go to Bottom"),
            (Action::Zoom(Zoom::In), "Zoom In"),
            (Action::Zoom(Zoom::Out), "Zoom Out"),
            (Action::Zoom(Zoom::Reset), "Reset Zoom"),
            (Action::History(HistDirection::Next), "Next File"),
            (Action::History(HistDirection::Prev), "Previous File"),
            (Action::Copy, "Copy Selection"),
            (Action::Search, "Search"),
            (Action::NextMatch, "Next Search Match"),
            (Action::PrevMatch, "Previous Search Match"),
            (Action::Help, "Toggle Help"),
            (Action::Quit, "Quit"),
        ];

        let unbound: Vec<&str> = listed
            .iter()
            .filter(|(action, _)| Inlyne::keys_for(&kb, *action).is_empty())
            .map(|(_, name)| *name)
            .collect();

        assert!(
            unbound.is_empty(),
            "help screen would show \"Not configured\" for: {unbound:?}"
        );
    }

    /// `CancelSearch` is deliberately unbound -- the quit key cancels an open
    /// search -- so the help must borrow the quit keys rather than claim it is
    /// unconfigured.
    #[test]
    fn cancel_search_is_covered_by_the_quit_key() {
        let kb = keybindings::Keybindings::default();
        assert!(
            Inlyne::keys_for(&kb, Action::CancelSearch).is_empty(),
            "if CancelSearch gained a binding, the help fallback can be dropped"
        );
        assert!(
            !Inlyne::keys_for(&kb, Action::Quit).is_empty(),
            "the fallback relies on Quit being bound"
        );
    }
}

#[cfg(test)]
mod search_mode_tests {
    use super::*;

    /// Keys that never emit a character must be able to navigate matches
    /// straight away; keys that do emit one must wait for navigation mode,
    /// otherwise they would be stolen from the query being typed.
    #[test]
    fn only_character_keys_are_ambiguous_during_search() {
        let none = ModifiersState::empty();
        let shift = ModifiersState::SHIFT;
        let ctrl = ModifiersState::CTRL;

        // Non-character keys -- safe to navigate immediately.
        for (key, mods) in [
            (VirtualKeyCode::Return, shift),
            (VirtualKeyCode::Return, none),
            (VirtualKeyCode::Tab, shift),
            (VirtualKeyCode::F3, none),
            (VirtualKeyCode::G, ctrl),
        ] {
            assert!(
                !Inlyne::key_produces_text(Some(key), mods),
                "{key:?}+{mods:?} should not be treated as text input"
            );
        }

        // Character keys -- must not steal input mid-query.
        for (key, mods) in [
            (VirtualKeyCode::J, none),
            (VirtualKeyCode::J, shift),
            (VirtualKeyCode::N, none),
            (VirtualKeyCode::Slash, none),
        ] {
            assert!(
                Inlyne::key_produces_text(Some(key), mods),
                "{key:?}+{mods:?} should be treated as text input"
            );
        }
    }

    /// While typing, the bar explains how to run the query and how to leave --
    /// not the page keys, which are inert, and not match-stepping, which is
    /// not live yet.
    #[test]
    fn typing_hints_describe_running_the_search() {
        let kb = keybindings::Keybindings::default();
        let hints = Inlyne::build_search_status_bar_hints(&kb);
        assert!(hints.contains("run search"), "{hints}");
        assert!(hints.contains("exit search"), "{hints}");
        assert!(!hints.contains("scroll"), "page keys are inert here: {hints}");
        // Character keys are swallowed by the query, so they must not be
        // advertised as working mid-search.
        assert!(!hints.contains(", q "), "q types into the query: {hints}");
        assert!(
            !hints.split("exit").next().unwrap().contains("n,"),
            "n only works after the search is run: {hints}"
        );
    }

    /// Once the search has run, match-stepping is live, so those keys -- including
    /// character ones like `n` -- are advertised.
    #[test]
    fn nav_hints_describe_stepping_through_matches() {
        let kb = keybindings::Keybindings::default();
        let hints = Inlyne::build_search_nav_status_bar_hints(&kb);
        assert!(hints.contains("next match"), "{hints}");
        assert!(hints.contains("prev match"), "{hints}");
        assert!(hints.contains("exit search"), "{hints}");
        // `n` is bound to NextMatch and works in this sub-state.
        assert!(hints.contains('n'), "{hints}");
        // Exiting is still never taken over by stepping, so `q` stays out.
        assert!(!hints.contains(", q "), "q types into the query: {hints}");
    }

    /// `/` opens search, matching vim/less/browsers. `?` is Shift+/ and stays
    /// Help, so the two must not collide.
    #[test]
    fn slash_opens_search_and_does_not_shadow_help() {
        let kb = keybindings::Keybindings::default();
        let search = Inlyne::keys_for(&kb, Action::Search);
        assert!(search.iter().any(|k| k == "/"), "{search:?}");
        let help = Inlyne::keys_for(&kb, Action::Help);
        assert!(help.iter().any(|k| k == "?"), "{help:?}");
    }

    #[test]
    fn shift_enter_is_bound_to_previous_match() {
        let kb = keybindings::Keybindings::default();
        let prev = Inlyne::keys_for(&kb, Action::PrevMatch);
        // keys_for caps at MAX_HINT_ALTERNATIVES, so check the raw list.
        let all: Vec<String> = kb
            .iter()
            .filter(|(a, _)| *a == Action::PrevMatch)
            .map(|(_, c)| Inlyne::format_key_combo(c))
            .collect();
        assert!(
            all.iter().any(|k| k == "Shift+Enter"),
            "expected Shift+Enter among {all:?} (capped view: {prev:?})"
        );
    }
}






#[cfg(test)]
mod search_extra_hint_tests {
    use super::*;

    /// Page movement stays live during a query, but only via keys that emit no
    /// text -- `k`/`j` are typed into the query, so advertising them would be
    /// a lie.
    #[test]
    fn extra_hints_only_list_keys_that_survive_text_entry() {
        let kb = keybindings::Keybindings::default();
        let extra = Inlyne::build_search_extra_hints(&kb);

        assert!(extra.contains("Up/Down"), "{extra}");
        assert!(extra.contains("PgUp/PgDn"), "{extra}");
        assert!(extra.contains("scroll"), "{extra}");
        assert!(extra.contains("page"), "{extra}");
        // The vim-style character bindings must not appear.
        assert!(!extra.contains("k/j"), "character keys advertised: {extra}");
    }
}


#[cfg(test)]
mod config_error_tests {
    use super::*;

    /// A dropped config must say so on one readable line: where it broke and
    /// why, without the multi-line excerpt `toml` draws.
    #[test]
    fn summarizes_a_toml_error_to_one_line() {
        let err = crate::opts::Config::load_from_str(
            "[dark-theme]\nscrollbar-color = \"blue\"\n",
        )
        .unwrap_err();
        let summary = summarize_config_error(&err);

        assert!(!summary.contains('\n'), "must be one line: {summary}");
        assert!(summary.contains("using defaults"), "{summary}");
        assert!(summary.contains("line 2"), "should say where: {summary}");
        assert!(summary.contains("expected u32"), "should say why: {summary}");
        // The excerpt/caret lines are noise on a status bar.
        assert!(!summary.contains('^'), "caret leaked in: {summary}");
        println!("SUMMARY: {summary}");
    }
}

/// The heading/table tightening pass once removed everything in the gap, not
/// just the spacers, which silently ate real content.
#[cfg(test)]
mod tighten_tables_tests {
    use super::*;
    use crate::positioner::Spacer;
    use crate::table::Table;
    use crate::text::{Text, TextBox};

    fn heading(s: &str) -> Element {
        let mut tb = TextBox::new(vec![Text::new(s.to_owned(), 1.0, [0.; 4])], 1.0);
        tb.is_header = true;
        Element::TextBox(tb)
    }

    fn prose(s: &str) -> Element {
        Element::TextBox(TextBox::new(
            vec![Text::new(s.to_owned(), 1.0, [0.; 4])],
            1.0,
        ))
    }

    fn spacer() -> Element {
        Element::Spacer(Spacer::invisible())
    }

    fn table() -> Element {
        Element::Table(Table::new())
    }

    fn describe(elements: &[Element]) -> Vec<String> {
        elements
            .iter()
            .map(|e| match e {
                Element::TextBox(tb) => {
                    let text: String = tb.texts.iter().flat_map(|t| t.text.chars()).collect();
                    if tb.is_header {
                        format!("h:{text}")
                    } else {
                        format!("p:{text}")
                    }
                }
                Element::Spacer(_) => "spacer".to_owned(),
                Element::Table(_) => "table".to_owned(),
                _ => "other".to_owned(),
            })
            .collect()
    }

    /// Spacers are the whole gap, so they go and the table tightens up.
    #[test]
    fn spacers_between_heading_and_table_are_dropped() {
        let mut elements = vec![heading("Keys"), spacer(), spacer(), table()];
        Inlyne::tighten_tables_under_headings(&mut elements);
        assert_eq!(describe(&elements), ["h:Keys", "table"]);
    }

    /// The real cheatsheet shape: a title, a generated-from note, an intro,
    /// then the first subheading and its table. Every one of those used to
    /// disappear because the scan ran from the title past all the prose to the
    /// first table it could find.
    #[test]
    fn prose_between_title_and_first_table_survives() {
        let mut elements = vec![
            heading("Hyprland Cheatsheet"),
            spacer(),
            prose("Generated from keybinds.nix on rebuild."),
            spacer(),
            prose("Grouped by bind type."),
            spacer(),
            heading("Keys"),
            spacer(),
            table(),
        ];
        Inlyne::tighten_tables_under_headings(&mut elements);
        assert_eq!(
            describe(&elements),
            [
                "h:Hyprland Cheatsheet",
                "spacer",
                "p:Generated from keybinds.nix on rebuild.",
                "spacer",
                "p:Grouped by bind type.",
                "spacer",
                "h:Keys",
                "table",
            ]
        );
    }

    /// A paragraph in the gap means the table is its own block, so the spacers
    /// around it stay too -- removing them would close a gap the author wrote.
    #[test]
    fn prose_in_the_gap_leaves_spacers_alone() {
        let mut elements = vec![heading("Keys"), spacer(), prose("A note."), spacer(), table()];
        let before = describe(&elements);
        Inlyne::tighten_tables_under_headings(&mut elements);
        assert_eq!(describe(&elements), before);
    }

    /// A captioned table keeps its spacing: the caption is its own label, so it
    /// is not being tucked under the heading.
    #[test]
    fn captioned_table_keeps_its_spacers() {
        let mut captioned = Table::new();
        captioned.caption = Some(TextBox::new(
            vec![Text::new("Table 1".to_owned(), 1.0, [0.; 4])],
            1.0,
        ));
        let mut elements = vec![heading("Keys"), spacer(), Element::Table(captioned)];
        let before = describe(&elements);
        Inlyne::tighten_tables_under_headings(&mut elements);
        assert_eq!(describe(&elements), before);
    }

    /// Two headings in a row: only the one actually above the table tightens.
    #[test]
    fn only_the_heading_above_the_table_tightens() {
        let mut elements = vec![
            heading("Outer"),
            spacer(),
            heading("Inner"),
            spacer(),
            table(),
        ];
        Inlyne::tighten_tables_under_headings(&mut elements);
        assert_eq!(describe(&elements), ["h:Outer", "spacer", "h:Inner", "table"]);
    }
}
