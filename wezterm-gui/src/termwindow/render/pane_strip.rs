use crate::pane_strip::{layout_strip, StripHitKind, StripStyle, StripTab};
use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::RenderScreenLineParams;
use crate::termwindow::{UIItem, UIItemType};
use mux::pane::PaneId;
use mux::renderable::RenderableDimensions;
use mux::Mux;
use std::collections::HashSet;
use termwiz::cell::{CellAttributes, Intensity, Underline};
use termwiz::color::ColorSpec;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

/// Decides when output from a hidden stack member is worth a repaint.
///
/// A hidden member is not drawn, but its strip tab gains an unseen marker
/// the first time it produces output, so the window must repaint once for
/// that. Repainting on every chunk of output would let a chatty background
/// pane drive the frame rate, so each stretch of unseen output ("episode")
/// gets one request.
///
/// The set records "already requested for this episode" rather than "was
/// drawn last frame": while zoomed only the zoomed stack is drawn, and a
/// tab-level overlay hides every strip, so a drawn-based test would never
/// admit those panes and would repaint on every output instead.
#[derive(Default)]
pub(crate) struct HiddenOutputTracker {
    requested: HashSet<PaneId>,
}

impl HiddenOutputTracker {
    /// Returns true if the window should be invalidated for this output.
    pub fn on_output(
        &mut self,
        pane_id: PaneId,
        in_active_tab: bool,
        visible: bool,
        unseen: bool,
    ) -> bool {
        if in_active_tab && !visible && unseen && !self.requested.contains(&pane_id) {
            self.requested.insert(pane_id);
            true
        } else {
            false
        }
    }

    /// Called after every frame. Dropping ids that became visible or whose
    /// unseen output was cleared ends their episode, so the next unseen
    /// output asks for a repaint again.
    pub fn prune(&mut self, still_hidden_and_unseen: impl Fn(PaneId) -> bool) {
        self.requested
            .retain(|&pane_id| still_hidden_and_unseen(pane_id));
    }
}

impl crate::TermWindow {
    /// Paint the one-row tab strip at the top of every leaf of the active
    /// tab and register its hit regions. Tabs without header rows have no
    /// room reserved for a strip and are left exactly as before.
    pub fn paint_pane_strips(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return Ok(()),
        };
        if tab.header_rows() == 0 {
            return Ok(());
        }
        let tab_id = tab.tab_id();
        // A tab-level overlay (launcher, tab navigator, ...) is drawn over
        // the whole tab, header rows included; see get_pos_panes_for_tab.
        if self.tab_state(tab_id).overlay.is_some() {
            return Ok(());
        }

        let style = self.pane_strip_style();
        let (origin_x, origin_y) = self.content_origin_pixels()?;
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;

