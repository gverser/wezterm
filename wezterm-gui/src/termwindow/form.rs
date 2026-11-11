//! A small form dialog: labelled fields the user can fill in any order.
//!
//! `FormState` is the editing model and has no rendering in it, so that it can
//! be tested; `FormModal` draws it with the box model and wires it to the
//! keyboard, the IME, the clipboard and the mouse.

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text,
    /// Shown as dots unless `FormState::reveal_password` is set.
    Password,
    /// One of a fixed set of options; `Field::value` holds the chosen one.
    Choice(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub label: String,
    pub kind: FieldKind,
    pub value: String,
    pub enabled: bool,
    /// Shown dimmed while the field is empty, saying what empty means.
    pub placeholder: String,
}

impl Field {
    pub fn text(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            kind: FieldKind::Text,
            value: value.to_string(),
            enabled: true,
            placeholder: String::new(),
        }
    }

    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self
    }

    pub fn password(label: &str, value: &str) -> Self {
        Self {
            kind: FieldKind::Password,
            ..Self::text(label, value)
        }
    }

    pub fn choice(label: &str, options: &[&str], value: &str) -> Self {
        Self {
            kind: FieldKind::Choice(options.iter().map(|s| s.to_string()).collect()),
            ..Self::text(label, value)
        }
    }

    fn is_editable_text(&self) -> bool {
        self.enabled && !matches!(self.kind, FieldKind::Choice(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum FormButton {
    /// Validates, and submits if there is nothing to complain about.
    Primary(String),
    /// Closes the form without doing anything.
    Cancel(String),
    /// An extra action tied to a field, such as "浏览…" next to a path.
    Extra(String, usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormState {
    pub title: String,
    /// Lines of text shown under the title, e.g. the question a confirmation asks.
    pub message: Vec<String>,
    pub fields: Vec<Field>,
    pub buttons: Vec<FormButton>,
    pub focus: usize,
    /// Position of the cursor in the focused field, counted in chars.
    pub cursor: usize,
    /// Uncommitted IME text, drawn at the cursor.
    pub composing: Option<String>,
    pub reveal_password: bool,
    /// Validation messages, by field index.
    pub errors: Vec<(usize, String)>,
    /// The primary button deletes something: drawn in the danger colour.
    pub danger: bool,
}

impl FormState {
    pub fn new(title: &str, fields: Vec<Field>, buttons: Vec<FormButton>) -> Self {
        let mut state = Self {
            title: title.to_string(),
            message: vec![],
            fields,
            buttons,
            focus: 0,
            cursor: 0,
            composing: None,
            reveal_password: false,
            errors: vec![],
            danger: false,
        };
        if let Some(first) = state.fields.iter().position(|f| f.enabled) {
            state.focus_field(first);
        }
        state
    }

    /// What `c` does to a confirmation -- a form with nothing to type in:
    /// y answers yes (Some(true)), n answers no (Some(false)). A form with
    /// fields takes every letter as text.
    pub fn shortcut(&self, c: char) -> Option<bool> {
        if !self.fields.is_empty() {
            return None;
        }
        match c {
            'y' | 'Y' => Some(true),
            'n' | 'N' => Some(false),
            _ => None,
        }
    }

    /// The line of keys under the buttons.
    pub fn key_hint(&self) -> &'static str {
        if self.fields.is_empty() {
            "Y 确定　N 取消　Enter 确定　Esc 取消"
        } else {
            "Tab 切换　Enter 确定　Esc 取消"
        }
    }

    /// The value of the field with this label, or "" if there is none.
    pub fn value(&self, label: &str) -> &str {
        self.fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }

    pub fn index_of(&self, label: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.label == label)
    }

    fn focused_text_field(&mut self) -> Option<&mut Field> {
        self.fields
            .get_mut(self.focus)
            .filter(|f| f.is_editable_text())
    }

    fn byte_offset(value: &str, chars: usize) -> usize {
        value
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(value.len())
    }

    fn edited(&mut self) {
        let focus = self.focus;
        self.errors.retain(|(i, _)| *i != focus);
    }

    pub fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| *c != '\r' && *c != '\n').collect();
        if text.is_empty() {
            return;
        }
        let cursor = self.cursor;
        let Some(field) = self.focused_text_field() else {
            return;
        };
        let at = Self::byte_offset(&field.value, cursor);
        field.value.insert_str(at, &text);
        self.cursor += text.chars().count();
        self.edited();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let cursor = self.cursor;
        let Some(field) = self.focused_text_field() else {
            return;
        };
        let at = Self::byte_offset(&field.value, cursor - 1);
        field.value.remove(at);
        self.cursor -= 1;
        self.edited();
    }

    pub fn delete(&mut self) {
        let cursor = self.cursor;
        let Some(field) = self.focused_text_field() else {
            return;
        };
        if cursor >= field.value.chars().count() {
            return;
        }
        let at = Self::byte_offset(&field.value, cursor);
        field.value.remove(at);
        self.edited();
    }

    fn focused_len(&self) -> usize {
        self.fields
            .get(self.focus)
            .map(|f| f.value.chars().count())
            .unwrap_or(0)
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.focused_len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.focused_len();
    }

    fn step_focus(&mut self, delta: isize) {
        let n = self.fields.len() as isize;
        if n == 0 {
            return;
        }
        let mut i = self.focus as isize;
        for _ in 0..n {
            i = (i + delta).rem_euclid(n);
            if self.fields[i as usize].enabled {
                self.focus_field(i as usize);
                return;
            }
        }
    }

    pub fn focus_next(&mut self) {
        self.step_focus(1);
    }

    pub fn focus_prev(&mut self) {
        self.step_focus(-1);
    }

    /// Focus a field, with the cursor at the end of its value. Disabled
    /// fields can't be focused.
    pub fn focus_field(&mut self, i: usize) {
        if self.fields.get(i).map(|f| f.enabled).unwrap_or(false) {
            self.focus = i;
            self.cursor = self.focused_len();
            self.composing = None;
        }
    }

    /// Move a choice field to its next (`delta > 0`) or previous option.
    pub fn choice_step(&mut self, delta: isize) {
        let focus = self.focus;
        let Some(field) = self.fields.get_mut(focus) else {
            return;
        };
        if !field.enabled {
            return;
        }
        let FieldKind::Choice(options) = &field.kind else {
            return;
        };
        if options.is_empty() {
            return;
        }
        let n = options.len() as isize;
        let current = options.iter().position(|o| *o == field.value).unwrap_or(0) as isize;
        field.value = options[(current + delta).rem_euclid(n) as usize].clone();
        self.edited();
    }

    /// Pick a specific option of a choice field.
    pub fn choose(&mut self, field: usize, option: &str) {
        if let Some(f) = self.fields.get_mut(field) {
            if f.enabled {
                f.value = option.to_string();
                self.focus_field(field);
                self.edited();
            }
        }
    }

    /// The value as it should be drawn.
    pub fn display_value(&self, i: usize) -> String {
        let Some(field) = self.fields.get(i) else {
            return String::new();
        };
        match field.kind {
            FieldKind::Password if !self.reveal_password => "•".repeat(field.value.chars().count()),
            _ => field.value.clone(),
        }
    }

    /// The IME's uncommitted text as field `i` shows it: masked like the
    /// value in a password field.
    pub fn display_composing(&self, i: usize) -> String {
        let composing = self.composing.clone().unwrap_or_default();
        match self.fields.get(i).map(|f| &f.kind) {
            Some(FieldKind::Password) if !self.reveal_password => {
                "•".repeat(composing.chars().count())
            }
            _ => composing,
        }
    }

    /// The placeholder to draw in place of field `i`'s value: only while
    /// nothing is in it, committed or still being composed.
    pub fn placeholder_shown(&self, i: usize) -> Option<&str> {
        let field = self.fields.get(i)?;
        let composing =
            self.focus == i && self.composing.as_deref().map_or(false, |c| !c.is_empty());
        if field.value.is_empty() && !composing && !field.placeholder.is_empty() {
            Some(&field.placeholder)
        } else {
            None
        }
    }

    pub fn error_for(&self, i: usize) -> Option<&str> {
        self.errors
            .iter()
            .find(|(f, _)| *f == i)
            .map(|(_, e)| e.as_str())
    }
}

use crate::termwindow::box_model::*;
use crate::termwindow::modal::Modal;
use crate::termwindow::{DimensionContext, TermWindowNotif, UIItemType};
use crate::utilsprites::RenderMetrics;
use crate::TermWindow;
use ::window::{MouseEventKind as WMEK, MousePress, WindowOps};
use config::keyassignment::KeyAssignment;
use config::Dimension;
use std::cell::{Ref, RefCell};
use termwiz::cell::unicode_column_width;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};
use window::color::LinearRgba;

