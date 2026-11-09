//! Drawing for the session sidebar.
//!
//! The sidebar occupies the left portion of `border.left`; see
//! `render/borders.rs` for how that space is carved out.

use crate::sessions::store::with_store;
use crate::sessions::{visible_range, NodeId, Placement, RowKind, VisibleRow};
use crate::termwindow::box_model::*;
use crate::termwindow::sidebar::{ContextMenu, SidebarFocus, SIDEBAR_LINE_HEIGHT};
use crate::termwindow::UIItemType;
use crate::utilsprites::RenderMetrics;
use ::window::{Rect, RectF};
use config::{Dimension, DimensionContext, TabBarColors};
use std::rc::Rc;
use wezterm_font::LoadedFont;
use wezterm_term::color::ColorPalette;

/// Width of the drag-to-resize strip along the sidebar's right edge.
const RESIZE_HANDLE_WIDTH: f32 = 4.;

/// Indent per tree level, in cells.
const INDENT_CELLS: f32 = 1.5;

impl crate::TermWindow {
    fn sidebar_font(&self) -> anyhow::Result<(Rc<LoadedFont>, RenderMetrics)> {
        let font = self.fonts.title_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        Ok((font, metrics))
    }

    /// Height of one row. Shared with `sidebar_row_capacity` so that the number
    /// of rows we lay out matches the number we claim to have room for.
    pub fn sidebar_row_height(&self) -> f32 {
        let cell_height = match self.sidebar_font() {
            Ok((_, metrics)) => metrics.cell_size.height as f32,
            Err(_) => self.render_metrics.cell_size.height as f32,
        };
        cell_height * SIDEBAR_LINE_HEIGHT as f32
    }

    /// The sidebar's rectangle, in window coordinates.
    ///
    /// It sits at the left of `border.left`, inboard of any decorative window
    /// frame border, and spans the full height of the window so that the tab
    /// bar starts to its right.
    pub fn sidebar_bounds(&self) -> RectF {
        let border = self.get_os_border();
        let width = self.sidebar_width();
        // The status bar sits to the right of the sidebar, not under it.
        let bottom = border.bottom.get() as f32 - self.status_bar_pixel_height();
        euclid::rect(
            (border.left.get() as f32 - width).max(0.),
            border.top.get() as f32,
            width,
            (self.dimensions.pixel_height as f32 - border.top.get() as f32 - bottom).max(0.),
        )
    }

    /// The rectangle of the sidebar field that currently has the keyboard, in
    /// window coordinates, for positioning the IME candidate window.
    ///
    /// Returns None when the sidebar does not own the keyboard, which is the
    /// signal to fall back to the terminal cursor.
    pub fn sidebar_text_cursor_rect(&self) -> Option<Rect> {
        if !self.sidebar_is_expanded() {
            return None;
        }

        let focus = self.sidebar.focus.clone()?;
        let bounds = self.sidebar_bounds();
        let row_height = self.sidebar_row_height();

        // Row 0 is the search box; the tree starts below it.
        let row = match focus {
            SidebarFocus::Search => 0,
            SidebarFocus::Rename(id) => {
                let search = self.sidebar.search.clone();
                let index = with_store(|store| {
                    let tree = store.tree();
                    let filter = tree.matching_ids(&search);
                    tree.visible_rows(filter.as_ref())
                        .iter()
                        .position(|r| r.id == id)
                });
                match index {
                    // Scrolled out of view; anchor at the top rather than
                    // placing the candidate window off screen.
                    None => 0,
                    Some(index) => {
                        let visible = index.saturating_sub(self.sidebar.scroll_offset);
                        visible + 1
                    }
                }
            }
        };

        Some(euclid::rect(
            bounds.min_x() as isize,
            (bounds.min_y() + row as f32 * row_height) as isize,
            bounds.width() as isize,
            row_height as isize,
        ))
    }

    pub(crate) fn sidebar_colors(&self) -> TabBarColors {
        // The sidebar deliberately reuses the tab bar palette rather than
        // introducing a dozen colour options of its own.
        self.config
            .colors
            .as_ref()
            .and_then(|c| c.tab_bar.as_ref())
            .cloned()
            .unwrap_or_else(TabBarColors::default)
    }

