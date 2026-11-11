//! Drawing for the sidebar's file page.

use crate::sftp::dialogs::can_pick_files;
use crate::sftp::model::{display_size, elide_left, elide_right, wrap_cells, FileEntry};
use crate::sftp::transfer::CANCELLED;
use crate::sftp::{FilesAction, FilesConn};
use crate::termwindow::box_model::*;
use crate::termwindow::render::sidebar::text_run;
use crate::termwindow::sidebar::{SidebarFocus, SIDEBAR_LINE_HEIGHT};
use crate::termwindow::theme::{fill, icon, lin, palette, px, rounded};
use crate::termwindow::UIItemType;
use crate::utilsprites::RenderMetrics;
use ::window::RectF;
use config::{Dimension, DimensionContext};
use std::rc::Rc;
use termwiz::cell::unicode_column_width;
use wezterm_font::LoadedFont;
use wezterm_term::color::ColorPalette;
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

/// The icon and its colour for a listing entry.
pub(crate) fn entry_icon(entry: &FileEntry) -> (&'static str, u32) {
    if entry.is_dir {
        (icon::FOLDER, palette::ICON_FOLDER)
    } else if entry.is_link {
        (icon::LINK, palette::ICON_LINK)
    } else {
        (icon::FILE, palette::ICON_FILE)
    }
}

/// How far along a transfer is, when its size is known.
pub(crate) fn progress_fraction(done: u64, total: Option<u64>) -> Option<f32> {
    match total {
        Some(total) if total > 0 => Some((done as f64 / total as f64).min(1.) as f32),
        _ => None,
    }
}

