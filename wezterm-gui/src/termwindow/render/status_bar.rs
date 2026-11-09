//! The ssh status bar: one row under the terminal area showing the server
//! the active pane is connected to.

use crate::sessions::monitor::{ensure_monitor, monitor_view, MonitorState};
use crate::sessions::monitor_proto::{status_segments, Level, SegmentKind};
use crate::termwindow::box_model::*;
use crate::termwindow::DimensionContext;
use crate::utilsprites::RenderMetrics;
use config::Dimension;
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

        let mut runs: Vec<(String, LinearRgba)> = vec![];
        match monitor_view(domain_id) {
            Some(view) if view.state == MonitorState::Unsupported => {
                runs.push(("仅支持 Linux 服务器".to_string(), color(11)));
            }
            Some(view) if view.state == MonitorState::Failed && view.status.is_none() => {
                runs.push(("状态不可用".to_string(), color(9)));
            }
            Some(view) => match view.status {
                Some(status) => {
                    for (i, seg) in status_segments(&status, max_cells).into_iter().enumerate() {
                        if i > 0 {
                            runs.push((SEPARATOR.to_string(), dim));
                        }
                        let c = match seg.level {
                            Level::Crit => color(9),
                            Level::Warn => color(11),
                            Level::Normal => match seg.kind {
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
                    if view.state == MonitorState::Failed {
                        runs.push((SEPARATOR.to_string(), dim));
                        runs.push(("状态不可用".to_string(), color(9)));
                    }
                }
                None => runs.push(("正在获取服务器状态…".to_string(), dim)),
            },
            None => runs.push(("正在获取服务器状态…".to_string(), dim)),
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