    pub fn paint_sidebar(&mut self, palette: &ColorPalette) -> anyhow::Result<()> {
        if !self.sidebar_is_enabled() {
            return Ok(());
        }

        // Rebuilding this every frame re-shapes every row's text, which is not
        // cheap; the fancy tab bar caches its computed element for the same
        // reason. The generation guards against edits made from an overlay,
        // which never reach `invalidate_sidebar`.
        let generation = (
            with_store(|store| store.generation()),
            crate::sessions::ssh::with_ssh_hosts(|store| store.generation()),
        );
        let stale = match &self.sidebar_computed {
            Some((cached, _)) => *cached != generation,
            None => true,
        };
        if stale {
            let computed = self.build_sidebar(palette)?;
            self.sidebar_computed = Some((generation, computed));
        }

        let computed = self
            .sidebar_computed
            .take()
            .expect("just populated the sidebar cache");
        for element in &computed.1 {
            let mut ui_items = element.ui_items();
            let gl_state = self.render_state.as_ref().unwrap();
            self.render_element(element, gl_state, None)?;
            self.ui_items.append(&mut ui_items);
        }
        self.sidebar_computed = Some(computed);
        self.paint_sidebar_drop_target()
    }

    /// Show where a node being dragged would land: a line across the gap it
    /// would go into, indented to the level it would go in at, or a box around
    /// the folder it would go inside.
    ///
    /// Drawn over the cached rows rather than built into them, so following
    /// the pointer costs a repaint, not a rebuild of the tree.
    fn paint_sidebar_drop_target(&self) -> anyhow::Result<()> {
        let target = match self.sidebar.drag.and_then(|drag| drag.target) {
            Some(target) => target,
            None => return Ok(()),
        };

        let (_, metrics) = self.sidebar_font()?;
        let thickness = metrics.underline_height.max(2) as f32;
        let indent = metrics.cell_size.width as f32 * INDENT_CELLS;
        let pad = metrics.cell_size.width as f32 * 0.5;
        let bounds = self.sidebar_bounds();

        // Where each row was drawn in this paint; the ui items were just
        // collected from it.
        let row_rect = |id: NodeId| -> Option<RectF> {
            self.ui_items.iter().find_map(|item| match item.item_type {
                UIItemType::SidebarNode(row) if row == id => Some(euclid::rect(
                    item.x as f32,
                    item.y as f32,
                    item.width as f32,
                    item.height as f32,
                )),
                _ => None,
            })
        };
        let depth_of = |id: NodeId| -> usize {
            with_store(|store| {
                store
                    .tree()
                    .visible_rows(None)
                    .iter()
                    .find(|row| row.id == id)
                    .map(|row| row.depth)
                    .unwrap_or(0)
            })
        };
        let line = |y: f32, depth: usize| -> RectF {
            let x = bounds.min_x() + pad + depth as f32 * indent;
            euclid::rect(
                x,
                y - thickness / 2.,
                (bounds.max_x() - pad - x).max(0.),
                thickness,
            )
        };

        let rects = match target {
            Placement::Before(id) => row_rect(id)
                .map(|rect| vec![line(rect.min_y(), depth_of(id))])
                .unwrap_or_default(),
            Placement::After(id) => row_rect(id)
                .map(|rect| vec![line(rect.max_y(), depth_of(id))])
                .unwrap_or_default(),
            Placement::LastIn(None) => self
                .ui_items
                .iter()
                .filter(|item| matches!(item.item_type, UIItemType::SidebarNode(_)))
                .last()
                .map(|item| vec![line((item.y + item.height) as f32, 0)])
                .unwrap_or_default(),
            Placement::LastIn(Some(folder)) => row_rect(folder)
                .map(|rect| {
                    vec![
                        euclid::rect(rect.min_x(), rect.min_y(), rect.width(), thickness),
                        euclid::rect(
                            rect.min_x(),
                            rect.max_y() - thickness,
                            rect.width(),
                            thickness,
                        ),
                        euclid::rect(rect.min_x(), rect.min_y(), thickness, rect.height()),
                        euclid::rect(
                            rect.max_x() - thickness,
                            rect.min_y(),
                            thickness,
                            rect.height(),
                        ),
                    ]
                })
                .unwrap_or_default(),
        };

        // The light text colour of the active tab: the edge colours are all
        // within a shade of the sidebar background in the default palette.
        let color = self.sidebar_colors().active_tab().fg_color.to_linear();
        let gl_state = self.render_state.as_ref().unwrap();
        // Above the rows, alongside the resize handle; the context menu sits
        // above both, though a press always closes it before a drag can begin.
        let layer = gl_state.layer_for_zindex(11)?;
        let mut layers = layer.quad_allocator();
        for rect in rects {
            self.filled_rectangle(&mut layers, 2, rect, color)?;
        }
        Ok(())
    }

