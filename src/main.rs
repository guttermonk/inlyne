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
    ElementState, Event, KeyboardInput, ModifiersState, MouseButton, MouseScrollDelta, WindowEvent,
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
        })
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
                Action::Quit => "Quit",
            };
            
            let combo_str = format!("{}", combo);
            action_map.entry(action_name.to_string())
                .or_insert_with(Vec::new)
                .push(combo_str);
        }
        
        // Build HTML content - use table rows with section names to avoid spacer insertion
        let mut content = String::from("<h1>⌨️ Keyboard Shortcuts</h1>\n\n");
        
        // Debug: log the action map
        tracing::debug!("Help action_map has {} entries", action_map.len());
        for (action, keys) in &action_map {
            tracing::debug!("  {} -> {:?}", action, keys);
        }
        
        // Navigation section
        content.push_str("## Navigation\n| Action | Keys |\n");
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
        content.push_str("## Zoom\n| Action | Keys |\n");
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
        content.push_str("## File Operations\n| Action | Keys |\n");
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
        
        // Application section
        content.push_str("## Application\n| Action | Keys |\n");
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
        
        // Footer
        content.push_str("---\n\n");
        content.push_str("*Press any help key or `ESC` to close this help*\n");
        
        tracing::debug!("Generated help content length: {} chars", content.len());
        content
    }

    pub fn position_queued_elements(
        element_queue: &Arc<Mutex<Vec<Element>>>,
        renderer: &mut Renderer,
        elements: &mut Vec<Positioned<Element>>,
    ) {
        let positioning_start = Instant::now();

        let mut elements_vec: Vec<Element> = element_queue.lock().drain(..).collect();
        
        // FIRST: Remove ALL elements between headers and tables without captions
        let mut elements_to_remove = Vec::new();
        
        for i in 0..elements_vec.len() {
            // Find headers
            if let Element::TextBox(ref tb) = elements_vec[i] {
                if tb.is_header {
                    // Found a header, look for next table
                    let mut found_table_without_caption = false;
                    let mut table_idx = 0;
                    
                    for j in (i + 1)..elements_vec.len() {
                        match &elements_vec[j] {
                            Element::Table(table) => {
                                let has_caption = table.caption.as_ref()
                                    .map(|c| !c.texts.is_empty() && c.texts.iter().any(|t| !t.text.trim().is_empty()))
                                    .unwrap_or(false);
                                
                                if !has_caption {
                                    found_table_without_caption = true;
                                    table_idx = j;
                                }
                                break;
                            }
                            _ => continue,
                        }
                    }
                    
                    // If we found a table without caption, mark ALL elements between for removal
                    if found_table_without_caption {
                        for k in (i + 1)..table_idx {
                            elements_to_remove.push(k);
                            tracing::warn!("Removing element {} between header {} and table {}", k, i, table_idx);
                        }
                    }
                }
            }
        }
        
        // Actually remove the elements (in reverse order to maintain indices)
        for &idx in elements_to_remove.iter().rev() {
            elements_vec.remove(idx);
        }
        
        tracing::info!("Removed {} elements between headers and tables", elements_to_remove.len());
        
        // SECOND: Position remaining elements normally
        // We need to check ahead before consuming elements, so we'll iterate while checking first
        while !elements_vec.is_empty() {
            // Now remove and position the element
            let element = elements_vec.remove(0);
            let mut positioned_element = Positioned::new(element);
            
            // Log element type and current reserved_height
            let element_type = match &positioned_element.inner {
                Element::TextBox(tb) if tb.is_header => "HEADER",
                Element::TextBox(_) => "TextBox",
                Element::Table(_) => "TABLE",
                Element::Spacer(_) => "Spacer",
                Element::Image(_) => "Image",
                Element::Row(_) => "Row",
                Element::Section(_) => "Section",
            };
            
            let reserved_before = renderer.positioner.reserved_height;
            tracing::info!("Positioning element ({}). Reserved height BEFORE: {:.2}px", 
                         element_type, reserved_before);
            
            // Ensure minimum 12px spacing before headers (except at document start)
            if matches!(&positioned_element.inner, Element::TextBox(tb) if tb.is_header) {
                if renderer.positioner.reserved_height > renderer.element_padding * renderer.hidpi_scale {
                    // Not at document start, ensure minimum spacing
                    let min_spacing = 12.0 * renderer.hidpi_scale * renderer.zoom;
                    let last_element_bottom = renderer.positioner.reserved_height;
                    
                    // Add spacing to ensure header has breathing room
                    renderer.positioner.reserved_height = last_element_bottom + min_spacing;
                    tracing::info!("  Ensured {:.2}px minimum (12px base) spacing before header", min_spacing);
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
            let element_y = element_bounds.pos.1;
            
            tracing::info!("  Element positioned at Y={:.2}px, height={:.2}px, bottom={:.2}px", 
                         element_y, element_height, element_y + element_height);
            
            renderer.positioner.reserved_height += element_height;
            
            // Determine padding based on element type
            let padding = if let Element::TextBox(ref tb) = positioned_element.inner {
                if tb.is_header {
                    // Headers always get at least 2px padding after them
                    let min_header_padding = 2.0 * renderer.hidpi_scale * renderer.zoom;
                    let normal_padding = renderer.element_padding * renderer.hidpi_scale * renderer.zoom;
                    let padding = normal_padding.max(min_header_padding);
                    tracing::info!("  Header gets {:.2}px padding after it (min 2px)", padding);
                    padding
                } else {
                    // Normal padding for non-header text
                    renderer.element_padding * renderer.hidpi_scale * renderer.zoom
                }
            } else if matches!(&positioned_element.inner, Element::Table(_)) {
                // Tables always get at least 6px padding for consistency
                let min_table_padding = 6.0 * renderer.hidpi_scale * renderer.zoom;
                let normal_padding = renderer.element_padding * renderer.hidpi_scale * renderer.zoom;
                let padding = normal_padding.max(min_table_padding);
                tracing::info!("  Table gets {:.2}px padding after it", padding);
                padding
            } else {
                // Normal padding for other elements
                renderer.element_padding * renderer.hidpi_scale * renderer.zoom
            };
            
            if padding > 0.0 {
                renderer.positioner.reserved_height += padding;
                tracing::info!("  Added padding: {:.2}px. New reserved height: {:.2}px", 
                            padding, renderer.positioner.reserved_height);
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
        
        // Reset scroll and positioning for help view
        self.renderer.scroll_y = 0.0;
        self.renderer.positioner.reserved_height = self.opts.element_padding * self.renderer.hidpi_scale;
        self.window.request_redraw();
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
        
        // Restore scroll position
        self.renderer.set_scroll_y(self.saved_scroll_y);
        self.window.request_redraw();
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
                    
                    self.renderer
                        .redraw(elements_to_render, &mut self.selection)
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

                        let cursor_icon = if let Some(hoverable) = Self::find_hoverable(
                            &mut self.renderer.text_system,
                            &mut self.renderer.positioner.taffy,
                            &self.elements,
                            loc,
                            screen_size,
                            self.renderer.zoom,
                        ) {
                            match hoverable {
                                Hoverable::Image(Image { is_link: None, .. }) => {
                                    CursorIcon::Default
                                }
                                Hoverable::Text(Text { link: None, .. }) => CursorIcon::Text,
                                _some_link => CursorIcon::Hand,
                            }
                        } else {
                            CursorIcon::Default
                        };
                        self.window.set_cursor_icon(cursor_icon);

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
                        // Handle '?' character directly for better keyboard layout compatibility
                        if c == '?' {
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
                        let key = Key::new(virtual_keycode, scancode);
                        let modified_key = ModifiedKey(key, modifiers);
                        if let Some(action) = self.keycombos.munch(modified_key) {
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
                            }
                        }
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
            let config = match &view.config {
                Some(config_path) => Config::load_from_file(config_path)?,
                None => Config::load_from_system().unwrap_or_else(|err| {
                    tracing::warn!(
                        "Failed reading config file. Falling back to defaults. Error: {}",
                        err
                    );
                    Config::default()
                }),
            };
            let opts = Opts::parse_and_load_from(view, config)?;

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
                    .keep(true),
            )?;

            _ = Config::load_from_str(&new_config)?;

            std::fs::write(config_path, new_config)?;
        }
    }

    Ok(())
}
