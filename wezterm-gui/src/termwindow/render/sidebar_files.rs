//! Drawing for the sidebar's file page.

use crate::sftp::dialogs::can_pick_files;
use crate::sftp::model::{display_size, elide_left, elide_right, wrap_cells, FileEntry};
use crate::sftp::transfer::CANCELLED;
use crate::sftp::{FilesAction, FilesConn};
use crate::termwindow::box_model::*;
use crate::termwindow::sidebar::{SidebarFocus, SIDEBAR_LINE_HEIGHT};
use crate::termwindow::UIItemType;
use crate::utilsprites::RenderMetrics;
use ::window::RectF;
use config::{Dimension, DimensionContext, TabBarColors};
use std::rc::Rc;
use termwiz::cell::unicode_column_width;
use wezterm_font::LoadedFont;
use wezterm_term::color::{ColorAttribute, ColorPalette};
use window::color::LinearRgba;

/// Room given to text being measured, so that nothing clips it.
const MEASURE_WIDTH: f32 = 100_000.;

const TOOLBAR: [FilesAction; 5] = [
    FilesAction::Up,
    FilesAction::Refresh,
    FilesAction::Home,
    FilesAction::NewFolder,
    FilesAction::Upload,
];

fn padded(element: Element, pad: f32) -> Element {
    element.padding(BoxDimension {
        left: Dimension::Pixels(pad),
        right: Dimension::Pixels(pad),
        top: Dimension::Pixels(0.),
        bottom: Dimension::Pixels(0.),
    })
}

fn plain_colors(bg: LinearRgba, fg: LinearRgba) -> ElementColors {
    ElementColors {
        border: BorderColor::default(),
        bg: bg.into(),
        text: fg.into(),
    }
}

