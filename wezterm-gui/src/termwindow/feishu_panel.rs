//! The Feishu panel: what is running, what this pane is doing, and the way out.
//!
//! The tab bar icon has to say two different things at once -- whether the
//! service is up, and whether *this* pane is being mirrored -- which is more
//! than a small glyph can carry. This is where those two are spelled out, along
//! with the panes the user turned on earlier and may have forgotten.

use crate::sessions::store::with_store;
use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::termwindow::DimensionContext;
use crate::utilsprites::RenderMetrics;
use crate::TermWindow;
use config::keyassignment::KeyAssignment;
use config::Dimension;
use mux::pane::PaneId;
use mux::{FeishuState, Mux};
use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};

/// The things this panel can do, named so the key handler and the row that
/// advertises the key cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    ToggleCurrentPane,
    StopService,
}

pub struct FeishuPanel {
    element: RefCell<Option<Vec<ComputedElement>>>,
}

impl FeishuPanel {
    pub fn new() -> Self {
        Self {
            element: RefCell::new(None),
        }
    }

    fn describe_state(state: FeishuState, detail: &Option<String>) -> String {
        match state {
            FeishuState::Stopped => "未启动".to_string(),
            FeishuState::Starting => "正在启动…".to_string(),
            FeishuState::Connected => "已连接".to_string(),
            FeishuState::Failed => match detail {
                Some(why) => format!("已停止 — {why}"),
                None => "已停止".to_string(),
            },
        }
    }

    /// Workspace name -> the sidebar entry it belongs to, so a row can say
    /// "casb" rather than "session:2".
    fn session_labels() -> HashMap<String, String> {
        with_store(|store| {
            store
                .tree()
                .sessions_with_names()
                .into_iter()
                .map(|(id, name)| (id.workspace_name(), name))
                .collect()
        })
    }

    /// Which session a pane belongs to, if it belongs to one the sidebar knows.
    fn session_of(mux: &Mux, labels: &HashMap<String, String>, pane_id: PaneId) -> Option<String> {
        let (_domain, window_id, _tab) = mux.resolve_pane_id(pane_id)?;
        let workspace = mux.get_window(window_id)?.get_workspace().to_string();
        labels.get(&workspace).cloned()
    }

    /// A pane, named the way the user thinks of it.
    ///
    /// The title alone is useless here: half a dozen mirrored panes are all
    /// called cmd.exe, which is exactly the situation this panel exists to
    /// untangle. The session name is what tells them apart.
    ///
    /// The number is `feishu_pane_id`, not `pane_id`. This panel sits next to a
    /// phone showing the same sessions, and the number on the phone comes from
    /// the mux server; printing the client's own id here put two different
    /// numbers on one window.
    fn pane_caption(mux: &Mux, labels: &HashMap<String, String>, pane_id: PaneId) -> String {
        let pane = mux.get_pane(pane_id);
        let title = pane.as_ref().map(|p| p.get_title()).unwrap_or_default();
        let shown = pane.map_or(pane_id, |p| p.feishu_pane_id());
        match Self::session_of(mux, labels, pane_id) {
            Some(session) => format!("{session}  ·  #{shown}  {title}"),
            None => format!("#{shown}  {title}"),
        }
    }

    fn compute(term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window.fonts.title_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());

        let mux = Mux::get();
        let (state, detail) = mux.feishu_service_state();
        let current = term_window
            .get_active_pane_or_overlay()
            .map(|p| p.pane_id());
        let subscribed = mux.feishu_panes();
        let labels = Self::session_labels();

        // Borrowed from the command palette rather than given their own config
        // keys: it is the same kind of surface, and one setting that moves both
        // beats two that can disagree.
        let panel_bg_linear = term_window.config.command_palette_bg_color.to_linear();
        let text_color: InheritableColor = term_window
            .config
            .command_palette_fg_color
            .to_linear()
            .into();
        let panel_bg: InheritableColor = panel_bg_linear.into();

        let mut elements = vec![];

