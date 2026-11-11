//! Drawing for the session sidebar.
//!
//! The sidebar occupies the left portion of `border.left`; see
//! `render/borders.rs` for how that space is carved out.

use crate::sessions::store::with_store;
use crate::sessions::{visible_range, NodeId, Placement, RowKind, SidebarPage, VisibleRow};
use crate::termwindow::box_model::*;
use crate::termwindow::sidebar::{
    is_wsl_domain, tree_row_on_screen, ContextMenu, MenuAction, SidebarFocus, SIDEBAR_LINE_HEIGHT,
};
use crate::termwindow::theme::{
    fill, icon, lin, menu_height, menu_rows, palette, px, rounded, MenuEntry, MenuRow,
};
use crate::termwindow::UIItemType;
use crate::utilsprites::RenderMetrics;
use ::window::{Rect, RectF};
use config::{Dimension, DimensionContext, TabBarColors};
use std::rc::Rc;
use wezterm_font::LoadedFont;
use wezterm_term::color::ColorPalette;
use window::color::LinearRgba;

/// Width of the drag-to-resize strip along the sidebar's right edge.
const RESIZE_HANDLE_WIDTH: f32 = 4.;

/// Indent per tree level, in cells.
const INDENT_CELLS: f32 = 1.5;

/// What a tree row is, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowIcon {
    Folder,
    Ssh,
    Wsl,
    Local,
}

impl RowIcon {
    fn glyph(self) -> (&'static str, u32) {
        match self {
            Self::Folder => (icon::FOLDER, palette::ICON_FOLDER),
            Self::Ssh => (icon::SERVER, palette::ICON_SSH),
            Self::Wsl => (icon::LINUX, palette::ICON_WSL),
            Self::Local => (icon::TERMINAL, palette::ICON_LOCAL),
        }
    }
}

/// The two nested boxes of a field: an edge `edge` drawn as a one pixel
/// ring around `inner`. The pair fits in one sidebar row.
fn field_box(font: &Rc<LoadedFont>, inner: Element, edge: LinearRgba, dpi: f32) -> Element {
    let one = px(dpi, 1.).max(1.).floor();
    Element::new(font, ElementContent::Children(vec![inner]))
        .display(DisplayType::Block)
        .padding(BoxDimension::new(Dimension::Pixels(one)))
        .border_corners(Some(rounded(px(dpi, 6.))))
        .colors(fill(edge, edge))
}

/// A run of text over whatever is behind it; `color` may be inherited, so
/// that it follows its row's hover colour. No background of its own: a
/// square one would cover the row's rounded corners.
pub(crate) fn text_run(font: &Rc<LoadedFont>, text: String, color: InheritableColor) -> Element {
    Element::new(font, ElementContent::Text(text)).colors(ElementColors {
        border: BorderColor::default(),
        bg: LinearRgba::TRANSPARENT.into(),
        text: color,
    })
}