/// The follow checkbox's glyph and colour.
pub(crate) fn follow_box(follow: bool) -> (&'static str, u32) {
    if follow {
        (icon::CHECKBOX_ON, palette::ACCENT)
    } else {
        (icon::CHECKBOX_OFF, palette::TEXT_MUTED)
    }
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
        palette: &ColorPalette,
        bounds: RectF,
        pad: f32,
    ) -> anyhow::Result<ComputedElement> {
        let width = bounds.width();
        let red = lin(palette::DANGER);

        let mut children = vec![];
        match self.files_conn() {
            None => {
                let hint = if self.files_target().is_some() {
                    "正在连接…"
                } else {
                    "当前窗格不是 SSH 会话"
                };
                children.push(self.sidebar_hint_element(hint, font, width, pad));
            }
            Some(conn) => {
                children.push(self.files_path_element(conn, font, metrics, palette, width));
                children.push(self.files_toolbar_element(font, width));
                children.push(self.files_follow_element(conn, font, width, pad));
                for line in self.files_notice_lines(conn) {
                    children.push(self.files_text_line(&line, red, font, width, pad));
                }

                if let Some(err) = &conn.error {
                    for line in self.files_wrap(err, self.files_inner_width()) {
                        children.push(self.files_text_line(&line, red, font, width, pad));
                    }
                } else if conn.entries.is_empty() {
                    let hint = if conn.loading {
                        "加载中…"
                    } else {
                        "（空目录）"
                    };
                    children.push(self.sidebar_hint_element(hint, font, width, pad));
                } else {
                    let capacity = self.files_list_capacity();
                    let end = (conn.scroll + capacity).min(conn.entries.len());
                    for index in conn.scroll..end {
                        children.push(self.files_row_element(
                            index,
                            &conn.entries[index],
                            conn.selected == Some(index),
                            font,
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
            bounds,
        )
    }

    /// Width of the ✕ beside the transfer line, padding included.
    pub(crate) fn files_cancel_width(&self) -> f32 {
        let pad = match self.sidebar_font() {
            Ok((_, metrics)) => metrics.cell_size.width as f32 * 0.5,
            Err(_) => 0.,
        };
        self.files_text_width(FilesAction::CancelTransfer.icon())
            .unwrap_or(0.)
            + pad * 2.
    }

    /// The transfer line along the bottom of the page, with its ✕, and
    /// under it a thin bar showing how far a transfer of known size is.
    pub(crate) fn files_transfer_element(
        &self,
        font: &Rc<LoadedFont>,
        metrics: &RenderMetrics,
        panel: RectF,
        pad: f32,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let lines = self.files_transfer_lines();
        if lines.is_empty() {
            return Ok(vec![]);
        }
        let dpi = self.dimensions.dpi as f32;
        let row = self.sidebar_row_height();
        let height = (lines.len() as f32 * row).min(panel.height());
        let bounds = euclid::rect(panel.min_x(), panel.max_y() - height, panel.width(), height);

        let queue = &self.files.transfers;
        let failed = !queue.is_busy()
            && queue.note.is_none()
            && matches!(&queue.last_result, Some((_, _, Err(err))) if err != CANCELLED);
        let band = lin(palette::PANEL);
        let fg = if failed {
            lin(palette::DANGER)
        } else {
            lin(palette::TEXT_MUTED)
        };

        let children = lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let mut parts = vec![text_run(font, line, fg.into())];
                if i == 0 {
                    parts.push(
                        padded(
                            Element::new(
                                font,
                                ElementContent::Text(
                                    FilesAction::CancelTransfer.icon().to_string(),
                                ),
                            ),
                            pad,
                        )
                        .float(Float::Right)
                        .item_type(UIItemType::FilesToolbar(FilesAction::CancelTransfer))
                        .border_corners(Some(rounded(px(dpi, 6.))))
                        .colors(fill(band, lin(palette::TEXT_MUTED)))
                        .hover_colors(Some(fill(
                            lin(palette::ROW_HOVER),
                            lin(palette::TEXT_STRONG),
                        ))),
                    );
                }
                padded(Element::new(font, ElementContent::Children(parts)), pad)
                    .display(DisplayType::Block)
                    .line_height(Some(SIDEBAR_LINE_HEIGHT))
                    .min_width(Some(Dimension::Pixels((panel.width() - pad * 2.).max(0.))))
                    .colors(plain_colors(band, fg))
            })
            .collect();

        // A divider along the top, drawn inside the band's height.
        let line = px(dpi, 1.).max(1.).floor();
        let element = Element::new(font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .border(BoxDimension {
                left: Dimension::Pixels(0.),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(line),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels((bounds.height() - line).max(0.))))
            .colors(ElementColors {
                border: BorderColor::new(lin(palette::DIVIDER)),
                bg: band.into(),
                text: fg.into(),
            });

        let mut computed = vec![self.files_compute(&element, bounds, metrics)?];

        let fraction = queue
            .active
            .as_ref()
            .and_then(|(_, progress, _)| progress_fraction(progress.done, progress.total));
        if let Some(fraction) = fraction {
            let margin = pad + px(dpi, 6.);
            let thickness = px(dpi, 3.).max(2.).round();
            let track = euclid::rect(
                bounds.min_x() + margin,
                bounds.min_y() + row - thickness - px(dpi, 2.).round(),
                (bounds.width() - margin * 2.).max(0.),
                thickness,
            );
            let done = euclid::rect(
                track.min_x(),
                track.min_y(),
                (track.width() * fraction).max(thickness),
                thickness,
            );
            for (rect, color) in [(track, palette::PILL), (done, palette::ACCENT)] {
                let bar = Element::new(font, ElementContent::Children(vec![]))
                    .display(DisplayType::Block)
                    .min_width(Some(Dimension::Pixels(rect.width())))
                    .min_height(Some(Dimension::Pixels(rect.height())))
                    .border_corners(Some(rounded(thickness / 2.)))
                    .colors(fill(lin(color), lin(color)));
                computed.push(self.files_compute(&bar, rect, metrics)?);
            }
        }
        Ok(computed)
    }

    /// `element` laid out in `bounds`, over the panel.
    fn files_compute(
        &self,
        element: &Element,
        bounds: RectF,
        metrics: &RenderMetrics,
    ) -> anyhow::Result<ComputedElement> {
        self.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: self.dimensions.dpi as f32,
                    pixel_max: bounds.max_y(),
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
            element,
        )
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
        metrics: &RenderMetrics,
        palette_: &ColorPalette,
        width: f32,
    ) -> Element {
        let editing = self.sidebar.focus == Some(SidebarFocus::FilesPath);
        let room = self.sidebar_field_room(width);
        let text = match (&self.files.path_edit, editing) {
            (Some(text), true) => {
                let composing = self.sidebar.composing.as_deref().unwrap_or("");
                let text = crate::sftp::model::display_path(text);
                self.files_fit(&format!("{text}{composing}_"), room, elide_left)
            }
            _ if conn.cwd.is_empty() => "…".to_string(),
            _ => self.files_fit(
                &crate::sftp::model::display_path(&conn.cwd),
                room,
                elide_left,
            ),
        };
        let raised = lin(palette::RAISED);
        // Drawn as a selection while typing would replace it.
        let colors = if editing && self.files.path_fresh {
            plain_colors(
                palette_.selection_bg.to_linear(),
                palette_.selection_fg.to_linear(),
            )
        } else if editing {
            fill(raised, lin(palette::TEXT_STRONG))
        } else {
            fill(raised, lin(palette::TEXT))
        };
        let text_element = Element::new(font, ElementContent::Text(text)).colors(colors);
        self.sidebar_field(font, metrics, vec![text_element], editing, width)
            .item_type(UIItemType::FilesPath)
    }

    fn files_toolbar_element(&self, font: &Rc<LoadedFont>, width: f32) -> Element {
        let dpi = self.dimensions.dpi as f32;
        let panel = lin(palette::PANEL);
        let buttons = TOOLBAR
            .iter()
            // Nothing to pick files with where there is no dialog; dropping
            // them still works.
            .filter(|action| **action != FilesAction::Upload || can_pick_files())
            .map(|action| {
                // Upload is what the page is for, so it says so in words and
                // in the accent colour; the rest are plain icons.
                let (text, fg, hover_fg) = if *action == FilesAction::Upload {
                    (
                        format!("{} 上传", action.icon()),
                        palette::ACCENT,
                        palette::ACCENT,
                    )
                } else {
                    (
                        action.icon().to_string(),
                        palette::TEXT_MUTED,
                        palette::TEXT_STRONG,
                    )
                };
                Element::new(font, ElementContent::Text(text))
                    .item_type(UIItemType::FilesToolbar(*action))
                    .padding(BoxDimension {
                        left: Dimension::Pixels(px(dpi, 6.)),
                        right: Dimension::Pixels(px(dpi, 6.)),
                        top: Dimension::Pixels(0.),
                        bottom: Dimension::Pixels(0.),
                    })
                    .margin(BoxDimension {
                        left: Dimension::Pixels(0.),
                        right: Dimension::Pixels(px(dpi, 2.)),
                        top: Dimension::Pixels(0.),
                        bottom: Dimension::Pixels(0.),
                    })
                    .border_corners(Some(rounded(px(dpi, 6.))))
                    .colors(fill(panel, lin(fg)))
                    .hover_colors(Some(fill(lin(palette::ROW_HOVER), lin(hover_fg))))
            })
            .collect();
        let margin = px(dpi, 6.);
        Element::new(font, ElementContent::Children(buttons))
            .display(DisplayType::Block)
            .line_height(Some(SIDEBAR_LINE_HEIGHT))
            .padding(BoxDimension {
                left: Dimension::Pixels(margin),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(0.),
                bottom: Dimension::Pixels(0.),
            })
            .min_width(Some(Dimension::Pixels((width - margin).max(0.))))
            .colors(fill(panel, lin(palette::TEXT_MUTED)))
    }

    fn files_follow_element(
        &self,
        conn: &FilesConn,
        font: &Rc<LoadedFont>,
        width: f32,
        pad: f32,
    ) -> Element {
        let (glyph, color) = follow_box(conn.follow);
        let parts = vec![
            text_run(font, format!("{glyph} "), lin(color).into()),
            text_run(font, "跟随终端".to_string(), InheritableColor::Inherited),
        ];
        self.sidebar_row_shell(font, parts, UIItemType::FilesFollow, false, 0., width, pad)
    }

    fn files_text_line(
        &self,
        text: &str,
        fg: LinearRgba,
        font: &Rc<LoadedFont>,
        width: f32,
        pad: f32,
    ) -> Element {
        let margin = px(self.dimensions.dpi as f32, 6.);
        padded(
            Element::new(font, ElementContent::Text(text.to_string())),
            pad + margin,
        )
        .display(DisplayType::Block)
        .line_height(Some(SIDEBAR_LINE_HEIGHT))
        .min_width(Some(Dimension::Pixels(
            (width - (pad + margin) * 2.).max(0.),
        )))
        .colors(plain_colors(lin(palette::PANEL), fg))
    }

    fn files_row_element(
        &self,
        index: usize,
        entry: &FileEntry,
        selected: bool,
        font: &Rc<LoadedFont>,
        width: f32,
        pad: f32,
    ) -> Element {
        // A link is listed as itself; what it points at is only looked up when
        // it is opened.
        let (glyph, glyph_color) = entry_icon(entry);
        let icon_text = format!("{glyph} ");

        // The size floats at the right and the sidebar doesn't clip the name
        // short of it, so make room for it.
        let size = display_size(entry.size);
        let size_text = if size.is_empty() {
            String::new()
        } else {
            format!("  {size}")
        };
        let size_width = self.files_text_width(&size_text).unwrap_or(0.);
        let icon_width = self.files_text_width(&icon_text).unwrap_or(0.);
        let name = self.files_fit(
            &crate::sftp::model::display_name(&entry.name),
            self.sidebar_row_room(width, 0., pad) - icon_width - size_width,
            elide_right,
        );

        let mut parts = vec![
            text_run(font, icon_text, lin(glyph_color).into()),
            text_run(font, name, InheritableColor::Inherited),
        ];
        if !size.is_empty() {
            parts.push(
                text_run(font, size_text, lin(palette::TEXT_MUTED).into()).float(Float::Right),
            );
        }
        self.sidebar_row_shell(
            font,
            parts,
            UIItemType::FilesRow(index),
            selected,
            0.,
            width,
            pad,
        )
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::termwindow::theme::{icon, palette};

    #[test]
    fn each_kind_of_entry_has_its_icon() {
        let e = |is_dir, is_link| FileEntry {
            name: "x".to_string(),
            is_dir,
            is_link,
            size: None,
        };
        assert_eq!(
            entry_icon(&e(true, false)),
            (icon::FOLDER, palette::ICON_FOLDER)
        );
        assert_eq!(
            entry_icon(&e(false, true)),
            (icon::LINK, palette::ICON_LINK)
        );
        assert_eq!(
            entry_icon(&e(false, false)),
            (icon::FILE, palette::ICON_FILE)
        );
    }

    #[test]
    fn a_transfer_shows_how_far_along_it_is_only_when_its_size_is_known() {
        assert_eq!(progress_fraction(50, Some(200)), Some(0.25));
        assert_eq!(progress_fraction(300, Some(200)), Some(1.));
        assert_eq!(progress_fraction(0, Some(0)), None);
        assert_eq!(progress_fraction(10, None), None);
    }

    #[test]
    fn the_follow_box_is_an_icon_either_way() {
        assert_eq!(follow_box(true), (icon::CHECKBOX_ON, palette::ACCENT));
        assert_eq!(follow_box(false), (icon::CHECKBOX_OFF, palette::TEXT_MUTED));
    }
}
