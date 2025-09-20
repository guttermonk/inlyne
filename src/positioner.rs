use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use anyhow::Context;
use taffy::Taffy;

use crate::image::Image;
use crate::text::TextSystem;
use crate::utils::{Align, Point, Rect, Size};
use crate::{debug_impls, Element};

pub const DEFAULT_PADDING: f32 = 2.;
pub const DEFAULT_MARGIN: f32 = 100.;

#[derive(Debug, PartialEq)]
pub struct Positioned<T> {
    pub inner: T,
    pub bounds: Option<Rect>,
}

impl<T> Positioned<T> {
    pub fn contains(&self, loc: Point) -> bool {
        self.bounds
            .as_ref()
            .context("Element not positioned")
            .unwrap()
            .contains(loc)
    }
}

impl<T> Positioned<T> {
    pub fn new<I: Into<T>>(item: I) -> Positioned<T> {
        Positioned {
            inner: item.into(),
            bounds: None,
        }
    }
}

#[derive(Default)]
pub struct Positioner {
    pub screen_size: Size,
    pub reserved_height: f32,
    pub hidpi_scale: f32,
    pub page_width: f32,
    pub page_margin: f32,
    pub anchors: HashMap<String, f32>,
    pub taffy: Taffy,
}

impl Positioner {
    pub fn new(screen_size: Size, hidpi_scale: f32, page_width: f32, page_margin: f32) -> Self {
        Self::new_with_padding(screen_size, hidpi_scale, page_width, page_margin, DEFAULT_PADDING)
    }
    
    pub fn new_with_padding(screen_size: Size, hidpi_scale: f32, page_width: f32, page_margin: f32, element_padding: f32) -> Self {
        let mut taffy = Taffy::new();
        taffy.disable_rounding();
        Self {
            reserved_height: element_padding * hidpi_scale,
            hidpi_scale,
            page_width,
            page_margin,
            screen_size,
            anchors: HashMap::new(),
            taffy,
        }
    }

    // Positions the element but does not update reserved_height
    pub fn position(
        &mut self,
        text_system: &mut TextSystem,
        element: &mut Positioned<Element>,
        zoom: f32,
        element_padding: f32,
    ) -> anyhow::Result<()> {
        let centering = (self.screen_size.0 - self.page_width).max(0.) / 2.;

        let bounds = match &mut element.inner {
            Element::TextBox(text_box) => {
                let indent = text_box.indent;
                let pos = (self.page_margin + indent + centering, self.reserved_height);

                let size = text_box.size(
                    text_system,
                    (
                        (self.screen_size.0 - pos.0 - self.page_margin - centering).max(0.),
                        f32::INFINITY,
                    ),
                    zoom,
                );

                if let Some(ref anchor_name) = text_box.is_anchor {
                    let _ = self.anchors.insert(anchor_name.clone(), pos.1);
                }

                Rect::new(pos, size)
            }
            Element::Spacer(spacer) => Rect::new(
                (0., self.reserved_height),
                (0., spacer.space * self.hidpi_scale * zoom),
            ),
            Element::Image(image) => {
                let size = image
                    .size(
                        (self.screen_size.0.min(self.page_width), self.screen_size.1),
                        zoom,
                    )
                    .unwrap_or_default();
                match image.is_aligned {
                    Some(Align::Center) => Rect::new(
                        (self.screen_size.0 / 2. - size.0 / 2., self.reserved_height),
                        size,
                    ),
                    _ => Rect::new((self.page_margin + centering, self.reserved_height), size),
                }
            }
            Element::Table(table) => {
                let pos = (self.page_margin + centering, self.reserved_height);
                let layout = table.layout(
                    text_system,
                    &mut self.taffy,
                    (
                        self.screen_size.0 - pos.0 - self.page_margin - centering,
                        f32::INFINITY,
                    ),
                    zoom,
                )?;
                Rect::new(
                    (self.page_margin + centering, self.reserved_height),
                    layout.size,
                )
            }
            Element::Row(row) => {
                let mut reserved_width = self.page_margin + centering;
                let mut inner_reserved_height: f32 = 0.;
                let mut max_height: f32 = 0.;
                let mut max_width: f32 = 0.;
                for element in &mut row.elements {
                    self.position(text_system, element, zoom, element_padding)?;
                    let element_bounds = element
                        .bounds
                        .as_mut()
                        .context("Element didn't have bounds")?;

                    let target_width = reserved_width
                        + element_padding * self.hidpi_scale * zoom
                        + element_bounds.size.0;
                    // Row would be too long with this element so add another line
                    if target_width > self.screen_size.0 - self.page_margin - centering {
                        max_width = max_width.max(reserved_width);
                        reserved_width = self.page_margin
                            + centering
                            + element_padding * self.hidpi_scale * zoom
                            + element_bounds.size.0;
                        inner_reserved_height +=
                            max_height + element_padding * self.hidpi_scale * zoom;
                        max_height = element_bounds.size.1;
                        element_bounds.pos.0 = self.page_margin + centering;
                    } else {
                        max_height = max_height.max(element_bounds.size.1);
                        element_bounds.pos.0 = reserved_width;
                        reserved_width = target_width;
                    }
                    element_bounds.pos.1 = self.reserved_height + inner_reserved_height;
                }
                max_width = max_width.max(reserved_width);
                inner_reserved_height += max_height + element_padding * self.hidpi_scale * zoom;
                Rect::new(
                    (self.page_margin + centering, self.reserved_height),
                    (
                        max_width - self.page_margin - centering,
                        inner_reserved_height,
                    ),
                )
            }
            Element::Section(section) => {
                let mut section_bounds =
                    Rect::new((self.page_margin + centering, self.reserved_height), (0., 0.));
                if let Some(ref mut summary) = *section.summary {
                    self.position(text_system, summary, zoom, element_padding)?;
                    let element_size = summary
                        .bounds
                        .as_mut()
                        .context("Element didn't have bounds")?
                        .size;
                    self.reserved_height +=
                        element_size.1 + element_padding * self.hidpi_scale * zoom;
                    section_bounds.size.1 +=
                        element_size.1 + element_padding * self.hidpi_scale * zoom;
                    section_bounds.size.0 = section_bounds.size.0.max(element_size.0)
                }
                for element in &mut section.elements {
                    self.position(text_system, element, zoom, element_padding)?;
                    let element_size = element
                        .bounds
                        .as_mut()
                        .context("Element didn't have bounds")?
                        .size;
                    self.reserved_height +=
                        element_size.1 + element_padding * self.hidpi_scale * zoom;
                    if !*section.hidden.borrow() {
                        section_bounds.size.1 +=
                            element_size.1 + element_padding * self.hidpi_scale * zoom;
                        section_bounds.size.0 = section_bounds.size.0.max(element_size.0)
                    }
                }
                self.reserved_height = section_bounds.pos.1;
                section_bounds
            }
        };
        element.bounds = Some(bounds);
        Ok(())
    }