/// What a `UIItemType::ModalItem(n)` index refers to.
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Field(usize),
    Choice(usize, String),
    Button(usize),
    Reveal,
}

/// How long a form that pops up by itself ignores Enter.
pub const EARLY_ENTER: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether an Enter at `now` comes too soon after a guarded form opened to
/// be an answer to it, rather than typing meant for the terminal.
fn enter_too_early(
    guard: Option<(std::time::Instant, std::time::Duration)>,
    now: std::time::Instant,
) -> bool {
    match guard {
        Some((opened, wait)) => now.saturating_duration_since(opened) < wait,
        None => false,
    }
}

type Validate = Box<dyn Fn(&FormState) -> Vec<(usize, String)>>;
type Submit = Box<dyn Fn(&FormState, &mut TermWindow)>;
type Change = Box<dyn Fn(&mut FormState)>;
type Extra = Box<dyn Fn(usize, &mut TermWindow)>;

pub struct FormModal {
    state: RefCell<FormState>,
    element: RefCell<Option<Vec<ComputedElement>>>,
    targets: RefCell<Vec<Target>>,
    cursor_rect: RefCell<Option<::window::Rect>>,
    validate: Validate,
    on_submit: Submit,
    on_change: Change,
    on_extra: Extra,
    on_cancel: Box<dyn Fn(&mut TermWindow)>,
    enter_guard: Option<(std::time::Instant, std::time::Duration)>,
}

