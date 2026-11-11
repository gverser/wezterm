//! The ssh status bar: one row under the terminal area showing the server
//! the active pane is connected to.

use crate::sessions::monitor::{
    age_text, bar_text, ensure_monitor, is_authenticated, monitor_view, BarText,
};
use crate::sessions::monitor_proto::{status_segments, Level, SegmentKind};
use crate::termwindow::box_model::*;
use crate::termwindow::theme::{icon, lin, palette, px};
use crate::termwindow::DimensionContext;
use crate::utilsprites::RenderMetrics;
use config::Dimension;
use mux::localpane::LocalPane;
use std::time::Instant;
use termwiz::cell::unicode_column_width;
use window::color::LinearRgba;

/// Between segments: space, not rules, so that the colours carry it.
const SEPARATOR: &str = "   ";

/// A segment's name and value, for the ones that have a name ("CPU 3%"):
/// the name is drawn quieter than the number. The host, the rates and the
/// disks are all value.
pub fn split_label(kind: SegmentKind, text: &str) -> (Option<&str>, &str) {
    match kind {
        SegmentKind::Cpu | SegmentKind::Mem | SegmentKind::Uptime | SegmentKind::Users => {
            match text.split_once(' ') {
                Some((name, value)) if !value.is_empty() => (Some(name), value),
                _ => (None, text),
            }
        }
        SegmentKind::Host | SegmentKind::Net | SegmentKind::Disks => (None, text),
    }
}

/// The colour of the dot at the start of the bar: how the connection is.
pub fn state_dot(text: &BarText) -> u32 {
    match text {
        BarText::Values { stale_for: None } => palette::OK,
        BarText::Values { stale_for: Some(_) }
        | BarText::Connecting
        | BarText::Fetching
        | BarText::Unsupported => palette::WARN,
        BarText::ConnectFailed | BarText::Disconnected | BarText::Unavailable => palette::DANGER,
    }
}

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

        // (text, colour) runs, left to right.
        let muted = lin(palette::TEXT_MUTED);
        let faint = lin(palette::TEXT_FAINT);
        let danger = lin(palette::DANGER);
        let warn = lin(palette::WARN);
        let bg = lin(palette::BASE);
        let dot = format!("{} ", icon::DOT);

        let cell_width = self.render_metrics.cell_size.width as f32;
        let max_cells = ((bounds.width() / cell_width) as usize)
            .saturating_sub(2)
            .saturating_sub(unicode_column_width(&dot, None));

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

        let mut runs: Vec<(String, LinearRgba)> = vec![(dot, lin(state_dot(&text)))];
        let mut say = |text: &str, c: LinearRgba| runs.push((text.to_string(), c));
        match text {
            BarText::Connecting => say("正在连接…", muted),
            BarText::ConnectFailed => say("连接失败", danger),
            BarText::Disconnected => say("连接已断开", danger),
            BarText::Fetching => say("正在获取服务器状态…", muted),
            BarText::Unsupported => say("仅支持 Linux 服务器", muted),
            BarText::Unavailable => say("状态不可用", danger),
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
                        runs.push((SEPARATOR.to_string(), faint));
                    }
                    // Names quiet, numbers plain; only a level worth knowing
                    // about is coloured, and old numbers are greyed out,
                    // warnings and all.
                    let value_color = match (stale_for, seg.level) {
                        (Some(_), _) => faint,
                        (None, Level::Crit) => danger,
                        (None, Level::Warn) => warn,
                        (None, Level::Normal) => match seg.kind {
                            SegmentKind::Host => lin(palette::TEXT_STRONG),
                            _ => lin(palette::TEXT),
                        },
                    };
                    let name_color = if stale_for.is_some() { faint } else { muted };
                    match split_label(seg.kind, &seg.text) {
                        (Some(name), value) => {
                            runs.push((format!("{name} "), name_color));
                            runs.push((value.to_string(), value_color));
                        }
                        (None, value) => runs.push((value.to_string(), value_color)),
                    }
                }
                if let Some(note) = stale {
                    runs.push((SEPARATOR.to_string(), faint));
                    runs.push((note, warn));
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

        // A hairline along the top, inside the bar's height.
        let line = px(self.dimensions.dpi as f32, 1.).max(1.).floor();
        let bar = Element::new(&font, ElementContent::Children(children))
            .display(DisplayType::Block)
            .colors(ElementColors {
                border: BorderColor::new(lin(palette::DIVIDER)),
                bg: bg.into(),
                text: bg.into(),
            })
            .border(BoxDimension {
                left: Dimension::Pixels(0.),
                right: Dimension::Pixels(0.),
                top: Dimension::Pixels(line),
                bottom: Dimension::Pixels(0.),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(1.),
                right: Dimension::Cells(1.),
                top: Dimension::Cells(0.),
                bottom: Dimension::Cells(0.),
            })
            .min_width(Some(Dimension::Pixels(bounds.width())))
            .min_height(Some(Dimension::Pixels((bounds.height() - line).max(0.))));

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

#[cfg(test)]
mod test {
    use super::*;
    use crate::termwindow::theme::palette;

    #[test]
    fn a_segment_splits_into_its_name_and_its_value() {
        assert_eq!(split_label(SegmentKind::Cpu, "CPU 3%"), (Some("CPU"), "3%"));
        assert_eq!(
            split_label(SegmentKind::Mem, "内存 0.3/3.8G"),
            (Some("内存"), "0.3/3.8G")
        );
        assert_eq!(
            split_label(SegmentKind::Uptime, "运行 2分"),
            (Some("运行"), "2分")
        );
        assert_eq!(split_label(SegmentKind::Host, "Ty"), (None, "Ty"));
        assert_eq!(
            split_label(SegmentKind::Disks, "/mnt/c 86% +4"),
            (None, "/mnt/c 86% +4")
        );
        assert_eq!(split_label(SegmentKind::Cpu, "CPU"), (None, "CPU"));
    }

    #[test]
    fn the_dot_says_how_the_connection_is() {
        assert_eq!(state_dot(&BarText::Values { stale_for: None }), palette::OK);
        assert_eq!(
            state_dot(&BarText::Values { stale_for: Some(8) }),
            palette::WARN
        );
        assert_eq!(state_dot(&BarText::Connecting), palette::WARN);
        assert_eq!(state_dot(&BarText::Fetching), palette::WARN);
        assert_eq!(state_dot(&BarText::Unsupported), palette::WARN);
        assert_eq!(state_dot(&BarText::ConnectFailed), palette::DANGER);
        assert_eq!(state_dot(&BarText::Disconnected), palette::DANGER);
        assert_eq!(state_dot(&BarText::Unavailable), palette::DANGER);
    }
}
