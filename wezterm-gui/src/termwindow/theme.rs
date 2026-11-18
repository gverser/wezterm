//! The look of the UI chrome this fork draws: the sidebar, its menus, the
//! forms, the status bar and the Feishu panel.
//!
//! One palette and one set of icons, named here and nowhere else, so that the
//! layers stay in step: base under panel under raised, one accent.
//! See docs/superpowers/specs/2026-11-11-ui-refresh-design.md.

use crate::termwindow::box_model::*;
use crate::termwindow::render::corners::{
    BOTTOM_LEFT_ROUNDED_CORNER, BOTTOM_RIGHT_ROUNDED_CORNER, TOP_LEFT_ROUNDED_CORNER,
    TOP_RIGHT_ROUNDED_CORNER,
};
use crate::utilsprites::RenderMetrics;
use ::window::RectF;
use config::{Dimension, DimensionContext};
use window::color::LinearRgba;

/// Colours as 0xRRGGBB, in sRGB; `lin` turns one into what the renderer wants.
pub mod palette {
    /// The bottom layer: the page rail, the tab bar, the status bar.
    pub const BASE: u32 = 0x15171c;
    /// The panel the pages are drawn on, and the current tab.
    pub const PANEL: u32 = 0x1c1f25;
    /// What floats above the panel: menus, fields, form cards.
    pub const RAISED: u32 = 0x262a32;
    pub const DIVIDER: u32 = 0x2a2f38;
    pub const INPUT_EDGE: u32 = 0x30363f;
    pub const CARD_EDGE: u32 = 0x3a404b;
    pub const MENU_SEPARATOR: u32 = 0x353b45;
    pub const ROW_HOVER: u32 = 0x252a32;
    pub const ROW_SELECTED: u32 = 0x1f3557;
    pub const ACCENT: u32 = 0x4c8dff;
    /// A menu item under the pointer, and the primary button.
    pub const MENU_HOVER: u32 = 0x2f6feb;
    pub const TEXT: u32 = 0xc9d1d9;
    pub const TEXT_STRONG: u32 = 0xe6edf3;
    pub const TEXT_MUTED: u32 = 0x8b949e;
    pub const TEXT_FAINT: u32 = 0x6e7681;
    pub const MENU_TEXT: u32 = 0xd6dde5;
    pub const DANGER: u32 = 0xff7b72;
    pub const OK: u32 = 0x3fb950;
    pub const WARN: u32 = 0xe3b341;
    pub const PILL: u32 = 0x2d333d;
    pub const DANGER_BUTTON: u32 = 0xda3633;
    /// Buttons under the pointer go a shade darker: white text keeps its
    /// contrast that way, where lightening them washed it out.
    pub const ACCENT_HOVER: u32 = 0x1f58c7;
    pub const DANGER_HOVER: u32 = 0xb62324;
    pub const RAIL_ACTIVE: u32 = 0x22262e;
    pub const RAIL_TEXT: u32 = 0x7d8590;
    pub const ICON_FOLDER: u32 = 0xe3b341;
    pub const ICON_SSH: u32 = 0x56d4dd;
    pub const ICON_LOCAL: u32 = 0x7ee787;
    pub const ICON_WSL: u32 = 0xffa657;
    pub const ICON_FILE: u32 = 0x8b949e;
    pub const ICON_LINK: u32 = 0x56d4dd;
    pub const WHITE: u32 = 0xffffff;
}