impl FormModal {
    pub fn new(state: FormState) -> Self {
        Self {
            state: RefCell::new(state),
            element: RefCell::new(None),
            targets: RefCell::new(vec![]),
            cursor_rect: RefCell::new(None),
            validate: Box::new(|_| vec![]),
            on_submit: Box::new(|_, _| {}),
            on_change: Box::new(|_| {}),
            on_extra: Box::new(|_, _| {}),
            on_cancel: Box::new(|_| {}),
            enter_guard: None,
        }
    }

    /// For a form that opens by itself rather than at the user's request:
    /// an Enter in its first [`EARLY_ENTER`] was typed for something else,
    /// and is ignored instead of submitting.
    pub fn ignore_early_enter(mut self) -> Self {
        self.enter_guard = Some((std::time::Instant::now(), EARLY_ENTER));
        self
    }

    pub fn validate(mut self, f: impl Fn(&FormState) -> Vec<(usize, String)> + 'static) -> Self {
        self.validate = Box::new(f);
        self
    }

    pub fn on_submit(mut self, f: impl Fn(&FormState, &mut TermWindow) + 'static) -> Self {
        self.on_submit = Box::new(f);
        self
    }

    /// Called after every edit, e.g. to enable or disable fields.
    pub fn on_change(mut self, f: impl Fn(&mut FormState) + 'static) -> Self {
        (f)(&mut self.state.borrow_mut());
        self.on_change = Box::new(f);
        self
    }

    pub fn on_extra(mut self, f: impl Fn(usize, &mut TermWindow) + 'static) -> Self {
        self.on_extra = Box::new(f);
        self
    }

    /// Called when the form is dismissed with Esc or its cancel button.
    pub fn on_cancel(mut self, f: impl Fn(&mut TermWindow) + 'static) -> Self {
        self.on_cancel = Box::new(f);
        self
    }

    fn cancel(&self, term_window: &mut TermWindow) {
        term_window.cancel_modal();
        (self.on_cancel)(term_window);
    }

    /// Set a field from outside, e.g. after a file picker returns.
    pub fn set_field_value(&self, field: usize, value: &str, term_window: &mut TermWindow) {
        {
            let mut state = self.state.borrow_mut();
            if let Some(f) = state.fields.get_mut(field) {
                f.value = value.to_string();
            }
            state.focus_field(field);
            state.errors.retain(|(i, _)| *i != field);
        }
        self.changed(term_window);
    }

    fn changed(&self, term_window: &mut TermWindow) {
        (self.on_change)(&mut self.state.borrow_mut());
        self.element.borrow_mut().take();
        if let Some(window) = term_window.window.as_ref() {
            window.invalidate();
        }
    }

    fn submit(&self, term_window: &mut TermWindow) {
        let errors = (self.validate)(&self.state.borrow());
        if errors.is_empty() {
            let state = self.state.borrow().clone();
            // Close first: submitting may open another modal in our place.
            term_window.cancel_modal();
            (self.on_submit)(&state, term_window);
        } else {
            {
                let mut state = self.state.borrow_mut();
                let first = errors[0].0;
                state.errors = errors;
                state.focus_field(first);
            }
            self.changed(term_window);
        }
    }

    fn paste(&self, term_window: &mut TermWindow) {
        let Some(window) = term_window.window.clone() else {
            return;
        };
        let future = window.get_clipboard(::window::Clipboard::Clipboard);
        promise::spawn::spawn(async move {
            match future.await {
                Ok(text) => {
                    window.notify(TermWindowNotif::Apply(Box::new(move |tw| {
                        if let Some(modal) = tw.get_modal() {
                            modal.insert_text(&text, tw);
                        }
                    })));
                }
                Err(err) => log::error!("reading the clipboard: {err:#}"),
            }
        })
        .detach();
    }

    fn activate(&self, target: Target, term_window: &mut TermWindow) {
        match target {
            Target::Field(i) => {
                self.state.borrow_mut().focus_field(i);
                self.changed(term_window);
            }
            Target::Choice(i, option) => {
                self.state.borrow_mut().choose(i, &option);
                self.changed(term_window);
            }
            Target::Reveal => {
                {
                    let mut state = self.state.borrow_mut();
                    state.reveal_password = !state.reveal_password;
                }
                self.changed(term_window);
            }
            Target::Button(b) => {
                let button = self.state.borrow().buttons.get(b).cloned();
                match button {
                    Some(FormButton::Primary(_)) => self.submit(term_window),
                    Some(FormButton::Cancel(_)) => self.cancel(term_window),
                    Some(FormButton::Extra(_, field)) => (self.on_extra)(field, term_window),
                    None => {}
                }
            }
        }
    }