impl crate::TermWindow {
    pub(crate) fn sidebar_font(&self) -> anyhow::Result<(Rc<LoadedFont>, RenderMetrics)> {
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

    /// The page tab rail down the sidebar's left edge.
    pub fn sidebar_rail_bounds(&self) -> RectF {
        let bounds = self.sidebar_bounds();
        euclid::rect(
            bounds.min_x(),
            bounds.min_y(),
            self.sidebar_rail_width().min(bounds.width()),
            bounds.height(),
        )
    }

    /// The panel to the right of the rail, showing the current page; zero
    /// width while collapsed.
    pub fn sidebar_panel_bounds(&self) -> RectF {
        let bounds = self.sidebar_bounds();
        let rail = self.sidebar_rail_width().min(bounds.width());
        euclid::rect(
            bounds.min_x() + rail,
            bounds.min_y(),
            bounds.width() - rail,
            bounds.height(),
        )
    }

    /// The rectangle of the sidebar field that currently has the keyboard, in
    /// window coordinates, for positioning the IME candidate window.
    ///
    /// Returns None when the sidebar does not own the keyboard, which is the
    /// signal to fall back to the terminal cursor.
    pub fn sidebar_text_cursor_rect(&self) -> Option<Rect> {
        // Only the page that is showing can hold the focus: switching pages
        // releases it.
        if !self.sidebar_is_expanded() {
            return None;
        }

        let focus = self.sidebar.focus.clone()?;
        let bounds = self.sidebar_panel_bounds();
        let row_height = self.sidebar_row_height();

        // The session page has its title, then the search box, then the
        // tree; the file page starts with its path bar.
        let row = match focus {
            SidebarFocus::FilesPath => 0,
            SidebarFocus::Search => 1,
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
                        tree_row_on_screen(index.saturating_sub(self.sidebar.scroll_offset))
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
            self.files.generation,
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
        let bounds = self.sidebar_panel_bounds();

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

        let color = lin(palette::ACCENT);
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
        let whole = self.sidebar_bounds();
        if whole.width() <= 0. || whole.height() <= 0. {
            return Ok(vec![]);
        }

        let colors = self.sidebar_colors();
        let pad = metrics.cell_size.width as f32 * 0.5;
        let capacity = self.sidebar_row_capacity();

        let rail = self.sidebar_rail(&font, &metrics, self.sidebar_rail_bounds())?;
        let bounds = self.sidebar_panel_bounds();
        if !self.sidebar_is_expanded() || bounds.width() <= 0. {
            return Ok(vec![rail]);
        }

        if self.sidebar.page == SidebarPage::Files {
            let mut computed = vec![
                rail,
                self.build_files_page(&font, &metrics, &colors, palette, bounds, pad)?,
            ];
            computed.extend(
                self.files_transfer_element(&font, &metrics, &colors, palette, bounds, pad)?,
            );
            computed.push(self.sidebar_resize_handle(&font, &metrics, &colors, bounds)?);
            // Last, so that it takes clicks ahead of the rows under it.
            computed.extend(self.files_menu_element(&font, &metrics)?);
            return Ok(computed);
        }

        let mut children = vec![
            self.sidebar_header_element(&font, bounds.width(), pad),
            self.sidebar_search_element(&font, &metrics, bounds.width(), pad),
        ];

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
                    "右键添加会话"
                } else {
                    "没有匹配的会话"
                };
                children.push(self.sidebar_hint_element(hint, &font, &colors, bounds.width(), pad));
                return;
            }

            let range = visible_range(rows.len(), scroll_offset, capacity);
            for row in &rows[range] {
                let row_icon = match row.kind {
                    RowKind::Folder { .. } => RowIcon::Folder,
                    RowKind::Session
                        if crate::sessions::ssh::with_ssh_hosts(|hosts| {
                            hosts.get(row.id).is_some()
                        }) =>
                    {
                        RowIcon::Ssh
                    }
                    RowKind::Session => match tree.find(row.id).map(|node| &node.kind) {
                        Some(wezterm_sessions::tree::NodeKind::Session { spawn }) => {
                            match &spawn.domain {
                                config::keyassignment::SpawnTabDomain::DomainName(name)
                                    if is_wsl_domain(name) =>
                                {
                                    RowIcon::Wsl
                                }
                                _ => RowIcon::Local,
                            }
                        }
                        _ => RowIcon::Local,
                    },
                };
                children.push(self.sidebar_row_element(
                    row,
                    row_icon,
                    &font,
                    &metrics,
                    bounds.width(),
                    pad,
                ));
            }
        });

        let mut computed = vec![
            rail,
            self.sidebar_panel(
                children,
                Some(UIItemType::SidebarBackground),
                &font,
                &metrics,
                &colors,
                bounds,
            )?,
        ];

        computed.push(self.sidebar_resize_handle(&font, &metrics, &colors, bounds)?);

        if let Some(menu) = self.sidebar.menu.clone() {
            computed.extend(self.sidebar_menu_element(&menu, &font, &metrics)?);
        }

        Ok(computed)
    }

    /// The session page's title row: its name, and buttons for what the
    /// menu over empty space offers most, plus collapsing the sidebar.
    fn sidebar_header_element(&self, font: &Rc<LoadedFont>, width: f32, pad: f32) -> Element {
        let dpi = self.dimensions.dpi as f32;
        let panel = lin(palette::PANEL);
        let button = |glyph: &str, item: UIItemType| {
            Element::new(font, ElementContent::Text(glyph.to_string()))
                .float(Float::Right)
                .item_type(item)
                .padding(BoxDimension {
                    left: Dimension::Pixels(px(dpi, 5.)),
                    right: Dimension::Pixels(px(dpi, 5.)),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .border_corners(Some(rounded(px(dpi, 6.))))
                .colors(fill(panel, lin(palette::TEXT_MUTED)))
                .hover_colors(Some(fill(
                    lin(palette::ROW_HOVER),
                    lin(palette::TEXT_STRONG),
                )))
        };
        // Floats stack from the right edge in the order given.
        let children = vec![
            Element::new(font, ElementContent::Text("会话".to_string()))
                .colors(fill(panel, lin(palette::TEXT_MUTED))),
            // The only always-visible way to collapse the sidebar; the key
            // assignment deliberately has no default binding.
            button(icon::CHEVRON_LEFT, UIItemType::SidebarCollapseToggle),
            button(
                icon::NEW_FOLDER,
                UIItemType::SidebarHeaderAction(MenuAction::NewFolder),
            ),
            button(
                icon::ADD,
                UIItemType::SidebarHeaderAction(MenuAction::NewSession),
            ),
        ];
        Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .padding(BoxDimension {
                left: Dimension::Pixels(pad + px(dpi, 6.)),
                right: Dimension::Pixels(px(dpi, 4.)),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels(
                (width - pad - px(dpi, 10.)).max(0.),
            )))
            .colors(fill(panel, lin(palette::TEXT_MUTED)))
    }

    fn sidebar_search_element(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
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

        let (text, text_color) = match (focused, self.sidebar.search.is_empty()) {
            (true, _) => (
                format!("{}{composing}_", self.sidebar.search),
                palette::TEXT_STRONG,
            ),
            (false, true) => ("搜索会话".to_string(), palette::TEXT_FAINT),
            (false, false) => (self.sidebar.search.clone(), palette::TEXT),
        };

        let raised = lin(palette::RAISED);
        let parts = vec![
            Element::new(font, ElementContent::Text(format!("{} ", icon::SEARCH)))
                .colors(fill(raised, lin(palette::TEXT_FAINT))),
            Element::new(font, ElementContent::Text(text)).colors(fill(raised, lin(text_color))),
        ];
        let _ = pad;
        self.sidebar_field(font, metrics, parts, focused, width)
            .item_type(UIItemType::SidebarSearch)
    }

    /// A one-line input box in the panel: a rounded well ringed in the
    /// input edge colour, or the accent while it has the keyboard. Ring,
    /// margin and padding come out of the row's height rather than adding to
    /// it: the rows below are counted, and the IME placed, in whole rows.
    pub(crate) fn sidebar_field(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        parts: Vec<Element>,
        focused: bool,
        width: f32,
    ) -> Element {
        let dpi = self.dimensions.dpi as f32;
        let one = px(dpi, 1.).max(1.).floor();
        let gap = px(dpi, 2.).floor();
        let cell = metrics.cell_size.height as f64;
        let inner_height = if cell > 0. {
            SIDEBAR_LINE_HEIGHT - f64::from(2. * one + 2. * gap) / cell
        } else {
            SIDEBAR_LINE_HEIGHT
        };
        let margin = px(dpi, 6.);
        let inner = Element::new(font, ElementContent::Children(parts))
            .display(DisplayType::Block)
            .line_height(Some(inner_height))
            .padding(BoxDimension {
                left: Dimension::Pixels(px(dpi, 6.)),
                right: Dimension::Pixels(px(dpi, 6.)),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .border_corners(Some(rounded(px(dpi, 5.))))
            .min_width(Some(Dimension::Pixels(self.sidebar_field_room(width))))
            .colors(fill(lin(palette::RAISED), lin(palette::TEXT)));
        let edge = if focused {
            lin(palette::ACCENT)
        } else {
            lin(palette::INPUT_EDGE)
        };
        field_box(font, inner, edge, dpi).margin(BoxDimension {
            left: Dimension::Pixels(margin),
            right: Dimension::Pixels(margin),
            top: Dimension::Pixels(gap),
            bottom: Dimension::Pixels(gap),
        })
    }

    /// Room for text inside a `sidebar_field` across a panel `width` wide.
    pub(crate) fn sidebar_field_room(&self, width: f32) -> f32 {
        let dpi = self.dimensions.dpi as f32;
        let one = px(dpi, 1.).max(1.).floor();
        (width - px(dpi, 6.) * 2. - px(dpi, 6.) * 2. - one * 2.).max(0.)
    }

    /// A row of the panel: inset, rounded, lit under the pointer, and when
    /// `selected` filled with the selection colour and marked by an accent
    /// bar down its left side. `indent` shifts the content, not the row.
    pub(crate) fn sidebar_row_shell(
        &self,
        font: &Rc<LoadedFont>,
        parts: Vec<Element>,
        item_type: UIItemType,
        selected: bool,
        indent: f32,
        width: f32,
        pad: f32,
    ) -> Element {
        let dpi = self.dimensions.dpi as f32;
        let (bg, fg) = if selected {
            (lin(palette::ROW_SELECTED), lin(palette::TEXT_STRONG))
        } else {
            (lin(palette::PANEL), lin(palette::TEXT))
        };
        let margin = px(dpi, 6.);
        // The selected row's accent bar is its left border; other rows draw
        // that border in their own fill, so that nothing shifts.
        let bar = Self::sidebar_row_bar(dpi);
        let edge = if selected { lin(palette::ACCENT) } else { bg };
        Element::new(font, ElementContent::Children(parts))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .item_type(item_type)
            .margin(BoxDimension {
                left: Dimension::Pixels(margin),
                right: Dimension::Pixels(margin),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .padding(BoxDimension {
                left: Dimension::Pixels(pad + indent),
                right: Dimension::Pixels(pad),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .border(BoxDimension {
                left: Dimension::Pixels(bar),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .border_corners(Some(rounded(px(dpi, 6.))))
            .min_width(Some(Dimension::Pixels(
                self.sidebar_row_room(width, indent, pad),
            )))
            .colors(ElementColors {
                border: BorderColor {
                    left: edge,
                    top: bg,
                    right: bg,
                    bottom: bg,
                },
                bg: bg.into(),
                text: fg.into(),
            })
            .hover_colors(if selected {
                None
            } else {
                Some(fill(lin(palette::ROW_HOVER), lin(palette::TEXT_STRONG)))
            })
    }

    fn sidebar_row_bar(dpi: f32) -> f32 {
        px(dpi, 2.).max(1.).floor()
    }

    /// Room for a row's content across a panel `width` wide.
    pub(crate) fn sidebar_row_room(&self, width: f32, indent: f32, pad: f32) -> f32 {
        let dpi = self.dimensions.dpi as f32;
        (width - px(dpi, 6.) * 2. - Self::sidebar_row_bar(dpi) - pad * 2. - indent).max(0.)
    }

    /// The panel's background with `children` stacked down it.
    ///
    /// `item_type` is what a click on the empty area below them hits.
    pub(crate) fn sidebar_panel(
        &self,
        children: Vec<Element>,
        item_type: Option<UIItemType>,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        bounds: RectF,
    ) -> anyhow::Result<ComputedElement> {
        let mut container = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: lin(palette::PANEL).into(),
                text: lin(palette::TEXT).into(),
            });
        let _ = colors;
        if let Some(item_type) = item_type {
            container = container.item_type(item_type);
        }

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
            &container,
        )
    }

    /// The column of page tabs down the sidebar's left edge: an icon over a
    /// short label, the current page lit and marked by an accent bar.
    ///
    /// It is all that is left when the sidebar is collapsed, so a click on
    /// either tab brings the panel back on that page.
    fn sidebar_rail(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        bounds: RectF,
    ) -> anyhow::Result<ComputedElement> {
        use crate::termwindow::theme::{icon, lin, palette, px, rounded};
        let expanded = self.sidebar_is_expanded();
        let dpi = self.dimensions.dpi as f32;
        let context = LayoutContext {
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
        };
        // box_model has no centring, so each glyph is padded in by half of
        // what it leaves of the rail. Its width comes from shaping it: CJK
        // advances in the title font are not a whole number of its cells.
        let glyph_width = |text: &str| -> f32 {
            self.compute_element(
                &context,
                &Element::new(font, ElementContent::Text(text.to_string())),
            )
            .map(|e| e.bounds.width())
            .unwrap_or(0.)
        };

        // Each tab sits inset in the rail, with a bar's width of border down
        // its left side that only the current tab colours in.
        let margin = px(dpi, 3.).floor();
        let bar = px(dpi, 3.).floor();
        let inner = (bounds.width() - margin * 2. - bar).max(0.);
        let line = |text: &str| {
            let side = ((inner - glyph_width(text)) / 2.).max(0.).floor();
            Element::new(font, ElementContent::Text(text.to_string()))
                .display(DisplayType::Block)
                .padding(BoxDimension {
                    left: Dimension::Pixels(side),
                    right: Dimension::Pixels(0.),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: InheritableColor::Inherited,
                })
        };

        let tabs = [
            (SidebarPage::Sessions, icon::TERMINAL, "会话"),
            (SidebarPage::Files, icon::FOLDER_OPEN, "文件"),
        ]
        .iter()
        .map(|&(page, glyph, label)| {
            // Marked only while the panel is actually showing it.
            let current = expanded && self.sidebar.page == page;
            let (bg, fg, edge) = if current {
                (
                    lin(palette::RAIL_ACTIVE),
                    lin(palette::TEXT_STRONG),
                    lin(palette::ACCENT),
                )
            } else {
                let base = lin(palette::BASE);
                (base, lin(palette::RAIL_TEXT), base)
            };
            let hover = lin(palette::ROW_HOVER);
            Element::new(
                font,
                ElementContent::Children(vec![line(glyph), line(label)]),
            )
            .display(DisplayType::Block)
            .item_type(UIItemType::SidebarPageTab(page))
            .margin(BoxDimension {
                left: Dimension::Pixels(margin),
                right: Dimension::Pixels(margin),
                top: Dimension::Pixels(margin),
                bottom: Dimension::Pixels(0.),
            })
            .padding(BoxDimension {
                left: Dimension::Pixels(0.),
                right: Dimension::Pixels(0.),
                top: Dimension::Cells(0.3),
                bottom: Dimension::Cells(0.3),
            })
            .border(BoxDimension {
                left: Dimension::Pixels(bar),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            // The corners take the top and bottom border colour, so those
            // stay the fill while the left one is the bar.
            .border_corners(Some(rounded(px(dpi, 6.))))
            .min_width(Some(Dimension::Pixels(inner)))
            .colors(ElementColors {
                border: BorderColor {
                    left: edge,
                    top: bg,
                    right: bg,
                    bottom: bg,
                },
                bg: bg.into(),
                text: fg.into(),
            })
            .hover_colors(if current {
                None
            } else {
                Some(ElementColors {
                    border: BorderColor::new(hover),
                    bg: hover.into(),
                    text: lin(palette::TEXT).into(),
                })
            })
        })
        .collect();

        let rail = Element::new(font, ElementContent::Children(tabs))
            .display(DisplayType::Block)
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())))
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: lin(palette::BASE).into(),
                text: lin(palette::RAIL_TEXT).into(),
            });

        self.compute_element(&context, &rail)
    }

    /// A non-interactive line of explanatory text.
    pub(crate) fn sidebar_hint_element(
        &self,
        text: &str,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        let _ = colors;
        let indent = px(self.dimensions.dpi as f32, 6.);
        Element::new(font, ElementContent::Text(text.to_string()))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .padding(BoxDimension {
                left: Dimension::Pixels(pad + indent),
                right: Dimension::Pixels(pad),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels((width - pad * 2. - indent).max(0.))))
            .colors(fill(lin(palette::PANEL), lin(palette::TEXT_FAINT)))
    }

    fn sidebar_row_element(
        &self,
        row: &VisibleRow,
        row_icon: RowIcon,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        width: f32,
        pad: f32,
    ) -> Element {
        let dpi = self.dimensions.dpi as f32;
        let renaming = self.sidebar.focus == Some(SidebarFocus::Rename(row.id));
        let selected = renaming || self.sidebar.selected == Some(row.id);

        let name = if renaming {
            let composing = self.sidebar.composing.as_deref().unwrap_or("");
            format!("{}{composing}_", self.sidebar.edit_buffer)
        } else {
            row.name.to_string()
        };

        // A node whose last launch failed is drawn in red, so that clicking it
        // and getting nothing doesn't just look like the sidebar is broken.
        let name_color = if self.sidebar.failed.contains(&row.id) {
            lin(palette::DANGER).into()
        } else {
            InheritableColor::Inherited
        };

        let chevron = match row.kind {
            RowKind::Folder { expanded: true } => Some(icon::CHEVRON_DOWN),
            RowKind::Folder { expanded: false } => Some(icon::CHEVRON_RIGHT),
            RowKind::Session => None,
        };
        let chevron = match chevron {
            Some(chevron) => text_run(font, format!("{chevron} "), lin(palette::TEXT_FAINT).into()),
            // A gap as wide as the arrow, so that names line up under
            // folders. (Transparent text would still be drawn.)
            None => {
                let width = self
                    .files_text_width(&format!("{} ", icon::CHEVRON_RIGHT))
                    .unwrap_or(0.);
                text_run(font, String::new(), InheritableColor::Inherited)
                    .min_width(Some(Dimension::Pixels(width)))
            }
        };
        let (glyph, glyph_color) = row_icon.glyph();

        let mut parts = vec![
            chevron,
            text_run(font, format!("{glyph} "), lin(glyph_color).into()),
            text_run(font, name, name_color),
        ];

        // How many terminals this session is holding. Sessions keep running in
        // the background once opened, so without this there is no way to see
        // what is still alive behind the tree.
        let running = match row.kind {
            RowKind::Session => self.session_terminal_count(row.id),
            RowKind::Folder { .. } => 0,
        };
        if running > 0 {
            let pill = lin(palette::PILL);
            parts.push(
                Element::new(
                    font,
                    ElementContent::Children(vec![
                        text_run(font, format!("{} ", icon::DOT), lin(palette::OK).into()),
                        text_run(font, format!("{running}"), lin(palette::TEXT).into()),
                    ]),
                )
                .float(Float::Right)
                .padding(BoxDimension {
                    left: Dimension::Pixels(px(dpi, 6.)),
                    right: Dimension::Pixels(px(dpi, 6.)),
                    top: Dimension::Pixels(0.),
                    bottom: Dimension::Pixels(0.),
                })
                .border_corners(Some(rounded(px(dpi, 7.))))
                .colors(fill(pill, lin(palette::TEXT))),
            );
        }

        let indent = row.depth as f32 * metrics.cell_size.width as f32 * INDENT_CELLS;
        self.sidebar_row_shell(
            font,
            parts,
            UIItemType::SidebarNode(row.id),
            selected,
            indent,
            width,
            pad,
        )
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

        // A hairline divider at rest, over a wider strip that takes the drag;
        // the whole strip lights up while it is being dragged.
        let _ = colors;
        let dragging = self.sidebar.pending_width.is_some();
        let line = px(self.dimensions.dpi as f32, 1.).max(1.).floor();
        let element = Element::new(font, ElementContent::Text(String::new()))
            .display(DisplayType::Block)
            .item_type(UIItemType::SidebarResizeHandle)
            .min_width(Some(Dimension::Pixels(RESIZE_HANDLE_WIDTH - line)))
            .min_height(Some(Dimension::Pixels(handle_bounds.height())))
            .border(BoxDimension {
                left: Dimension::Pixels(0.),
                right: Dimension::Pixels(line),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .colors(ElementColors {
                border: BorderColor::new(if dragging {
                    lin(palette::ACCENT)
                } else {
                    lin(palette::DIVIDER)
                }),
                bg: if dragging {
                    lin(palette::ACCENT).into()
                } else {
                    lin(palette::PANEL).into()
                },
                text: lin(palette::TEXT).into(),
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
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let entries = menu
            .items
            .iter()
            .map(|action| MenuEntry {
                icon: action.icon(),
                label: action.label(),
                group: action.group(),
                danger: action.danger(),
                item: UIItemType::SidebarMenuItem(*action),
            })
            .collect();
        self.sidebar_popup(entries, menu.x, menu.y, font, metrics)
    }

    /// A pop-up menu at (x, y), kept on screen, with its shadow: the
    /// shadow's elements come first, so that the menu is drawn over them.
    pub(crate) fn sidebar_popup(
        &self,
        entries: Vec<MenuEntry>,
        x: f32,
        y: f32,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let dpi = self.dimensions.dpi as f32;
        let ring = px(dpi, 1.).max(1.).floor();
        let inner_pad = px(dpi, 4.);
        let item_pad = px(dpi, 9.);
        let sep_height = px(dpi, 9.).round();
        let row_height = self.sidebar_row_height();

        let label_of = |entry: &MenuEntry| format!("{}  {}", entry.icon, entry.label);
        let widest = entries
            .iter()
            .map(|entry| self.files_text_width(&label_of(entry)).unwrap_or(0.))
            .fold(0., f32::max);
        let item_width = widest.ceil() + px(dpi, 12.);
        let rows = menu_rows(entries);
        let menu_width = item_width + item_pad * 2. + inner_pad * 2. + ring * 2.;
        let menu_height = menu_height(&rows, row_height, sep_height, inner_pad) + ring * 2.;

        let raised = lin(palette::RAISED);
        let white = lin(palette::WHITE);
        let children = rows
            .iter()
            .map(|row| match row {
                MenuRow::Item(entry) => {
                    let fg = if entry.danger {
                        lin(palette::DANGER)
                    } else {
                        lin(palette::MENU_TEXT)
                    };
                    // Icon and label both inherit the entry's colours, so that
                    // both turn white under the pointer.
                    Element::new(
                        font,
                        ElementContent::Children(vec![
                            text_run(
                                font,
                                format!("{}  ", entry.icon),
                                InheritableColor::Inherited,
                            ),
                            text_run(font, entry.label.to_string(), InheritableColor::Inherited),
                        ]),
                    )
                    .display(DisplayType::Block)
                    .line_height(Some(SIDEBAR_LINE_HEIGHT))
                    .item_type(entry.item.clone())
                    .padding(BoxDimension {
                        left: Dimension::Pixels(item_pad),
                        right: Dimension::Pixels(item_pad),
                        top: Dimension::Pixels(0.),
                        bottom: Dimension::Pixels(0.),
                    })
                    .border_corners(Some(rounded(px(dpi, 5.))))
                    .min_width(Some(Dimension::Pixels(item_width)))
                    .colors(fill(raised, fg))
                    .hover_colors(Some(fill(lin(palette::MENU_HOVER), white)))
                }
                // A hairline across the middle of its own short row.
                MenuRow::Separator => {
                    let line = px(dpi, 1.).max(1.).floor();
                    let above = ((sep_height - line) / 2.).floor();
                    Element::new(font, ElementContent::Children(vec![]))
                        .display(DisplayType::Block)
                        .margin(BoxDimension {
                            left: Dimension::Pixels(px(dpi, 6.)),
                            right: Dimension::Pixels(px(dpi, 6.)),
                            top: Dimension::Pixels(above),
                            bottom: Dimension::Pixels(sep_height - line - above),
                        })
                        .border(BoxDimension {
                            left: Dimension::Pixels(0.),
                            right: Dimension::Pixels(0.),
                            top: Dimension::Pixels(line),
                            bottom: Dimension::Pixels(0.),
                        })
                        .min_width(Some(Dimension::Pixels(
                            (item_width + item_pad * 2. - px(dpi, 12.)).max(0.),
                        )))
                        .colors(ElementColors {
                            border: BorderColor::new(lin(palette::MENU_SEPARATOR)),
                            bg: LinearRgba::TRANSPARENT.into(),
                            text: raised.into(),
                        })
                }
            })
            .collect();

        // Keep the menu on screen when it is opened near an edge.
        let window_width = self.dimensions.pixel_width as f32;
        let window_height = self.dimensions.pixel_height as f32;
        let x = x.min((window_width - menu_width).max(0.));
        let y = y.min((window_height - menu_height).max(0.));
        let bounds = euclid::rect(x, y, menu_width, menu_height);

        let inner = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .padding(BoxDimension::new(Dimension::Pixels(inner_pad)))
            .border_corners(Some(rounded(px(dpi, 7.))))
            .colors(fill(raised, lin(palette::MENU_TEXT)));
        let element = Element::new(font, ElementContent::Children(vec![inner]))
            .display(DisplayType::Block)
            .item_type(UIItemType::PopupBackground)
            .padding(BoxDimension::new(Dimension::Pixels(ring)))
            .border_corners(Some(rounded(px(dpi, 8.))))
            .colors(fill(lin(palette::CARD_EDGE), lin(palette::MENU_TEXT)));

        let mut computed = self.theme_shadow(bounds, px(dpi, 8.), 12)?;
        computed.push(self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi,
                    pixel_max: window_height,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi,
                    pixel_max: bounds.max_x(),
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds,
                metrics,
                gl_state: self.render_state.as_ref().unwrap(),
                zindex: 12,
            },
            &element,
        )?);
        Ok(computed)
    }
}