        let row = |text: String| {
            Element::new(&font, ElementContent::Text(text))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: text_color.clone(),
                })
                .padding(BoxDimension {
                    left: Dimension::Cells(0.5),
                    right: Dimension::Cells(0.5),
                    top: Dimension::Cells(0.),
                    bottom: Dimension::Cells(0.),
                })
                .min_width(Some(Dimension::Percent(1.)))
                .display(DisplayType::Block)
        };

        elements.push(row("飞书".to_string()));
        elements.push(row(format!(
            "  状态    {}",
            Self::describe_state(state, &detail)
        )));

        // Only offer to stop something that is actually running.
        if !matches!(state, FeishuState::Stopped) {
            elements.push(row("  [s] 关闭服务（同时取消所有订阅）".to_string()));
        }

        elements.push(row(String::new()));

        match current {
            Some(pane_id) => {
                let on = mux.pane_has_feishu(pane_id);
                elements.push(row(format!(
                    "  当前  {}",
                    Self::pane_caption(&mux, &labels, pane_id)
                )));
                elements.push(row(format!(
                    "  [空格] {}",
                    if on {
                        "停止镜像这个 pane"
                    } else {
                        "开始镜像这个 pane"
                    }
                )));
            }
            None => {
                // An empty window has no pane to mirror. Saying so beats a
                // toggle that silently does nothing.
                elements.push(row("  这个窗口还没有 session，先开一个".to_string()));
            }
        }

        let others: Vec<PaneId> = subscribed
            .iter()
            .copied()
            .filter(|id| Some(*id) != current)
            .collect();
        if !others.is_empty() {
            elements.push(row(String::new()));
            elements.push(row("  其他镜像中的 pane".to_string()));
            for id in others {
                elements.push(row(format!(
                    "    {}",
                    Self::pane_caption(&mux, &labels, id)
                )));
            }
        }

        elements.push(row(String::new()));
        elements.push(row("  [Esc] 关闭这个面板".to_string()));

        let dimensions = term_window.dimensions;
        let size = term_window.terminal_size;
        let desired_width = (size.cols / 2).max(46).min(size.cols);
        let avail_pixel_width =
            size.cols as f32 * term_window.render_metrics.cell_size.width as f32;
        let desired_pixel_width =
            desired_width as f32 * term_window.render_metrics.cell_size.width as f32;

        let element = Element::new(&font, ElementContent::Children(elements))
            .colors(ElementColors {
                border: BorderColor::new(panel_bg_linear),
                bg: panel_bg,
                text: text_color,
            })
            .margin(BoxDimension {
                left: Dimension::Cells(0.25),
                right: Dimension::Cells(0.25),
                top: Dimension::Cells(0.25),
                bottom: Dimension::Cells(0.25),
            })
            .padding(BoxDimension {
                left: Dimension::Cells(0.5),
                right: Dimension::Cells(0.5),
                top: Dimension::Cells(0.5),
                bottom: Dimension::Cells(0.5),
            })
            .border(BoxDimension::new(Dimension::Pixels(1.)))
            .border_corners(Some(Corners {
                top_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_LEFT_ROUNDED_CORNER,
                },
                top_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: TOP_RIGHT_ROUNDED_CORNER,
                },
                bottom_left: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_LEFT_ROUNDED_CORNER,
                },
                bottom_right: SizedPoly {
                    width: Dimension::Cells(0.25),
                    height: Dimension::Cells(0.25),
                    poly: BOTTOM_RIGHT_ROUNDED_CORNER,
                },
            }))
            .min_width(Some(Dimension::Pixels(desired_pixel_width)));

        let (padding_left, padding_top) = term_window.padding_left_top();
        let border = term_window.get_os_border();
        let top_bar_height = if term_window.show_tab_bar && !term_window.config.tab_bar_at_bottom {
            term_window.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;
        let x_adjust = ((avail_pixel_width - padding_left) - desired_pixel_width) / 2.;

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dimensions.dpi as f32,
                    pixel_max: dimensions.pixel_width as f32,
                    pixel_cell: metrics.cell_size.width as f32,
                },
                bounds: euclid::rect(
                    x_adjust,
                    top_pixel_y,
                    desired_pixel_width,
                    dimensions.pixel_height as f32 - top_pixel_y,
                ),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        Ok(vec![computed])
    }

    /// Carry out whatever the given row means.
    fn activate(&self, action: Action, term_window: &mut TermWindow) {
        match action {
            Action::ToggleCurrentPane => {
                term_window.toggle_feishu_for_active_pane();
                // The state arrives back as a notification; recompute so the
                // panel is not showing the previous answer.
                self.element.borrow_mut().take();
            }
            Action::StopService => {
                term_window.stop_feishu_service();
                self.element.borrow_mut().take();
            }
        }
    }
}

impl Modal for FeishuPanel {
    fn perform_assignment(
        &self,
        _assignment: &KeyAssignment,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }

    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        // Deliberately keyboard-only, like every other modal here. The event
        // that reaches a modal carries terminal cell coordinates, while this
        // panel is positioned in pixels; mapping between them precisely enough
        // to hit the right row is more machinery than the panel is worth, and
        // the shortcuts are written on it.
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        match (key, mods) {
            (KeyCode::Escape, KeyModifiers::NONE) => {
                term_window.cancel_modal();
            }
            (KeyCode::Char(' '), KeyModifiers::NONE) | (KeyCode::Enter, KeyModifiers::NONE) => {
                self.activate(Action::ToggleCurrentPane, term_window);
            }
            (KeyCode::Char('s'), KeyModifiers::NONE) => {
                self.activate(Action::StopService, term_window);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            self.element
                .borrow_mut()
                .replace(Self::compute(term_window)?);
        }
        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }
}