    fn compute(&self, term_window: &mut TermWindow) -> anyhow::Result<Vec<ComputedElement>> {
        let font = term_window.fonts.default_font()?;
        let metrics = RenderMetrics::with_font_metrics(&font.metrics());
        let cell_w = metrics.cell_size.width as f32;
        use crate::termwindow::theme::{fill, lin, palette, px, rounded};
        let dpi = term_window.dimensions.dpi as f32;
        let error_color = lin(palette::DANGER);
        let accent = lin(palette::ACCENT);
        let card = lin(palette::RAISED);
        let text_linear = lin(palette::TEXT);
        let text_color: InheritableColor = text_linear.into();
        let dim_text = lin(palette::TEXT_FAINT);
        let label_text = lin(palette::TEXT_MUTED);
        let well = lin(palette::PANEL);
        let white = lin(palette::WHITE);
        let one = px(dpi, 1.).max(1.).floor();
        let radius = px(dpi, 6.);
        // A field or option: a rounded well inside a one pixel ring. Nested
        // because box_model paints rounded corners in the border colour.
        let ringed = |inner: Element, edge: LinearRgba, hover_edge: Option<LinearRgba>| {
            let mut outer = Element::new(&font, ElementContent::Children(vec![inner]))
                .padding(BoxDimension::new(Dimension::Pixels(one)))
                .border_corners(Some(rounded(radius)))
                .colors(fill(edge, edge));
            if let Some(hover) = hover_edge {
                outer = outer.hover_colors(Some(fill(hover, hover)));
            }
            outer
        };
        let well_colors = |fg: LinearRgba| fill(well, fg);
        let button_colors = |bg: LinearRgba| fill(bg, white);
        let placeholder_text: InheritableColor = dim_text.into();

        let state = self.state.borrow();
        let mut targets = vec![];
        let mut tag = |t: Target| {
            targets.push(t);
            UIItemType::ModalItem(targets.len() - 1)
        };

        let dims = term_window.dimensions;
        let total_cells = ((dims.pixel_width as f32 / cell_w) as usize).saturating_sub(4);
        let width_cells = 60.min(total_cells).max(20);
        let label_cells = state
            .fields
            .iter()
            .map(|f| unicode_column_width(&f.label, None))
            .max()
            .unwrap_or(0)
            + 2;
        let value_cells = width_cells.saturating_sub(label_cells + 10).max(8);

        let plain = |text: String| {
            Element::new(&font, ElementContent::Text(text)).colors(ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: text_color.clone(),
            })
        };
        let block = |children: Vec<Element>| {
            Element::new(&font, ElementContent::Children(children))
                .display(DisplayType::Block)
                .min_width(Some(Dimension::Percent(1.)))
                .margin(BoxDimension {
                    left: Dimension::Cells(0.),
                    right: Dimension::Cells(0.),
                    top: Dimension::Cells(0.),
                    bottom: Dimension::Cells(0.15),
                })
        };