    // Resets reserved height and positions every element again
    pub fn reposition(
        &mut self,
        text_system: &mut TextSystem,
        elements: &mut [Positioned<Element>],
        zoom: f32,
        element_padding: f32,
    ) -> anyhow::Result<()> {
        self.reserved_height = element_padding * self.hidpi_scale * zoom;

        for i in 0..elements.len() {
            // Position the element at current reserved_height
            self.position(text_system, &mut elements[i], zoom, element_padding)?;
            
            // Update reserved_height to bottom of this element
            let element_bounds = elements[i]
                .bounds
                .as_ref()
                .context("Element didn't have bounds")?;
            
            self.reserved_height = element_bounds.pos.1 + element_bounds.size.1;
            
            // Check if we should skip padding after this element
            // Skip if this is a header followed by a table without caption
            let skip_padding = if matches!(&elements[i].inner, Element::TextBox(text_box) if text_box.is_header) {
                // This is a header - check if next element is a table without caption
                if i + 1 < elements.len() {
                    matches!(&elements[i + 1].inner, Element::Table(table)
                        if table.caption.as_ref()
                            .map(|c| c.texts.is_empty() || c.texts.iter().all(|t| t.text.trim().is_empty()))
                            .unwrap_or(true))
                } else {
                    false
                }
            } else {
                false
            };
            
            // Add padding after element unless we're skipping it
            if !skip_padding {
                self.reserved_height += element_padding * self.hidpi_scale * zoom;
            }
        }

        Ok(())
    }
}

#[derive(PartialEq)]
pub struct Spacer {
    pub space: f32,
    pub visible: bool,
}

impl Spacer {
    pub fn invisible() -> Self {
        Self::new(5.0, false)
    }

    pub fn visible() -> Self {
        Self::new(5.0, true)
    }

    pub fn new(space: f32, visible: bool) -> Self {
        Self { space, visible }
    }
}

impl fmt::Debug for Spacer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        debug_impls::spacer(self, f)
    }
}

#[derive(Debug, PartialEq)]
pub struct Row {
    pub elements: Vec<Positioned<Element>>,
    pub hidpi_scale: f32,
}

impl Row {
    pub fn with_image(image: Image, hidpi_scale: f32) -> Self {
        Self {
            elements: vec![Positioned::new(image)],
            hidpi_scale,
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Section {
    pub elements: Vec<Positioned<Element>>,
    pub hidpi_scale: f32,
    pub hidden: RefCell<bool>,
    pub summary: Box<Option<Positioned<Element>>>,
}

impl Section {
    pub fn bare(hidpi_scale: f32) -> Self {
        Self {
            elements: Default::default(),
            hidpi_scale,
            hidden: Default::default(),
            summary: Default::default(),
        }
    }
}