/// Codicons from the bundled Symbols Nerd Font Mono, which every UI font
/// falls back to. Code points checked against the font's glyph names.
pub mod icon {
    pub const TERMINAL: &str = "\u{ea85}";
    pub const SERVER: &str = "\u{eb50}";
    pub const LINUX: &str = "\u{ebc6}";
    pub const FOLDER: &str = "\u{ea83}";
    pub const FOLDER_OPEN: &str = "\u{eaf7}";
    pub const NEW_FOLDER: &str = "\u{ea80}";
    pub const FILE: &str = "\u{ea7b}";
    pub const LINK: &str = "\u{eb15}";
    pub const CHEVRON_RIGHT: &str = "\u{eab6}";
    pub const CHEVRON_DOWN: &str = "\u{eab4}";
    pub const CHEVRON_LEFT: &str = "\u{eab5}";
    pub const SEARCH: &str = "\u{ea6d}";
    pub const ADD: &str = "\u{ea60}";
    pub const EDIT: &str = "\u{ea73}";
    pub const SETTINGS: &str = "\u{eb51}";
    pub const TRASH: &str = "\u{ea81}";
    pub const DOWNLOAD: &str = "\u{eac2}";
    pub const UPLOAD: &str = "\u{eac3}";
    pub const UP: &str = "\u{eaa1}";
    pub const REFRESH: &str = "\u{eb37}";
    pub const HOME: &str = "\u{eb06}";
    pub const COPY: &str = "\u{ebcc}";
    pub const CLOSE: &str = "\u{ea76}";
    pub const DOT: &str = "\u{ea71}";
    /// Material Design checkboxes: codicons have none.
    pub const CHECKBOX_ON: &str = "\u{f0132}";
    pub const CHECKBOX_OFF: &str = "\u{f0131}";

    /// Every icon, for the test that they are all in the font.
    #[cfg(test)]
    pub const ALL: &[&str] = &[
        TERMINAL,
        SERVER,
        LINUX,
        FOLDER,
        FOLDER_OPEN,
        NEW_FOLDER,
        FILE,
        LINK,
        CHEVRON_RIGHT,
        CHEVRON_DOWN,
        CHEVRON_LEFT,
        SEARCH,
        ADD,
        EDIT,
        SETTINGS,
        TRASH,
        DOWNLOAD,
        UPLOAD,
        UP,
        REFRESH,
        HOME,
        COPY,
        CLOSE,
        DOT,
        CHECKBOX_ON,
        CHECKBOX_OFF,
    ];
}

/// The fill a menu entry lights up with under the pointer. One that destroys
/// something goes red, like the button that does the same in a confirmation.
pub fn menu_item_hover(danger: bool) -> u32 {
    if danger {
        palette::DANGER_BUTTON
    } else {
        palette::MENU_HOVER
    }
}

/// The colours of the secondary bits of a sidebar row, its size and its
/// folder arrow. On the selected row's blue they step up a shade, or they'd
/// sink into it.
pub fn row_side_colors(selected: bool) -> (u32, u32) {
    if selected {
        (palette::TEXT, palette::TEXT_MUTED)
    } else {
        (palette::TEXT_MUTED, palette::TEXT_FAINT)
    }
}