        let mut rows = vec![];
        rows.push(block(vec![plain(state.title.clone()).colors(
            ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: lin(palette::TEXT_STRONG).into(),
            },
        )]));
        rows.push(block(vec![plain(String::new())]));
        for line in &state.message {
            rows.push(block(vec![plain(line.clone())]));
        }

        for (i, field) in state.fields.iter().enumerate() {
            let focused = i == state.focus;
            let mut row = vec![];
            // Widths are set in cells rather than by padding with spaces: a
            // CJK glyph from a fallback font need not be exactly two cells
            // wide, and padding would then leave the boxes ragged.
            let label = plain(field.label.clone())
                .min_width(Some(Dimension::Cells(label_cells as f32)))
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: if field.enabled { label_text } else { dim_text }.into(),
                });
            row.push(label);

            match &field.kind {
                FieldKind::Choice(options) => {
                    for option in options {
                        let selected = *option == field.value;
                        let text = format!(" {} {} ", if selected { "●" } else { "○" }, option);
                        let fg = if !field.enabled {
                            dim_text
                        } else if selected {
                            accent
                        } else {
                            label_text
                        };
                        let edge = if selected && focused {
                            accent
                        } else {
                            lin(palette::CARD_EDGE)
                        };
                        let mut inner = Element::new(&font, ElementContent::Text(text))
                            .border_corners(Some(rounded(radius - one)))
                            .colors(well_colors(fg));
                        if field.enabled {
                            inner = inner.item_type(tag(Target::Choice(i, option.clone())));
                        }
                        row.push(
                            ringed(inner, edge, field.enabled.then(|| lin(palette::INPUT_EDGE)))
                                .margin(BoxDimension {
                                    left: Dimension::Cells(0.),
                                    right: Dimension::Cells(1.),
                                    top: Dimension::Cells(0.),
                                    bottom: Dimension::Cells(0.),
                                }),
                        );
                    }
                }
                FieldKind::Text | FieldKind::Password => {
                    let placeholder = state.placeholder_shown(i);
                    let mut shown = state.display_value(i);
                    if focused && field.enabled {
                        let at = FormState::byte_offset(&shown, state.cursor);
                        let composing = state.display_composing(i);
                        shown.insert_str(at, &format!("{composing}\u{258f}"));
                    }
                    if let Some(placeholder) = placeholder {
                        shown.push_str(placeholder);
                    }
                    let mut c = if !field.enabled {
                        fill(lin(palette::BASE), dim_text)
                    } else if focused {
                        well_colors(lin(palette::TEXT_STRONG))
                    } else {
                        well_colors(text_linear)
                    };
                    if placeholder.is_some() && field.enabled {
                        c.text = placeholder_text.clone();
                    }
                    let edge = if focused && field.enabled {
                        accent
                    } else {
                        lin(palette::CARD_EDGE)
                    };
                    let mut e = Element::new(&font, ElementContent::Text(shown))
                        .colors(c)
                        .border_corners(Some(rounded(radius - one)))
                        .min_width(Some(Dimension::Cells(value_cells as f32)));
                    if field.enabled {
                        e = e.item_type(tag(Target::Field(i)));
                    }
                    row.push(ringed(e, edge, None));

                    if field.kind == FieldKind::Password && field.enabled {
                        let text = if state.reveal_password {
                            " 隐藏 "
                        } else {
                            " 显示 "
                        };
                        row.push(
                            Element::new(&font, ElementContent::Text(text.to_string()))
                                .border_corners(Some(rounded(radius)))
                                .colors(fill(lin(palette::PILL), text_linear))
                                .hover_colors(Some(fill(lin(palette::CARD_EDGE), text_linear)))
                                .margin(BoxDimension {
                                    left: Dimension::Cells(1.),
                                    right: Dimension::Cells(0.),
                                    top: Dimension::Cells(0.),
                                    bottom: Dimension::Cells(0.),
                                })
                                .item_type(tag(Target::Reveal)),
                        );
                    }
                }
            }

            for (b, button) in state.buttons.iter().enumerate() {
                if let FormButton::Extra(label, f) = button {
                    if *f == i && field.enabled {
                        row.push(
                            Element::new(&font, ElementContent::Text(format!(" {label} ")))
                                .border_corners(Some(rounded(radius)))
                                .colors(fill(lin(palette::PILL), text_linear))
                                .hover_colors(Some(fill(lin(palette::CARD_EDGE), text_linear)))
                                .margin(BoxDimension {
                                    left: Dimension::Cells(1.),
                                    right: Dimension::Cells(0.),
                                    top: Dimension::Cells(0.),
                                    bottom: Dimension::Cells(0.),
                                })
                                .item_type(tag(Target::Button(b))),
                        );
                    }
                }
            }
            rows.push(block(row));

            if let Some(error) = state.error_for(i) {
                rows.push(block(vec![Element::new(
                    &font,
                    ElementContent::Text(error.to_string()),
                )
                .margin(BoxDimension {
                    left: Dimension::Cells(label_cells as f32),
                    right: Dimension::Cells(0.),
                    top: Dimension::Cells(0.),
                    bottom: Dimension::Cells(0.),
                })
                .colors(ElementColors {
                    border: BorderColor::default(),
                    bg: InheritableColor::Inherited,
                    text: error_color.into(),
                })]));
            }
        }

        rows.push(block(vec![plain(String::new())]));
        let mut buttons = vec![];
        for (b, button) in state.buttons.iter().enumerate() {
            let (label, c, hover) = match button {
                FormButton::Primary(l) if state.danger => (
                    l,
                    button_colors(lin(palette::DANGER_BUTTON)),
                    button_colors(lin(palette::DANGER)),
                ),
                FormButton::Primary(l) => (
                    l,
                    button_colors(lin(palette::MENU_HOVER)),
                    button_colors(accent),
                ),
                FormButton::Cancel(l) => (
                    l,
                    fill(lin(palette::PILL), text_linear),
                    fill(lin(palette::CARD_EDGE), text_linear),
                ),
                FormButton::Extra(..) => continue,
            };
            buttons.push(
                Element::new(&font, ElementContent::Text(format!("  {label}  ")))
                    .border_corners(Some(rounded(radius)))
                    .colors(c)
                    .hover_colors(Some(hover))
                    .margin(BoxDimension {
                        left: Dimension::Cells(0.),
                        right: Dimension::Cells(2.),
                        top: Dimension::Cells(0.),
                        bottom: Dimension::Cells(0.),
                    })
                    .item_type(tag(Target::Button(b))),
            );
        }
        rows.push(block(buttons));
        rows.push(block(vec![plain(state.key_hint().to_string()).colors(
            ElementColors {
                border: BorderColor::default(),
                bg: InheritableColor::Inherited,
                text: dim_text.into(),
            },
        )]));

        let desired_pixel_width = width_cells as f32 * cell_w;
        let inner = Element::new(&font, ElementContent::Children(rows))
            .colors(fill(card, text_linear))
            .padding(BoxDimension {
                left: Dimension::Cells(1.),
                right: Dimension::Cells(1.),
                top: Dimension::Cells(0.5),
                bottom: Dimension::Cells(0.5),
            })
            .border_corners(Some(rounded(px(dpi, 7.))))
            .min_width(Some(Dimension::Pixels(desired_pixel_width - one * 2.)));
        let element = Element::new(&font, ElementContent::Children(vec![inner]))
            .colors(fill(lin(palette::CARD_EDGE), text_linear))
            .padding(BoxDimension::new(Dimension::Pixels(one)))
            .border_corners(Some(rounded(px(dpi, 8.))))
            .min_width(Some(Dimension::Pixels(desired_pixel_width)));

        drop(state);
        *self.targets.borrow_mut() = targets;

        let border = term_window.get_os_border();
        let top_bar_height = if term_window.show_tab_bar && !term_window.config.tab_bar_at_bottom {
            term_window.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let top_pixel_y =
            top_bar_height + border.top.get() as f32 + 2. * metrics.cell_size.height as f32;
        let x = ((dims.pixel_width as f32 - desired_pixel_width) / 2.).max(0.);

        let computed = term_window.compute_element(
            &LayoutContext {
                height: DimensionContext {
                    dpi: dims.dpi as f32,
                    pixel_max: dims.pixel_height as f32,
                    pixel_cell: metrics.cell_size.height as f32,
                },
                width: DimensionContext {
                    dpi: dims.dpi as f32,
                    pixel_max: dims.pixel_width as f32,
                    pixel_cell: cell_w,
                },
                bounds: euclid::rect(
                    x,
                    top_pixel_y,
                    desired_pixel_width,
                    (dims.pixel_height as f32 - top_pixel_y).max(0.),
                ),
                metrics: &metrics,
                gl_state: term_window.render_state.as_ref().unwrap(),
                zindex: 100,
            },
            &element,
        )?;

        // Put the IME candidate window at the text cursor of the focused field.
        let state = self.state.borrow();
        let focus_target = Target::Field(state.focus);
        let index = self
            .targets
            .borrow()
            .iter()
            .position(|t| *t == focus_target);
        *self.cursor_rect.borrow_mut() = index.and_then(|index| {
            let item = computed
                .ui_items()
                .into_iter()
                .find(|item| item.item_type == UIItemType::ModalItem(index))?;
            let shown = state.display_value(state.focus);
            let prefix: String = shown.chars().take(state.cursor).collect();
            let composing = state.display_composing(state.focus);
            let cells =
                unicode_column_width(&prefix, None) + unicode_column_width(&composing, None);
            Some(::window::Rect::new(
                ::window::Point::new(
                    item.x as isize + (cells as f32 * cell_w) as isize,
                    item.y as isize,
                ),
                metrics.cell_size,
            ))
        });

        let mut all = term_window.theme_shadow(computed.bounds, px(dpi, 8.), 100)?;
        all.push(computed);
        Ok(all)
    }
}