    /// Build the sidebar as a list of independently positioned elements.
    ///
    /// They are returned in draw order, and `resolve_ui_item` hit-tests the
    /// item list in reverse, so the context menu coming last is what makes it
    /// take clicks ahead of the rows underneath it.
    fn build_sidebar(&self, palette: &ColorPalette) -> anyhow::Result<Vec<ComputedElement>> {
        let (font, metrics) = self.sidebar_font()?;
        let bounds = self.sidebar_bounds();
        if bounds.width() <= 0. || bounds.height() <= 0. {
            return Ok(vec![]);
        }

        let colors = self.sidebar_colors();
        let pad = metrics.cell_size.width as f32 * 0.5;
        let capacity = self.sidebar_row_capacity();

        if !self.sidebar_is_expanded() {
            return Ok(vec![self.sidebar_rail(&font, &metrics, &colors, bounds)?]);
        }

        let mut children = vec![self.sidebar_search_element(&font, &colors, bounds.width(), pad)];

        let search = self.sidebar.search.clone();
        let scroll_offset = self.sidebar.scroll_offset;
        with_store(|store| {
            let tree = store.tree();
            let filter = tree.matching_ids(&search);
            let rows = tree.visible_rows(filter.as_ref());

            if rows.is_empty() {
                // An empty sidebar with no affordance just looks broken; say
                // how to get started, or why the list came back empty.
                let hint = if tree.is_empty() {
                    "Right click to add a session"
                } else {
                    "No matching sessions"
                };
                children.push(self.sidebar_hint_element(hint, &font, &colors, bounds.width(), pad));
                return;
            }

            let range = visible_range(rows.len(), scroll_offset, capacity);
            for row in &rows[range] {
                children.push(self.sidebar_row_element(
                    row,
                    &font,
                    &metrics,
                    &colors,
                    palette,
                    bounds.width(),
                    pad,
                ));
            }
        });

        let container = Element::new(&font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .item_type(UIItemType::SidebarBackground)
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: colors.background().to_linear().into(),
                text: colors.inactive_tab().fg_color.to_linear().into(),
            });