impl crate::TermWindow {
    /// Width of `text` in the sidebar's font, in pixels.
    pub(crate) fn files_text_width(&self, text: &str) -> Option<f32> {
        let (font, metrics) = self.sidebar_font().ok()?;
        let bounds = self.sidebar_panel_bounds();
        let context = LayoutContext {
            height: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                pixel_max: bounds.height(),
                pixel_cell: metrics.cell_size.height as f32,
            },
            width: DimensionContext {
                dpi: self.dimensions.dpi as f32,
                // Don't let the panel's edge clip what is being measured.
                pixel_max: MEASURE_WIDTH,
                pixel_cell: metrics.cell_size.width as f32,
            },
            bounds: euclid::rect(0., 0., MEASURE_WIDTH, bounds.height()),
            metrics: &metrics,
            gl_state: self.render_state.as_ref()?,
            zindex: 10,
        };
        self.compute_element(
            &context,
            &Element::new(&font, ElementContent::Text(text.to_string())),
        )
        .ok()
        .map(|e| e.bounds.width())
    }

    /// Room for text across the panel, inside its padding.
    pub(crate) fn files_inner_width(&self) -> f32 {
        let pad = match self.sidebar_font() {
            Ok((_, metrics)) => metrics.cell_size.width as f32 * 0.5,
            Err(_) => 0.,
        };
        (self.sidebar_panel_bounds().width() - pad * 2.).max(0.)
    }

    /// `text` shortened by `shorten` (which keeps a number of columns) until
    /// it fits in `max_width` pixels.
    ///
    /// The sidebar's font is proportional, so columns say little about
    /// pixels: start from the share of the columns that the overflow
    /// suggests, then measure and trim.
    fn files_fit(&self, text: &str, max_width: f32, shorten: fn(&str, usize) -> String) -> String {
        let Some(full) = self.files_text_width(text) else {
            return text.to_string();
        };
        if full <= max_width {
            return text.to_string();
        }
        let columns = unicode_column_width(text, None);
        let mut keep = ((columns as f32) * max_width / full).floor() as usize;
        loop {
            let candidate = shorten(text, keep);
            match self.files_text_width(&candidate) {
                Some(width) if width > max_width && keep > 0 => keep -= 1,
                _ => return candidate,
            }
        }
    }

    /// `text` broken into lines that each fit in `max_width` pixels.
    pub(crate) fn files_wrap(&self, text: &str, max_width: f32) -> Vec<String> {
        let Some(full) = self.files_text_width(text) else {
            return vec![text.to_string()];
        };
        if full <= max_width || full <= 0. {
            return vec![text.to_string()];
        }
        // Lines of the average column width; a little short, so that a line
        // heavy in wide glyphs still fits.
        let columns = unicode_column_width(text, None) as f32;
        let per_line = ((columns * max_width / full) * 0.9).floor().max(1.) as usize;
        wrap_cells(text, per_line)
    }

    /// The file page's panel: the path bar, the buttons, the follow
    /// checkbox, and the listing of the active pane's connection.
    pub(crate) fn build_files_page(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        palette: &ColorPalette,
        bounds: RectF,
        pad: f32,
    ) -> anyhow::Result<ComputedElement> {
        let width = bounds.width();
        let red = palette
            .resolve_fg(ColorAttribute::PaletteIndex(1))
            .to_linear();

        let mut children = vec![];
        match self.files_conn() {
            None => {
                let hint = if self.files_target().is_some() {
                    "正在连接…"
                } else {
                    "当前窗格不是 SSH 会话"
                };
                children.push(self.sidebar_hint_element(hint, font, colors, width, pad));
            }
            Some(conn) => {
                children.push(self.files_path_element(conn, font, colors, palette, width, pad));
                children.push(self.files_toolbar_element(font, colors, width, pad));
                children.push(self.files_follow_element(conn, font, colors, width, pad));
                for line in self.files_notice_lines(conn) {
                    children.push(self.files_text_line(&line, red, font, colors, width, pad));
                }

                if let Some(err) = &conn.error {
                    for line in self.files_wrap(err, self.files_inner_width()) {
                        children.push(self.files_text_line(&line, red, font, colors, width, pad));
                    }
                } else if conn.entries.is_empty() {
                    let hint = if conn.loading {
                        "加载中…"
                    } else {
                        "（空目录）"
                    };
                    children.push(self.sidebar_hint_element(hint, font, colors, width, pad));
                } else {
                    let capacity = self.files_list_capacity();
                    let end = (conn.scroll + capacity).min(conn.entries.len());
                    for index in conn.scroll..end {
                        children.push(self.files_row_element(
                            index,
                            &conn.entries[index],
                            conn.selected == Some(index),
                            font,
                            metrics,
                            colors,
                            width,
                            pad,
                        ));
                    }
                }
            }
        }

        self.sidebar_panel(
            children,
            Some(UIItemType::FilesBackground),
            font,
            metrics,
            colors,
            bounds,
        )
    }

    /// Width of the ✕ beside the transfer line, padding included.
    pub(crate) fn files_cancel_width(&self) -> f32 {
        let pad = match self.sidebar_font() {
            Ok((_, metrics)) => metrics.cell_size.width as f32 * 0.5,
            Err(_) => 0.,
        };
        self.files_text_width(FilesAction::CancelTransfer.label())
            .unwrap_or(0.)
            + pad * 2.
    }

    /// The transfer line along the bottom of the page, with its ✕.
    pub(crate) fn files_transfer_element(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        colors: &TabBarColors,
        palette: &ColorPalette,
        panel: RectF,
        pad: f32,
    ) -> anyhow::Result<Option<ComputedElement>> {
        let lines = self.files_transfer_lines();
        if lines.is_empty() {
            return Ok(None);
        }
        let row = self.sidebar_row_height();
        let height = (lines.len() as f32 * row).min(panel.height());
        let bounds = euclid::rect(panel.min_x(), panel.max_y() - height, panel.width(), height);

        let queue = &self.files.transfers;
        let failed = !queue.is_busy()
            && queue.note.is_none()
            && matches!(&queue.last_result, Some((_, _, Err(err))) if err != CANCELLED);
        let band = colors.inactive_tab();
        let fg = if failed {
            palette
                .resolve_fg(ColorAttribute::PaletteIndex(1))
                .to_linear()
        } else {
            band.fg_color.to_linear()
        };
        let hover = colors.inactive_tab_hover();

        let children = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let mut parts = vec![Element::new(font, ElementContent::Text(line))
                    .colors(plain_colors(band.bg_color.to_linear(), fg))];
                if i == 0 {
                    parts.push(
                        padded(
                            Element::new(
                                font,
                                ElementContent::Text(
                                    FilesAction::CancelTransfer.label().to_string(),
                                ),
                            ),
                            pad,
                        )
                        .float(Float::Right)
                        .item_type(UIItemType::FilesToolbar(FilesAction::CancelTransfer))
                        .colors(plain_colors(
                            band.bg_color.to_linear(),
                            band.fg_color.to_linear(),
                        ))
                        .hover_colors(Some(plain_colors(
                            hover.bg_color.to_linear(),
                            hover.fg_color.to_linear(),
                        ))),
                    );
                }
                padded(Element::new(font, ElementContent::Children(parts)), pad)
                    .display(DisplayType::Block)
                    .line_height(Some(SIDEBAR_LINE_HEIGHT))
                    .min_width(Some(Dimension::Pixels((panel.width() - pad * 2.).max(0.))))
                    .colors(plain_colors(band.bg_color.to_linear(), fg))
            })
            .collect();

        let element = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())))
            .colors(plain_colors(band.bg_color.to_linear(), fg));

        Ok(Some(self.compute_element(
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
            &element,
        )?))
    }

    /// The page's right-click menu and its shadow, if it is open.
    pub(crate) fn files_menu_element(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let Some(menu) = self.files.menu.as_ref() else {
            return Ok(vec![]);
        };
        let entries = menu
            .items
            .iter()
            .map(|action| crate::termwindow::theme::MenuEntry {
                icon: action.icon(),
                label: action.label(),
                group: action.group(),
                danger: action.danger(),
                item: UIItemType::FilesMenuItem(*action),
            })
            .collect();
        self.sidebar_popup(entries, menu.x, menu.y, font, metrics)
    }

    fn files_path_element(
        &self,
        conn: &FilesConn,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        palette: &ColorPalette,
        width: f32,
        pad: f32,
    ) -> Element {
        let editing = self.sidebar.focus == Some(SidebarFocus::FilesPath);
        let text = match (&self.files.path_edit, editing) {
            (Some(text), true) => {
                let composing = self.sidebar.composing.as_deref().unwrap_or("");
                let text = crate::sftp::model::display_path(text);
                self.files_fit(
                    &format!("{text}{composing}_"),
                    self.files_inner_width(),
                    elide_left,
                )
            }
            _ if conn.cwd.is_empty() => "…".to_string(),
            _ => self.files_fit(
                &crate::sftp::model::display_path(&conn.cwd),
                self.files_inner_width(),
                elide_left,
            ),
        };
        let style = if editing {
            colors.active_tab()
        } else {
            colors.inactive_tab()
        };
        let hover = colors.inactive_tab_hover();
        // Drawn as a selection while typing would replace it.
        let selected = editing && self.files.path_fresh;
        let text_element = Element::new(font, ElementContent::Text(text));
        let text_element = if selected {
            text_element.colors(plain_colors(
                palette.selection_bg.to_linear(),
                palette.selection_fg.to_linear(),
            ))
        } else {
            text_element
        };
        padded(
            Element::new(font, ElementContent::Children(vec![text_element])),
            pad,
        )
        .display(DisplayType::Block)
        .line_height(Some(SIDEBAR_LINE_HEIGHT))
        .item_type(UIItemType::FilesPath)
        .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
        .colors(plain_colors(
            style.bg_color.to_linear(),
            style.fg_color.to_linear(),
        ))
        .hover_colors(if editing {
            None
        } else {
            Some(plain_colors(
                hover.bg_color.to_linear(),
                hover.fg_color.to_linear(),
            ))
        })
    }

    fn files_toolbar_element(
        &self,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        let normal = colors.inactive_tab();
        let hover = colors.inactive_tab_hover();
        let buttons = TOOLBAR
            .iter()
            // Nothing to pick files with where there is no dialog; dropping
            // them still works.
            .filter(|action| **action != FilesAction::Upload || can_pick_files())
            .map(|action| {
                padded(
                    Element::new(font, ElementContent::Text(action.label().to_string())),
                    pad,
                )
                .item_type(UIItemType::FilesToolbar(*action))
                .colors(plain_colors(
                    colors.background().to_linear(),
                    normal.fg_color.to_linear(),
                ))
                .hover_colors(Some(plain_colors(
                    hover.bg_color.to_linear(),
                    hover.fg_color.to_linear(),
                )))
            })
            .collect();
        Element::new(font, ElementContent::Children(buttons))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .min_width(Some(Dimension::Pixels(width)))
            .colors(plain_colors(
                colors.background().to_linear(),
                normal.fg_color.to_linear(),
            ))
    }

    fn files_follow_element(
        &self,
        conn: &FilesConn,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        let normal = colors.inactive_tab();
        let hover = colors.inactive_tab_hover();
        let text = if conn.follow {
            "☑ 跟随终端"
        } else {
            "☐ 跟随终端"
        };
        padded(
            Element::new(font, ElementContent::Text(text.to_string())),
            pad,
        )
        .display(DisplayType::Block)
        .line_height(Some(SIDEBAR_LINE_HEIGHT))
        .item_type(UIItemType::FilesFollow)
        .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
        .colors(plain_colors(
            colors.background().to_linear(),
            normal.fg_color.to_linear(),
        ))
        .hover_colors(Some(plain_colors(
            hover.bg_color.to_linear(),
            hover.fg_color.to_linear(),
        )))
    }

    fn files_text_line(
        &self,
        text: &str,
        fg: LinearRgba,
        font: &Rc<LoadedFont>,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        padded(
            Element::new(font, ElementContent::Text(text.to_string())),
            pad,
        )
        .display(DisplayType::Block)
        .line_height(Some(SIDEBAR_LINE_HEIGHT))
        .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
        .colors(plain_colors(colors.background().to_linear(), fg))
    }

    fn files_row_element(
        &self,
        index: usize,
        entry: &FileEntry,
        selected: bool,
        font: &Rc<LoadedFont>,
        _metrics: &RenderMetrics,
        colors: &TabBarColors,
        width: f32,
        pad: f32,
    ) -> Element {
        // A link is listed as itself; what it points at is only looked up when
        // it is opened.
        let prefix = if entry.is_dir {
            "▸ "
        } else if entry.is_link {
            "↪ "
        } else {
            "  "
        };
        let style = if selected {
            colors.active_tab()
        } else {
            colors.inactive_tab()
        };
        let hover = colors.inactive_tab_hover();
        let row_colors = plain_colors(style.bg_color.to_linear(), style.fg_color.to_linear());

        // The size floats at the right and the sidebar doesn't clip the name
        // short of it, so make room for it.
        let size = display_size(entry.size);
        let size_text = if size.is_empty() {
            String::new()
        } else {
            format!("  {size}")
        };
        let size_width = self.files_text_width(&size_text).unwrap_or(0.);
        let name = self.files_fit(
            &format!("{prefix}{}", crate::sftp::model::display_name(&entry.name)),
            self.files_inner_width() - size_width,
            elide_right,
        );

        let mut parts =
            vec![Element::new(font, ElementContent::Text(name)).colors(row_colors.clone())];
        if !size.is_empty() {
            parts.push(
                Element::new(font, ElementContent::Text(size_text))
                    .float(Float::Right)
                    .colors(row_colors.clone()),
            );
        }

        padded(Element::new(font, ElementContent::Children(parts)), pad)
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .item_type(UIItemType::FilesRow(index))
            .min_width(Some(Dimension::Pixels((width - pad * 2.).max(0.))))
            .colors(row_colors)
            .hover_colors(Some(plain_colors(
                hover.bg_color.to_linear(),
                hover.fg_color.to_linear(),
            )))
    }
}