impl Modal for FormModal {
    fn perform_assignment(&self, assignment: &KeyAssignment, term_window: &mut TermWindow) -> bool {
        match assignment {
            KeyAssignment::PasteFrom(_) => {
                self.paste(term_window);
                true
            }
            _ => false,
        }
    }

    fn mouse_event(&self, _event: MouseEvent, _term_window: &mut TermWindow) -> anyhow::Result<()> {
        // Clicks arrive through `ui_item_event` instead, in window pixels.
        Ok(())
    }

    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool> {
        let is_choice = {
            let state = self.state.borrow();
            matches!(
                state.fields.get(state.focus).map(|f| &f.kind),
                Some(FieldKind::Choice(_))
            )
        };
        match (key, mods) {
            (KeyCode::Escape, _) => {
                self.cancel(term_window);
                return Ok(true);
            }
            (KeyCode::Enter, KeyModifiers::NONE) => {
                if !enter_too_early(self.enter_guard, std::time::Instant::now()) {
                    self.submit(term_window);
                }
                return Ok(true);
            }
            (KeyCode::Char('v') | KeyCode::Char('V'), m) if m.contains(KeyModifiers::CTRL) => {
                self.paste(term_window);
                return Ok(true);
            }
            (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::DownArrow, KeyModifiers::NONE) => {
                self.state.borrow_mut().focus_next()
            }
            (KeyCode::Tab, KeyModifiers::SHIFT) | (KeyCode::UpArrow, KeyModifiers::NONE) => {
                self.state.borrow_mut().focus_prev()
            }
            (KeyCode::LeftArrow, KeyModifiers::NONE) if is_choice => {
                self.state.borrow_mut().choice_step(-1)
            }
            (KeyCode::RightArrow, KeyModifiers::NONE)
            | (KeyCode::Char(' '), KeyModifiers::NONE)
                if is_choice =>
            {
                self.state.borrow_mut().choice_step(1)
            }
            (KeyCode::LeftArrow, KeyModifiers::NONE) => self.state.borrow_mut().left(),
            (KeyCode::RightArrow, KeyModifiers::NONE) => self.state.borrow_mut().right(),
            (KeyCode::Home, KeyModifiers::NONE) => self.state.borrow_mut().home(),
            (KeyCode::End, KeyModifiers::NONE) => self.state.borrow_mut().end(),
            (KeyCode::Backspace, KeyModifiers::NONE) => self.state.borrow_mut().backspace(),
            (KeyCode::Delete, KeyModifiers::NONE) => self.state.borrow_mut().delete(),
            (KeyCode::Char(c), KeyModifiers::NONE) | (KeyCode::Char(c), KeyModifiers::SHIFT) => {
                let shortcut = self.state.borrow().shortcut(c);
                match shortcut {
                    Some(true) => {
                        self.submit(term_window);
                        return Ok(true);
                    }
                    Some(false) => {
                        self.cancel(term_window);
                        return Ok(true);
                    }
                    None => self.state.borrow_mut().insert(&c.to_string()),
                }
            }
            _ => return Ok(false),
        }
        self.changed(term_window);
        Ok(true)
    }

    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>> {
        if self.element.borrow().is_none() {
            let computed = self.compute(term_window)?;
            self.element.borrow_mut().replace(computed);
        }
        Ok(Ref::map(self.element.borrow(), |v| {
            v.as_ref().unwrap().as_slice()
        }))
    }