        let mut computed = vec![self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.height(),
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.max_x(),
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds,
                metrics: &metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 10,
            },
            &container,
        )?];

        computed.push(self.sidebar_resize_handle(&font, &metrics, &colors, bounds)?);

        if let Some(menu) = self.sidebar.menu.clone() {
            computed.push(self.sidebar_menu_element(&menu, &font, &metrics, &colors)?);
        }

        Ok(computed)
    }

    fn sidebar_search_element(
        &self,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        let focused = self.sidebar.focus == Some(SidebarFocus::Search);

        // The trailing underscore is the only cursor we have here, and without
        // it a focused-but-empty search box looks identical to an idle one --
        // which matters, because while it is focused the terminal is not
        // receiving any keystrokes.
        let composing = if focused {
            self.sidebar.composing.as_deref().unwrap_or("")
        } else {
            ""
        };

        let text = match (focused, self.sidebar.search.is_empty()) {
            (true, _) => format!("Search: {}{composing}_", self.sidebar.search),
            (false, true) => "Search...".to_string(),
            (false, false) => format!("Search: {}", self.sidebar.search),
        };

        let style = if focused {
            colors.active_tab()
        } else {
            colors.inactive_tab()
        };

        let hover = colors.inactive_tab_hover();
        let field_colors = ElementColors {
            border: BorderColor::default(),
            bg: style.bg_color.to_linear().into(),
            text: style.fg_color.to_linear().into(),
        };

        let label = Element::new(font, ElementContent::Text(text))
            .item_type(UIItemType::SidebarSearch)
            .colors(field_colors.clone());

        // The only always-visible way to collapse the sidebar; the key
        // assignment deliberately has no default binding.
        let chevron = Element::new(font, ElementContent::Text("\u{00ab}".to_string()))
            .float(Float::Right)
            .item_type(UIItemType::SidebarCollapseToggle)
            .colors(field_colors.clone())
            .hover_colors(Some(ElementColors {
                border: BorderColor::default(),
                bg: hover.bg_color.to_linear().into(),
                text: hover.fg_color.to_linear().into(),
            }));

        Element::new(font, ElementContent::Children(vec![label, chevron]))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .padding(BoxDimension {
                left: Dimension::Pixels(pad),
                right: Dimension::Pixels(pad),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
            .colors(field_colors)
    }

    /// What is left when the sidebar is collapsed: a sliver carrying the
    /// arrow that brings it back.
    ///
    /// Collapsing to zero width would strand the user with no way to reopen it
    /// short of the command palette.
    fn sidebar_rail(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        bounds: RectF,
    ) -> anyhow::Result<ComputedElement> {
        let style = colors.inactive_tab();
        let hover = colors.inactive_tab_hover();

        let arrow = Element::new(font, ElementContent::Text("\u{00bb}".to_string()))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .item_type(UIItemType::SidebarCollapseToggle)
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: colors.background().to_linear().into(),
                text: style.fg_color.to_linear().into(),
            })
            .hover_colors(Some(ElementColors {
                border: BorderColor::default(),
                bg: hover.bg_color.to_linear().into(),
                text: hover.fg_color.to_linear().into(),
            }));

        self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.height(),
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.max_x(),
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds,
                metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 10,
            },
            &arrow,
        )
    }

    /// A non-interactive line of explanatory text.
    fn sidebar_hint_element(
        &self,
        text: &str,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        let style = colors.inactive_tab();
        Element::new(font, ElementContent::Text(text.to_string()))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .padding(BoxDimension {
                left: Dimension::Pixels(pad),
                right: Dimension::Pixels(pad),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: colors.background().to_linear().into(),
                text: style.fg_color.to_linear().into(),
            })
    }

    fn sidebar_row_element(
        &self,
        row: &VisibleRow,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        palette: &ColorPalette,
        width: f32,
        pad: f32,
    ) -> Element {
        let renaming = self.sidebar.focus == Some(SidebarFocus::Rename(row.id));

        let prefix = match row.kind {
            RowKind::Folder { expanded: true } => "\u{25be} ",
            RowKind::Folder { expanded: false } => "\u{25b8} ",
            RowKind::Session
                if crate::sessions::ssh::with_ssh_hosts(|store| store.get(row.id).is_some()) =>
            {
                "\u{21c4} "
            }
            RowKind::Session => "\u{2022} ",
        };

        let text = if renaming {
            let composing = self.sidebar.composing.as_deref().unwrap_or("");
            format!("{prefix}{}{composing}_", self.sidebar.edit_buffer)
        } else {
            format!("{prefix}{}", row.name)
        };

        let selected = self.sidebar.selected == Some(row.id);
        let style = if selected || renaming {
            colors.active_tab()
        } else {
            colors.inactive_tab()
        };
        let hover = colors.inactive_tab_hover();

        // A node whose last launch failed is drawn in red, so that clicking it
        // and getting nothing doesn't just look like the sidebar is broken.
        let text_color = if self.sidebar.failed.contains(&row.id) {
            palette.resolve_fg(wezterm_term::color::ColorAttribute::PaletteIndex(1))
        } else {
            style.fg_color.into()
        };

        let indent = row.depth as f32 * metrics.cell_size.width as f32 * INDENT_CELLS;

        let row_colors = ElementColors {
            border: BorderColor::default(),
            bg: style.bg_color.to_linear().into(),
            text: text_color.to_linear().into(),
        };

        let mut parts =
            vec![Element::new(font, ElementContent::Text(text)).colors(row_colors.clone())];

        // How many terminals this session is holding. Sessions keep running in
        // the background once opened, so without this there is no way to see
        // what is still alive behind the tree.
        let running = match row.kind {
            RowKind::Session => self.session_terminal_count(row.id),
            RowKind::Folder { .. } => 0,
        };
        if running > 0 {
            parts.push(
                Element::new(font, ElementContent::Text(format!("{running}")))
                    .float(Float::Right)
                    .colors(row_colors.clone()),
            );
        }

        Element::new(font, ElementContent::Children(parts))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .item_type(UIItemType::SidebarNode(row.id))
            .padding(BoxDimension {
                left: Dimension::Pixels(pad + indent),
                right: Dimension::Pixels(pad),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels((width - pad * 2. - indent).max(0.))))
            .colors(row_colors)
            .hover_colors(Some(ElementColors {
                border: BorderColor::default(),
                bg: hover.bg_color.to_linear().into(),
                text: hover.fg_color.to_linear().into(),
            }))
    }

    /// The strip along the right edge that starts a width drag.
    fn sidebar_resize_handle(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        bounds: RectF,
    ) -> anyhow::Result<ComputedElement> {
        // While a drag is in flight we draw the strip where the pointer is
        // rather than where the sidebar currently ends, so there is some
        // feedback without resizing every pty on each mouse-move.
        let x = match self.sidebar.pending_width {
            Some(width) => (bounds.min_x() + width - RESIZE_HANDLE_WIDTH).max(bounds.min_x()),
            None => bounds.max_x() - RESIZE_HANDLE_WIDTH,
        };

        let handle_bounds = euclid::rect(x, bounds.min_y(), RESIZE_HANDLE_WIDTH, bounds.height());

        let element = Element::new(font, ElementContent::Text(String::new()))
            .display(DisplayType::Block)
            .item_type(UIItemType::SidebarResizeHandle)
            .min_width(Some(Dimension::Pixels(RESIZE_HANDLE_WIDTH)))
            .min_height(Some(Dimension::Pixels(handle_bounds.height())))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: if self.sidebar.pending_width.is_some() {
                    colors.inactive_tab_edge_hover().to_linear().into()
                } else {
                    colors.inactive_tab_edge().to_linear().into()
                },
                text: colors.inactive_tab().fg_color.to_linear().into(),
            });

        self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: handle_bounds.height(),
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: handle_bounds.max_x(),
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: handle_bounds,
                metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 11,
            },
            &element,
        )
    }

    fn sidebar_menu_element(
        &self,
        menu: &ContextMenu,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
    ) -> anyhow::Result<ComputedElement> {
        let cell_width = metrics.cell_size.width as f32;
        let pad = cell_width;
        let row_height = self.sidebar_row_height();

        let widest = menu
            .items
            .iter()
            .map(|action| action.label().chars().count())
            .max()
            .unwrap_or(0) as f32;
        let menu_width = widest * cell_width + pad * 2.;
        let menu_height = menu.items.len() as f32 * row_height;

        let normal = colors.inactive_tab();
        let hover = colors.inactive_tab_hover();

        let items = menu
            .items
            .iter()
            .map(|action| {
                Element::new(font, ElementContent::Text(action.label().to_string()))
                    .display(DisplayType::Block)
                    .line_height(Some(SIDEBAR_LINE_HEIGHT))
                    .item_type(UIItemType::SidebarMenuItem(*action))
                    .padding(BoxDimension {
                        left: Dimension::Pixels(pad),
                        right: Dimension::Pixels(pad),
                        top: Dimension::Pixels(0.),
                        bottom: Dimension::Pixels(0.),
                    })
                    .min_width(Some(Dimension::Pixels((menu_width - pad * 2.).max(0.))))
                    .colors(ElementColors {
                        border: BorderColor::default(),
                        bg: normal.bg_color.to_linear().into(),
                        text: normal.fg_color.to_linear().into(),
                    })
                    .hover_colors(Some(ElementColors {
                        border: BorderColor::default(),
                        bg: hover.bg_color.to_linear().into(),
                        text: hover.fg_color.to_linear().into(),
                    }))
            })
            .collect();

        // Keep the menu on screen when it is opened near an edge.
        let window_width = self.dimensions.pixel_width as f32;
        let window_height = self.dimensions.pixel_height as f32;
        let x = menu.x.min((window_width - menu_width).max(0.));
        let y = menu.y.min((window_height - menu_height).max(0.));
        let bounds = euclid::rect(x, y, menu_width, menu_height);

        let element = Element::new(font, ElementContent::Children(items))
            .display(DisplayType::Block)
            .min_width(Some(Dimension::Pixels(menu_width)))
            .border(BoxDimension {
                left: Dimension::Pixels(1.),
                right: Dimension::Pixels(1.),
                top: Dimension::Pixels(1.),
                bottom: Dimension::Pixels(1.),
            })
            .colors(ElementColors {
                border: BorderColor::new(colors.inactive_tab_edge_hover().to_linear()),
                bg: normal.bg_color.to_linear().into(),
                text: normal.fg_color.to_linear().into(),
            });

        self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: window_height,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.max_x(),
                    pixel_cell: cell_width,
                },
                bounds,
                metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 12,
            },
            &element,
        )
    }
}