        // Everything below mirrors the retro tab bar in tab_bar.rs, which is
        // the other caller that renders a synthetic line with no pane.
        let palette = self.palette().clone();
        let window_is_transparent =
            !self.window_background.is_empty() || self.config.window_background_opacity != 1.0;
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                self.config.text_background_opacity
            });

        for stack in tab.iter_stacks() {
            let tabs: Vec<StripTab> = stack
                .panes
                .iter()
                .enumerate()
                .map(|(i, pane)| StripTab {
                    pane_id: pane.pane_id(),
                    title: pane.get_title(),
                    unseen: pane.has_unseen_output(),
                    is_visible: i == stack.visible,
                })
                .collect();
            let layout = layout_strip(&tabs, stack.is_active, stack.width, &style);

            let left = origin_x + stack.left as f32 * cell_width;
            let top = origin_y + stack.top as f32 * cell_height;
            let pixel_width = stack.width as f32 * cell_width;

            self.render_screen_line(
                RenderScreenLineParams {
                    top_pixel_y: top,
                    left_pixel_x: left,
                    pixel_width,
                    stable_line_idx: None,
                    line: &layout.line,
                    selection: 0..0,
                    cursor: &Default::default(),
                    palette: &palette,
                    dims: &RenderableDimensions {
                        cols: stack.width,
                        physical_top: 0,
                        scrollback_rows: 0,
                        scrollback_top: 0,
                        viewport_rows: 1,
                        dpi: self.terminal_size.dpi,
                        pixel_height: self.render_metrics.cell_size.height as usize,
                        pixel_width: self.terminal_size.pixel_width,
                        reverse_video: false,
                    },
                    config: &self.config,
                    cursor_border_color: LinearRgba::default(),
                    foreground: palette.foreground.to_linear(),
                    pane: None,
                    is_active: true,
                    selection_fg: LinearRgba::default(),
                    selection_bg: LinearRgba::default(),
                    cursor_fg: LinearRgba::default(),
                    cursor_bg: LinearRgba::default(),
                    cursor_is_default_color: true,
                    white_space,
                    filled_box,
                    window_is_transparent,
                    default_bg,
                    style: None,
                    font: None,
                    use_pixel_positioning: self.config.experimental_pixel_positioning,
                    render_metrics: self.render_metrics,
                    shape_key: None,
                    password_input: false,
                },
                layers,
            )?;

            // resolve_ui_item searches from the back, so the whole-strip item
            // goes first and the tabs and close buttons on top of it win.
            self.ui_items.push(UIItem {
                x: left as usize,
                y: top as usize,
                width: pixel_width as usize,
                height: cell_height as usize,
                item_type: UIItemType::PaneTabBar {
                    tab_id,
                    leaf: stack.index,
                },
            });
            for hit in &layout.hits {
                let item_type = match hit.kind {
                    StripHitKind::Tab(pane_id) => UIItemType::PaneTab { tab_id, pane_id },
                    StripHitKind::Close(pane_id) => UIItemType::PaneTabClose { tab_id, pane_id },
                };
                self.ui_items.push(UIItem {
                    x: (left + hit.x as f32 * cell_width) as usize,
                    y: top as usize,
                    width: (hit.width as f32 * cell_width) as usize,
                    height: cell_height as usize,
                    item_type,
                });
            }
        }

        Ok(())
    }

    fn pane_strip_style(&self) -> StripStyle {
        let colors = self
            .config
            .resolved_palette
            .tab_bar
            .clone()
            .unwrap_or_default();
        let mut background = CellAttributes::default();
        background.set_background(ColorSpec::TrueColor(*colors.background()));
        let hidden = colors.inactive_tab().as_cell_attributes();
        // The visible tab of an unfocused leaf must still stand out from the
        // hidden ones, which share its colours.
        let mut visible = hidden.clone();
        visible
            .set_intensity(Intensity::Bold)
            .set_underline(Underline::Single);
        StripStyle {
            background,
            active: colors.active_tab().as_cell_attributes(),
            visible,
            hidden,
        }
    }

    /// Ends the unseen-output episode of panes that are no longer hidden
    /// members of the active tab, or have nothing unseen any more. Runs
    /// after every frame, including frames that drew no strips (zoom, a tab
    /// overlay, header_rows == 0), so that ids do not linger.
    pub fn prune_hidden_output(&mut self) {
        let mux = Mux::get();
        let tab = mux.get_active_tab_for_window(self.mux_window_id);
        self.hidden_output.prune(|pane_id| {
            let hidden = tab.as_ref().map_or(false, |tab| {
                tab.contains_pane(pane_id) && !tab.contains_visible_pane(pane_id)
            });
            hidden
                && mux
                    .get_pane(pane_id)
                    .map_or(false, |pane| pane.has_unseen_output())
        });
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn hidden_output_repaints_once_per_unseen_episode() {
        let mut t = HiddenOutputTracker::default();
        // A chatty hidden pane asks for one repaint, not one per output.
        assert_eq!(
            [
                t.on_output(5, true, false, true),
                t.on_output(5, true, false, true)
            ],
            [true, false]
        );
        // Still hidden and unseen after a frame: the episode continues.
        t.prune(|_| true);
        assert!(!t.on_output(5, true, false, true));
        // Shown in the meantime: the episode is over, and once it is hidden
        // again with fresh output a new one starts.
        t.prune(|_| false);
        assert!(t.on_output(5, true, false, true));
        // Visible panes repaint through is_pane_visible, not here.
        assert!(!t.on_output(6, true, true, true));
        // Panes outside the active tab are not on screen at all.
        assert!(!t.on_output(7, false, false, true));
        // Nothing unseen means no marker to show.
        assert!(!t.on_output(8, true, false, false));
    }
}
