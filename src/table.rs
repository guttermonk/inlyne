use std::sync::Arc;

use crate::text::{Text, TextBox, TextBoxMeasure, TextSystem};
use crate::utils::{default, Point, Rect, Size};

use taffy::node::MeasureFunc;
use taffy::prelude::{
    auto, line, points, AvailableSpace, Display, Layout, Size as TaffySize, Style, Taffy,
};
use taffy::style::FlexDirection;
use taffy::style::JustifyContent;

pub const TABLE_ROW_GAP: f32 = 20.;
pub const TABLE_COL_GAP: f32 = 20.;

#[derive(Debug)]
pub struct TableLayout {
    pub rows: Vec<Vec<Layout>>,
    pub caption_layout: Option<Layout>,
    pub size: Size,
}

#[derive(Default, Debug, PartialEq)]
pub struct Table {
    pub rows: Vec<Vec<TextBox>>,
    pub caption: Option<TextBox>,
}

impl Table {
    pub fn new() -> Table {
        Table::default()
    }

    pub fn set_caption(&mut self, caption: TextBox) {
        self.caption = Some(caption);
    }

    pub fn find_hoverable<'a>(
        &'a self,
        text_system: &mut TextSystem,
        taffy: &mut Taffy,
        loc: Point,
        pos: Point,
        bounds: Size,
        zoom: f32,
    ) -> Option<&'a Text> {
        let table_layout = self.layout(text_system, taffy, bounds, zoom).ok()?;

        // Check caption first if it exists
        if let (Some(caption), Some(caption_layout)) = (&self.caption, &table_layout.caption_layout) {
            if Rect::new(
                (pos.0 + caption_layout.location.x, pos.1 + caption_layout.location.y),
                (caption_layout.size.width, caption_layout.size.height),
            )
            .contains(loc)
            {
                return caption.find_hoverable(
                    text_system,
                    loc,
                    (pos.0 + caption_layout.location.x, pos.1 + caption_layout.location.y),
                    (caption_layout.size.width, caption_layout.size.height),
                    zoom,
                );
            }
        }

        for (row, row_layout) in self.rows.iter().zip(table_layout.rows.iter()) {
            for (item, layout) in row.iter().zip(row_layout.iter()) {
                if Rect::new(
                    (pos.0 + layout.location.x, pos.1 + layout.location.y),
                    (layout.size.width, layout.size.height),
                )
                .contains(loc)
                {
                    return item.find_hoverable(
                        text_system,
                        loc,
                        (pos.0 + layout.location.x, pos.1 + layout.location.y),
                        (layout.size.width, layout.size.height),
                        zoom,
                    );
                }
            }
        }
        None
    }

    pub fn layout(
        &self,
        text_system: &mut TextSystem,
        taffy: &mut Taffy,
        bounds: Size,
        zoom: f32,
    ) -> anyhow::Result<TableLayout> {
        let max_columns = self
            .rows
            .iter()
            .fold(0, |max, row| std::cmp::max(row.len(), max));

        // Create caption node if present and non-empty
        let caption_node = if let Some(ref caption) = self.caption {
            if !caption.texts.is_empty() {
                let caption_clone = caption.clone();
                let textbox_measure = TextBoxMeasure {
                    font_system: text_system.font_system.clone(),
                    text_cache: text_system.text_cache.clone(),
                    textbox: Arc::new(caption_clone),
                    zoom,
                };
                Some(taffy.new_leaf_with_measure(
                    Style::default(),
                    MeasureFunc::Boxed(Box::new(move |known_dimensions, available_space| {
                        textbox_measure.measure(known_dimensions, available_space)
                    })),
                )?)
            } else {
                None
            }
        } else {
            None
        };

        // Setup the grid
        let root_style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: TaffySize {
                width: points(bounds.0),
                height: auto(),
            },
            justify_content: Some(JustifyContent::Start),
            ..default()
        };

        let grid_style = Style {
            display: Display::Grid,
            gap: TaffySize {
                width: points(TABLE_COL_GAP),
                height: points(TABLE_ROW_GAP),
            },
            grid_template_columns: vec![auto(); max_columns],
            ..default()
        };

        let mut nodes = Vec::new();
        let mut node_row = Vec::new();

        for (y, row) in self.rows.iter().enumerate() {
            for (x, item) in row.iter().enumerate() {
                let item = item.clone();
                let textbox_measure = TextBoxMeasure {
                    font_system: text_system.font_system.clone(),
                    text_cache: text_system.text_cache.clone(),
                    textbox: Arc::new(item.clone()),
                    zoom,
                };
                node_row.push(taffy.new_leaf_with_measure(
                    Style {
                        grid_row: line(y as i16 + 1),
                        grid_column: line(x as i16 + 1),
                        ..default()
                    },
                    MeasureFunc::Boxed(Box::new(move |known_dimensions, available_space| {
                        textbox_measure.measure(known_dimensions, available_space)
                    })),
                )?);
            }
            nodes.push(node_row.clone());
            node_row.clear();
        }

        let mut flattened_nodes = Vec::new();
        for row in &nodes {
            flattened_nodes.append(&mut row.clone());
        }

        let grid = taffy.new_with_children(grid_style, &flattened_nodes)?;
        
        // Only create root container if we have a caption
        let (layout_root, caption_node_ref) = if let Some(caption_node) = caption_node {
            // Create flex container for caption + grid
            let mut root_children = Vec::new();
            root_children.push(caption_node);
            root_children.push(grid);
            let root = taffy.new_with_children(root_style, &root_children)?;
            (root, Some(caption_node))
        } else {
            // No caption - use grid directly without wrapper
            (grid, None)
        };

        taffy.compute_layout(
            layout_root,
            TaffySize::<AvailableSpace> {
                width: AvailableSpace::Definite(bounds.0),
                height: AvailableSpace::MaxContent,
            },
        )?;

        let rows_layout: Vec<Vec<Layout>> = nodes
            .into_iter()
            .map(|row| row.iter().map(|n| *taffy.layout(*n).unwrap()).collect())
            .collect();
        
        let caption_layout = if let Some(caption_node_ref) = caption_node_ref {
            Some(*taffy.layout(caption_node_ref)?)
        } else {
            None
        };
        
        let size = taffy.layout(layout_root)?.size;

        Ok(TableLayout {
            rows: rows_layout,
            caption_layout,
            size: (size.width, size.height),
        })
    }

    pub fn push_row(&mut self, row: Vec<TextBox>) {
        self.rows.push(row);
    }
}
