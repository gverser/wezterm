//! The ssh status bar: one row under the terminal area showing the server
//! the active pane is connected to.

use crate::sessions::monitor::{
    age_text, bar_text, ensure_monitor, is_authenticated, monitor_view, BarText,
};
use crate::sessions::monitor_proto::{status_segments, Level, SegmentKind};
use crate::termwindow::box_model::*;
use crate::termwindow::DimensionContext;
use crate::utilsprites::RenderMetrics;
use config::Dimension;
use mux::localpane::LocalPane;
use std::time::Instant;
use termwiz::cell::unicode_column_width;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

const SEPARATOR: &str = " │ ";

impl crate::TermWindow {
    /// The band the status bar occupies, in window pixels.
    pub fn status_bar_bounds(&self) -> euclid::Rect<f32, ::window::PixelUnit> {
        let border = self.get_os_border();
        let height = self.status_bar_pixel_height();
        let left = border.left.get() as f32;
        let width = self.dimensions.pixel_width as f32 - left - border.right.get() as f32;
        euclid::rect(
            left,
            self.dimensions.pixel_height as f32 - border.bottom.get() as f32,
            width.max(0.),
            height,
        )
    }

    pub fn paint_status_bar(&mut self) -> anyhow::Result<()> {
        let Some(domain_id) = self.status_bar_domain() else {
            return Ok(());
        };
        ensure_monitor(domain_id);

        let bounds = self.status_bar_bounds();
        if bounds.width() <= 0. || bounds.height() <= 0. {
            return Ok(());
        }

        let cell_width = self.render_metrics.cell_size.width as f32;
        let max_cells = ((bounds.width() / cell_width) as usize).saturating_sub(2);

        // (text, colour) runs, left to right.
        let palette = self.palette().clone();
        let color = |idx: u8| -> LinearRgba {
            palette
                .resolve_fg(ColorAttribute::PaletteIndex(idx))
                .to_linear()
        };
        let colors = self.sidebar_colors();
        let dim = colors.inactive_tab().fg_color.to_linear();
        let bg = colors.background().to_linear();

        // A pane held open after its connection failed or dropped.
        let pane_ended = self
            .get_active_pane_no_overlay()
            .and_then(|pane| {
                pane.downcast_ref::<LocalPane>()
                    .map(|local| local.process_ended())
            })
            .unwrap_or(false);
        let view = monitor_view(domain_id);
        let text = bar_text(
            pane_ended,
            is_authenticated(domain_id),
            view.as_ref(),
            Instant::now(),
        );

        let mut runs: Vec<(String, LinearRgba)> = vec![];
        let mut say = |text: &str, c: LinearRgba| runs.push((text.to_string(), c));
        match text {
            BarText::Connecting => say("正在连接…", dim),
            BarText::ConnectFailed => say("连接失败", color(9)),
            BarText::Disconnected => say("连接已断开", color(9)),
            BarText::Fetching => say("正在获取服务器状态…", dim),
            BarText::Unsupported => say("仅支持 Linux 服务器", color(11)),
            BarText::Unavailable => say("状态不可用", color(9)),
            BarText::Values { stale_for } => {
                let stale = stale_for.map(|secs| format!("数据已过期（{}）", age_text(secs)));
                // Room for the note, so that the numbers give way to it.
                let note_cells = stale.as_ref().map_or(0, |note| {
                    unicode_column_width(note, None) + unicode_column_width(SEPARATOR, None)
                });
                let status = view.and_then(|view| view.status);
                let segments = status
                    .as_ref()
                    .map(|status| status_segments(status, max_cells.saturating_sub(note_cells)))
                    .unwrap_or_default();
                for (i, seg) in segments.into_iter().enumerate() {
                    if i > 0 {
                        runs.push((SEPARATOR.to_string(), dim));
                    }
                    let c = match (stale_for, seg.level) {
                        // Old numbers are greyed out, warnings and all.
                        (Some(_), _) => dim,
                        (None, Level::Crit) => color(9),
                        (None, Level::Warn) => color(11),
                        (None, Level::Normal) => match seg.kind {
                            SegmentKind::Host => color(14),
                            SegmentKind::Cpu => color(10),
                            SegmentKind::Mem => color(13),
                            SegmentKind::Net => color(12),
                            SegmentKind::Uptime => color(7),
                            SegmentKind::Users => color(6),
                            SegmentKind::Disks => color(10),
                        },
                    };
                    runs.push((seg.text, c));
                }
                if let Some(note) = stale {
                    if !runs.is_empty() {
                        runs.push((SEPARATOR.to_string(), dim));
                    }
                    runs.push((note, color(11)));
                }
            }
        }

        let key = format!(
            "{:?}|{}|{}|{}|{}",
            runs.iter()
                .map(|(t, c)| format!("{t}{:?}", c.0))
                .collect::<Vec<_>>(),
            bounds.origin.x,
            bounds.origin.y,
            bounds.width(),
            bounds.height()
        );
        let stale = match &self.status_bar_computed {
            Some((cached, _)) => *cached != key,
            None => true,
        };
        if stale {
            let computed = self.build_status_bar(&runs, bounds, bg)?;
            self.status_bar_computed = Some((key, computed));
        }

        let (key, computed) = self
            .status_bar_computed
            .take()
            .expect("status bar cache was just filled");
        for element in &computed {
            let gl_state = self.render_state.as_ref().unwrap();
            self.render_element(element, gl_state, None)?;
        }
        self.status_bar_computed = Some((key, computed));
        Ok(())
    }

    fn build_status_bar(
        &self,
        runs: &[(String, LinearRgba)],
        bounds: euclid::Rect<f32, ::window::PixelUnit>,
        bg: LinearRgba,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let font = self.fonts.default_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let children = runs
            .iter()
            .map(|(text, fg)| {
                Element::new(&font, ElementContent::Text(text.clone())).colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: (*fg).into(),
                })
            })
            .collect();

        let bar = Element::new(&font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::default(),
                bg: bg.into(),
                text: bg.into(),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(1.),
                right: Dimension::Cells(1.),
                top: Dimension::Cells(0.),
                bottom: Dimension::Cells(0.),
            })
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels(bounds.height())));

        let computed = self.compute_element(
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
            &bar,
        )?;
        Ok(vec![computed])
    }
}