/// WCAG contrast ratio between two opaque colours, 1 to 21.
#[cfg(test)]
pub fn contrast(a: u32, b: u32) -> f32 {
    fn luminance(hex: u32) -> f32 {
        let channel = |shift: u32| {
            let c = ((hex >> shift) & 0xff) as f32 / 255.;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// A palette colour, opaque, as the renderer takes it.
pub fn lin(hex: u32) -> LinearRgba {
    LinearRgba::with_srgba((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 0xff)
}

/// Black at `alpha`, for shadows.
pub fn shade(alpha: f32) -> LinearRgba {
    LinearRgba(0., 0., 0., alpha)
}

/// A length given at 96 dpi, at `dpi`.
pub fn px(dpi: f32, at_96: f32) -> f32 {
    at_96 * dpi / 96.
}

fn corner(radius: f32, poly: &'static [crate::customglyph::Poly]) -> SizedPoly {
    SizedPoly {
        width: Dimension::Pixels(radius),
        height: Dimension::Pixels(radius),
        poly,
    }
}

/// All four corners rounded, `radius` in pixels.
///
/// box_model fills a corner with the element's top or bottom border colour,
/// so the element's border colour must match its background (`fill`), or
/// the corners come out in the border colour.
pub fn rounded(radius: f32) -> Corners {
    Corners {
        top_left: corner(radius, TOP_LEFT_ROUNDED_CORNER),
        top_right: corner(radius, TOP_RIGHT_ROUNDED_CORNER),
        bottom_left: corner(radius, BOTTOM_LEFT_ROUNDED_CORNER),
        bottom_right: corner(radius, BOTTOM_RIGHT_ROUNDED_CORNER),
    }
}

/// Colours for a solid shape: the border takes the fill, so that rounded
/// corners do too.
pub fn fill(bg: LinearRgba, text: LinearRgba) -> ElementColors {
    ElementColors {
        border: BorderColor::new(bg),
        bg: bg.into(),
        text: text.into(),
    }
}

/// One entry of a pop-up menu.
#[derive(Debug, Clone)]
pub struct MenuEntry<'a> {
    pub icon: &'static str,
    pub label: &'a str,
    /// Entries of one group sit together; a line separates groups.
    pub group: u8,
    /// Drawn in the danger colour: it deletes something.
    pub danger: bool,
    pub item: crate::termwindow::UIItemType,
}

#[derive(Debug, Clone)]
pub enum MenuRow<'a> {
    Item(MenuEntry<'a>),
    Separator,
}

/// `entries` in order, with a separator wherever the group changes.
pub fn menu_rows(entries: Vec<MenuEntry>) -> Vec<MenuRow> {
    let mut rows = vec![];
    let mut last_group = None;
    for entry in entries {
        if last_group.map_or(false, |group| group != entry.group) {
            rows.push(MenuRow::Separator);
        }
        last_group = Some(entry.group);
        rows.push(MenuRow::Item(entry));
    }
    rows
}

/// The height of a menu of `rows`: entries `item_h` tall, separators
/// `sep_h`, inside `pad` above and below.
pub fn menu_height(rows: &[MenuRow], item_h: f32, sep_h: f32, pad: f32) -> f32 {
    let height: f32 = rows
        .iter()
        .map(|row| match row {
            MenuRow::Item(_) => item_h,
            MenuRow::Separator => sep_h,
        })
        .sum();
    height + pad * 2.
}

impl crate::TermWindow {
    /// A soft shadow for something floating over `rect`: two translucent
    /// rounded blocks a little larger than it and dropped a few pixels.
    /// box_model has no blur, so this is the nearest it gets. Draw these
    /// before the thing itself.
    pub(crate) fn theme_shadow(
        &self,
        rect: RectF,
        radius: f32,
        zindex: i8,
    ) -> anyhow::Result<Vec<ComputedElement>> {
        let font = self.fonts.title_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let dpi = self.dimensions.dpi as f32;
        let mut shadows = vec![];
        for (grow, drop, alpha) in [(3., 4., 0.14), (1., 2., 0.2)] {
            let grow = px(dpi, grow);
            let drop = px(dpi, drop);
            let bounds = euclid::rect(
                rect.min_x() - grow,
                rect.min_y() - grow + drop,
                rect.width() + grow * 2.,
                rect.height() + grow * 2.,
            );
            let element = Element::new(&font, ElementContent::Text(String::new()))
                .display(DisplayType::Block)
                .min_width(Some(Dimension::Pixels(bounds.width())))
                .min_height(Some(Dimension::Pixels(bounds.height())))
                .border_corners(Some(rounded(radius + grow)))
                .colors(fill(shade(alpha), shade(0.)));
            shadows.push(self.compute_element(
                &LayoutContext {
                    height: DimensionContext {
                        dpi,
                        pixel_max: bounds.max_y(),
                        pixel_cell: metrics.cell_size.height as f32,
                    },
                    width: DimensionContext {
                        dpi,
                        pixel_max: bounds.max_x(),
                        pixel_cell: metrics.cell_size.width as f32,
                    },
                    bounds,
                    metrics: &metrics,
                    gl_state: self.render_state.as_ref().unwrap(),
                    zindex,
                },
                &element,
            )?);
        }
        Ok(shadows)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn every_icon_is_in_the_bundled_symbols_font() {
        let data: &'static [u8] =
            include_bytes!("../../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");
        let handle = wezterm_font::locator::FontDataHandle {
            source: wezterm_font::locator::FontDataSource::BuiltIn {
                name: "symbols",
                data,
            },
            index: 0,
            variation: 0,
            origin: wezterm_font::locator::FontOrigin::BuiltIn,
            coverage: None,
        };
        let parsed = wezterm_font::parser::ParsedFont::from_locator(&handle).unwrap();
        // One at a time: a RangeSet built by `add` keeps adjacent points as
        // separate ranges, so comparing whole sets fails on equal contents.
        for icon in icon::ALL {
            let mut chars = icon.chars();
            let c = chars.next().expect("an icon is one character") as u32;
            assert!(chars.next().is_none(), "{:?} is one character", icon);
            let mut wanted = rangeset::RangeSet::new();
            wanted.add(c);
            let found = parsed.coverage_intersection(&wanted).unwrap();
            assert!(found.contains(c), "U+{:04X} is not in the symbols font", c);
        }
    }

    #[test]
    fn palette_values_are_the_specs() {
        assert_eq!(palette::PANEL, 0x1c1f25);
        assert_eq!(palette::ACCENT, 0x4c8dff);
        let white = lin(0xffffff);
        assert!((white.0 - 1.).abs() < 1e-6 && (white.3 - 1.).abs() < 1e-6);
        let black = lin(0x000000);
        assert!(black.0.abs() < 1e-6 && (black.3 - 1.).abs() < 1e-6);
    }

    #[test]
    fn separators_go_between_groups_only() {
        let e = |group| MenuEntry {
            icon: icon::ADD,
            label: "x",
            group,
            danger: false,
            item: crate::termwindow::UIItemType::SidebarBackground,
        };
        let rows = menu_rows(vec![e(0), e(0), e(1), e(2)]);
        let kinds: Vec<bool> = rows
            .iter()
            .map(|r| matches!(r, MenuRow::Separator))
            .collect();
        assert_eq!(kinds, vec![false, false, true, false, true, false]);
        assert_eq!(menu_height(&rows, 28., 9., 4.), 4. * 28. + 2. * 9. + 8.);
        assert!(menu_rows(vec![]).is_empty());
    }

    #[test]
    fn contrast_follows_wcag() {
        assert!((contrast(palette::WHITE, 0x000000) - 21.).abs() < 0.01);
        assert!((contrast(0x777777, 0x777777) - 1.).abs() < 1e-6);
        // Either way round.
        assert_eq!(
            contrast(palette::TEXT, palette::PANEL),
            contrast(palette::PANEL, palette::TEXT)
        );
    }

    #[test]
    fn text_on_its_background_is_readable() {
        let (size, arrow) = row_side_colors(true);
        let (idle_size, idle_arrow) = row_side_colors(false);
        // Text: WCAG AA for body text.
        for (what, fg, bg) in [
            (
                "primary button hover",
                palette::WHITE,
                palette::ACCENT_HOVER,
            ),
            ("danger button hover", palette::WHITE, palette::DANGER_HOVER),
            ("menu entry hover", palette::WHITE, menu_item_hover(false)),
            (
                "dangerous menu entry hover",
                palette::WHITE,
                menu_item_hover(true),
            ),
            ("size on a selected row", size, palette::ROW_SELECTED),
            ("size on a row", idle_size, palette::PANEL),
        ] {
            let ratio = contrast(fg, bg);
            assert!(ratio >= 4.5, "{}: {:.2}", what, ratio);
        }
        // Glyphs: 3:1 for graphical objects.
        for (what, fg, bg) in [
            ("arrow on a selected row", arrow, palette::ROW_SELECTED),
            ("arrow on a row", idle_arrow, palette::PANEL),
        ] {
            let ratio = contrast(fg, bg);
            assert!(ratio >= 3., "{}: {:.2}", what, ratio);
        }
    }

    #[test]
    fn a_dangerous_entry_lights_up_like_the_danger_button() {
        assert_eq!(menu_item_hover(true), palette::DANGER_BUTTON);
        assert_eq!(menu_item_hover(false), palette::MENU_HOVER);
    }

    #[test]
    fn sizes_are_given_at_96_dpi() {
        assert_eq!(px(96., 46.), 46.);
        assert_eq!(px(120., 8.), 10.);
    }
}