    fn reconfigure(&self, _term_window: &mut TermWindow) {
        self.element.borrow_mut().take();
    }

    fn insert_text(&self, text: &str, term_window: &mut TermWindow) -> bool {
        {
            let mut state = self.state.borrow_mut();
            state.composing = None;
            state.insert(text);
        }
        self.changed(term_window);
        true
    }

    fn set_composing(&self, text: Option<String>, term_window: &mut TermWindow) -> bool {
        self.state.borrow_mut().composing = text.filter(|t| !t.is_empty());
        self.changed(term_window);
        true
    }

    fn text_cursor_rect(&self, _term_window: &TermWindow) -> Option<::window::Rect> {
        *self.cursor_rect.borrow()
    }

    fn ui_item_event(
        &self,
        index: usize,
        event: &::window::MouseEvent,
        term_window: &mut TermWindow,
    ) -> bool {
        if event.kind != WMEK::Press(MousePress::Left) {
            return true;
        }
        let target = self.targets.borrow().get(index).cloned();
        if let Some(target) = target {
            self.activate(target, term_window);
        }
        true
    }

    fn captures_mouse(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn y_and_n_answer_a_confirmation_but_not_a_form_with_fields() {
        let confirm = crate::sessions::forms::confirm_form("删除", "删除 x？", "删除");
        assert_eq!(confirm.shortcut('y'), Some(true));
        assert_eq!(confirm.shortcut('Y'), Some(true));
        assert_eq!(confirm.shortcut('N'), Some(false));
        assert_eq!(confirm.shortcut('q'), None);
        let rename = FormState::new(
            "重命名",
            vec![Field::text("名称", "a")],
            vec![
                FormButton::Primary("确定".to_string()),
                FormButton::Cancel("取消".to_string()),
            ],
        );
        assert_eq!(rename.shortcut('n'), None, "n is a letter of a name");
        assert_eq!(rename.shortcut('y'), None);
    }

    #[test]
    fn deleting_asks_with_a_danger_button() {
        assert!(crate::sessions::forms::danger_confirm_form("删除", "m", "删除").danger);
        assert!(!crate::sessions::forms::confirm_form("关闭", "m", "关闭").danger);
    }

    #[test]
    fn a_confirmation_mentions_its_keys() {
        let confirm = crate::sessions::forms::confirm_form("删除", "m", "删除");
        assert_eq!(confirm.key_hint(), "Y 确定　N 取消　Enter 确定　Esc 取消");
        assert_eq!(form().key_hint(), "Tab 切换　Enter 确定　Esc 取消");
    }

    fn form() -> FormState {
        FormState::new(
            "测试",
            vec![
                Field::text("名称", "ab"),
                Field::password("密码", "secret"),
                Field::choice("认证方式", &["密码", "私钥"], "密码"),
                Field::text("私钥文件", ""),
            ],
            vec![
                FormButton::Primary("保存".into()),
                FormButton::Cancel("取消".into()),
            ],
        )
    }

    #[test]
    fn enter_right_after_a_guarded_form_opens_is_ignored() {
        let opened = std::time::Instant::now();
        let guard = Some((opened, EARLY_ENTER));
        let ms = std::time::Duration::from_millis;
        assert!(enter_too_early(guard, opened + ms(100)));
        assert!(enter_too_early(guard, opened + ms(999)));
        assert!(!enter_too_early(guard, opened + ms(1000)));
        assert!(
            !enter_too_early(None, opened),
            "unguarded forms take it at once"
        );
    }

    #[test]
    fn new_form_focuses_first_field_at_end() {
        let f = form();
        assert_eq!(f.focus, 0);
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn insert_at_cursor_handles_multibyte() {
        let mut f = form();
        f.left();
        f.insert("中");
        assert_eq!(f.fields[0].value, "a中b");
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn backspace_and_delete_respect_cursor() {
        let mut f = form();
        f.insert("中文");
        f.left();
        f.backspace();
        assert_eq!(f.fields[0].value, "ab文");
        assert_eq!(f.cursor, 2);
        f.delete();
        assert_eq!(f.fields[0].value, "ab");
        f.home();
        f.backspace();
        assert_eq!(f.fields[0].value, "ab");
        f.delete();
        assert_eq!(f.fields[0].value, "b");
        f.end();
        f.delete();
        assert_eq!(f.fields[0].value, "b");
    }

    #[test]
    fn cursor_moves_stop_at_the_ends() {
        let mut f = form();
        f.right();
        assert_eq!(f.cursor, 2);
        f.home();
        f.left();
        assert_eq!(f.cursor, 0);
    }

    #[test]
    fn tab_skips_disabled_fields() {
        let mut f = form();
        f.fields[1].enabled = false;
        f.focus_next();
        assert_eq!(f.focus, 2);
        f.focus_prev();
        assert_eq!(f.focus, 0);
        f.focus_prev();
        assert_eq!(f.focus, 3, "wraps around backwards");
        f.focus_next();
        assert_eq!(f.focus, 0, "wraps around forwards");
    }

    #[test]
    fn focus_lands_at_end_of_value() {
        let mut f = form();
        f.focus_field(1);
        assert_eq!(f.cursor, "secret".chars().count());
    }

    #[test]
    fn disabled_field_cannot_be_focused_by_click() {
        let mut f = form();
        f.fields[3].enabled = false;
        f.focus_field(3);
        assert_eq!(f.focus, 0);
    }

    #[test]
    fn password_display_is_masked_until_revealed() {
        let mut f = form();
        assert_eq!(f.display_value(1), "••••••");
        f.reveal_password = true;
        assert_eq!(f.display_value(1), "secret");
        assert_eq!(f.display_value(0), "ab");
    }

    #[test]
    fn password_composition_is_masked_until_revealed() {
        // An IME composes ASCII too when it is in its native mode; what is
        // being typed into a password must not show.
        let mut f = form();
        f.composing = Some("pass".into());
        assert_eq!(f.display_composing(1), "••••");
        assert_eq!(f.display_composing(0), "pass");
        f.reveal_password = true;
        assert_eq!(f.display_composing(1), "pass");
        f.composing = None;
        assert_eq!(f.display_composing(1), "");
    }

    #[test]
    fn a_blank_field_shows_its_placeholder_until_something_is_typed() {
        let mut f = FormState::new(
            "t",
            vec![Field::text("端口", "").with_placeholder("默认 22")],
            vec![],
        );
        assert_eq!(f.placeholder_shown(0), Some("默认 22"));
        f.composing = Some("2".into());
        assert_eq!(f.placeholder_shown(0), None);
        f.composing = None;
        f.insert("2");
        assert_eq!(f.placeholder_shown(0), None);
        let bare = FormState::new("t", vec![Field::text("x", "")], vec![]);
        assert_eq!(bare.placeholder_shown(0), None, "no placeholder set");
    }

    #[test]
    fn choice_step_wraps() {
        let mut f = form();
        f.focus_field(2);
        f.choice_step(1);
        assert_eq!(f.fields[2].value, "私钥");
        f.choice_step(1);
        assert_eq!(f.fields[2].value, "密码");
        f.choice_step(-1);
        assert_eq!(f.fields[2].value, "私钥");
    }

    #[test]
    fn typing_into_a_choice_does_nothing() {
        let mut f = form();
        f.focus_field(2);
        f.insert("x");
        f.backspace();
        assert_eq!(f.fields[2].value, "密码");
    }

    #[test]
    fn newlines_are_stripped_from_paste() {
        let mut f = form();
        f.insert("c\r\nd\n");
        assert_eq!(f.fields[0].value, "abcd");
        assert_eq!(f.cursor, 4);
    }

    #[test]
    fn value_lookup_by_label() {
        let f = form();
        assert_eq!(f.value("名称"), "ab");
        assert_eq!(f.value("没有这个"), "");
    }

    #[test]
    fn editing_clears_that_fields_error() {
        let mut f = form();
        f.errors = vec![(0, "坏".into()), (1, "也坏".into())];
        f.insert("x");
        assert_eq!(f.errors, vec![(1, "也坏".to_string())]);
    }
}
