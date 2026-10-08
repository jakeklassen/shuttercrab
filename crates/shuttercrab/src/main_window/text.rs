//! Text actions on the screenshot shown, as Snipping Tool's. I, or the Text
//! actions button, reads the screenshot's text with Windows' OCR, on the
//! device, the first time; then the screenshot dims and each line of text
//! stands out, outlined. Drag across words to pick them, as in a document
//! (Shift with a click carries the pick on); the arrow keys move word by
//! word and line by line, with Shift carrying the pick. Ctrl+A picks it
//! all, Ctrl+C copies what is picked, or all the text when nothing is.
//!
//! Q, Quick redact, blacks out the email addresses and phone numbers;
//! Shift+Q opens its options, which choose which and take every redaction
//! off. R, or Redact text in the right-click menu, blacks out what is
//! picked. Redactions are marks: undo takes them back, and Copy and Save as
//! include them. Escape lets go of the pick, then closes Text actions.

use super::{
    MainWindow,
    canvas::{Gesture, Pointer, Shown},
    selection::Placing,
};
use crate::{
    cursors,
    markup::{Mark, Redaction},
    palette::{border, coral, hover, muted, tile},
    shot_view::Xy,
    text::{self, Finds, Rect, Span, Step, Text, WordAt},
};
use gpui_kit::{
    AnyElement, ClickEvent, Context, CursorStyle, Div, Hsla, InteractiveElement as _, IntoElement,
    Keystroke, MouseButton, ParentElement as _, PathBuilder, PathStyle, Role, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window,
    assets::IconName, canvas, component::Icon, deferred, div, point, prelude::FluentBuilder as _,
    px, rgb,
};
use lyon_tessellation::FillOptions;
use std::sync::Arc;

/// How far a redaction reaches past the text, logical pixels of the screen
/// the screenshot was taken on: enough that no edge of a letter shows.
const REDACT_PAD: f32 = 2.;

/// How far a line's outline sits outside its text, logical pixels on
/// screen.
const LINE_PAD: f32 = 3.;

/// The screenshot's text: being read, read, or not readable.
pub(super) enum Reading {
    Pending,
    Read(Arc<Text>),
    Failed(SharedString),
}

/// Text actions, while open.
pub(super) struct TextActions {
    /// The text shown: what was read, less any outside the crop; `None`
    /// until it is read.
    text: Option<Arc<Text>>,
    /// The words picked: where the pick started, and where it reaches.
    pick: Option<(WordAt, WordAt)>,
    menu: Option<TextMenu>,
}

impl TextActions {
    fn picked(&self) -> Option<Span> {
        self.pick.map(|(from, to)| Span::between(from, to))
    }
}

/// A menu of Text actions', open.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TextMenu {
    /// Quick redact's options, with the one the arrow keys are on.
    Redact(usize),
    /// The right-click menu at canvas point `at`.
    Context { at: Xy, highlighted: usize },
}

/// Quick redact's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedactItem {
    Emails,
    Phones,
    RemoveAll,
}

impl RedactItem {
    const ALL: [RedactItem; 3] = [
        RedactItem::Emails,
        RedactItem::Phones,
        RedactItem::RemoveAll,
    ];

    fn label(self) -> &'static str {
        match self {
            RedactItem::Emails => "Email addresses",
            RedactItem::Phones => "Phone numbers",
            RedactItem::RemoveAll => "Remove all redactions",
        }
    }
}

/// The right-click menu's items, as Snipping Tool's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextItem {
    Copy,
    Redact,
}

impl ContextItem {
    const ALL: [ContextItem; 2] = [ContextItem::Copy, ContextItem::Redact];
}

impl Shown {
    /// `text`, less any outside the crop.
    fn visible_text(&self, text: &Arc<Text>) -> Arc<Text> {
        match self.marks.crop() {
            None => text.clone(),
            Some(crop) => Arc::new(text::within(
                text,
                Rect {
                    x: crop.x as f32,
                    y: crop.y as f32,
                    width: crop.width as f32,
                    height: crop.height as f32,
                },
            )),
        }
    }

    /// A press at screenshot pixel `pixel` in Text actions: start picking
    /// from the word there (with `extend`, carry the pick on to it). Away
    /// from the text, let go of the pick.
    pub(super) fn press_text(&mut self, pixel: (f32, f32), extend: bool) -> Option<Gesture> {
        let texting = self.texting.as_mut()?;
        texting.menu = None;
        let text = texting.text.as_ref()?;
        let Some(word) = text::word_near(text, pixel) else {
            texting.pick = None;
            return None;
        };
        texting.pick = Some(match texting.pick {
            Some((from, _)) if extend => (from, word),
            _ => (word, word),
        });
        Some(Gesture::Text)
    }

    /// The pointer dragged on to `pixel` while picking text.
    pub(super) fn drag_text(&mut self, pixel: (f32, f32)) {
        if let Some(texting) = &mut self.texting
            && let (Some(text), Some((from, _))) = (&texting.text, texting.pick)
            && let Some(word) = text::word_toward(text, pixel)
        {
            texting.pick = Some((from, word));
        }
    }

    /// A right-click at canvas point `at` (screenshot pixel `pixel`): on a
    /// word, its menu, picking the word unless it is picked already.
    pub(super) fn open_text_context(&mut self, at: Xy, pixel: (f32, f32)) {
        let Some(texting) = &mut self.texting else {
            return;
        };
        let word = texting
            .text
            .as_ref()
            .and_then(|text| text::word_near(text, pixel));
        let Some(word) = word else {
            texting.menu = None;
            return;
        };
        if !texting.picked().is_some_and(|span| span.contains(word)) {
            texting.pick = Some((word, word));
        }
        texting.menu = Some(TextMenu::Context { at, highlighted: 0 });
    }

    /// The redactions on it.
    fn redactions(&self) -> Vec<Redaction> {
        let marks = self.marks.marks().iter();
        marks
            .filter_map(|mark| match mark {
                Mark::Redaction(hidden) => Some(*hidden),
                _ => None,
            })
            .collect()
    }

    /// Add the redactions not there already, as one change. Returns how
    /// many were added.
    fn add_redactions(&mut self, boxes: Vec<Redaction>) -> usize {
        let marks = self.marks.marks();
        let new: Vec<Mark> = boxes
            .into_iter()
            .map(Mark::Redaction)
            .filter(|mark| !marks.contains(mark))
            .collect();
        let count = new.len();
        self.marks.add_all(new);
        count
    }
}

impl MainWindow {
    pub(super) fn is_texting(&self) -> bool {
        self.shown.as_ref().is_some_and(|s| s.texting.is_some())
    }

    /// Whether Text actions are open.
    pub fn text_actions_open(&self) -> bool {
        self.is_texting()
    }

    /// The text picked in Text actions, as Ctrl+C would copy it, if any.
    pub fn picked_text(&self) -> Option<String> {
        let texting = self.texting()?;
        let hidden = self.shown.as_ref()?.redactions();
        Some(text::copy(
            texting.text.as_ref()?,
            texting.picked()?,
            &hidden,
        ))
    }

    fn texting(&self) -> Option<&TextActions> {
        self.shown.as_ref()?.texting.as_ref()
    }

    fn texting_mut(&mut self) -> Option<&mut TextActions> {
        self.shown.as_mut()?.texting.as_mut()
    }

    /// I, or the Text actions button: open Text actions, or close them.
    pub(super) fn toggle_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_texting() {
            self.stop_text(cx);
        } else {
            self.start_text(window, cx);
        }
    }

    /// Open Text actions, with nothing else in hand, reading the text if
    /// it has not been read yet.
    fn start_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.is_none() {
            return;
        }
        self.cancel_crop(cx);
        self.close_flyouts();
        self.menu = None;
        self.select(None, window, cx);
        self.hand = None;
        let Some(shown) = &mut self.shown else {
            return;
        };
        let text = match &shown.reading {
            Some(Reading::Read(text)) => Some(shown.visible_text(text)),
            _ => None,
        };
        shown.texting = Some(TextActions {
            text,
            pick: None,
            menu: None,
        });
        if shown.reading.is_none() {
            shown.reading = Some(Reading::Pending);
            self.read_text(cx);
        }
        cx.notify();
    }

    /// Close Text actions; the text read stays, for the next time.
    pub(super) fn stop_text(&mut self, cx: &mut Context<Self>) {
        if let Some(shown) = &mut self.shown
            && shown.texting.take().is_some()
        {
            if matches!(shown.gesture, Some(Gesture::Text)) {
                shown.gesture = None;
            }
            cx.notify();
        }
    }

    /// Read the screenshot's text, off the main thread, and keep it for
    /// the screenshot it was read from.
    fn read_text(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let image = shown.shot.image.clone();
        let reading = (self.hooks.read_text)(&shown.shot, cx);
        cx.spawn(async move |this, cx| {
            let result = reading.await;
            let _ = this.update(cx, |this, cx| {
                let Some(shown) = &mut this.shown else {
                    return;
                };
                if !Arc::ptr_eq(&shown.shot.image, &image) {
                    return;
                }
                let reading = match result {
                    Ok(text) => Reading::Read(Arc::new(text::reading_order(text))),
                    Err(why) => Reading::Failed(why.into()),
                };
                let visible = match &reading {
                    Reading::Read(text) => Some(shown.visible_text(text)),
                    _ => None,
                };
                if let Some(texting) = &mut shown.texting {
                    texting.text = visible;
                }
                shown.reading = Some(reading);
                cx.notify();
            });
        })
        .detach();
    }

    /// What Quick redact looks for, as the settings keep it.
    fn finds(&self) -> Finds {
        let settings = self.settings();
        Finds {
            emails: settings.redact_emails,
            phones: settings.redact_phones,
        }
    }

    /// A key in Text actions: Ctrl+A, Ctrl+C, the arrows, Q, Shift+Q, R and
    /// Escape, and those of an open menu. Returns whether the key was used.
    pub(super) fn on_text_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // A toolbar menu open over Text actions has the keys.
        if !self.is_texting() || self.menu.is_some() {
            return false;
        }
        let key = keystroke.key.as_str();
        let modifiers = &keystroke.modifiers;
        if modifiers.control {
            match key {
                "a" if !modifiers.shift => self.pick_all(cx),
                "c" if !modifiers.shift => self.copy_text(false, cx),
                _ => return false,
            }
            return true;
        }
        if modifiers.alt || modifiers.platform {
            return false;
        }
        if self.on_text_menu_key(key, window, cx) {
            return true;
        }
        let single = self.single_keys();
        match key {
            "escape" => {
                let picked = self.texting_mut().and_then(|t| t.pick.take()).is_some();
                if picked {
                    cx.notify();
                } else {
                    self.stop_text(cx);
                }
            }
            "left" => self.step_pick(Step::Back, modifiers.shift, cx),
            "right" => self.step_pick(Step::Forward, modifiers.shift, cx),
            "up" => self.step_pick(Step::Up, modifiers.shift, cx),
            "down" => self.step_pick(Step::Down, modifiers.shift, cx),
            "q" if single && modifiers.shift => self.toggle_redact_menu(cx),
            "q" if single => self.quick_redact(window, cx),
            "r" if single => self.redact_picked(window, cx),
            _ => return false,
        }
        true
    }

    /// A key while a Text actions menu is open: arrows move through it,
    /// Enter or Space chooses, Escape closes. Returns whether it was used.
    fn on_text_menu_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.texting().and_then(|t| t.menu) else {
            return false;
        };
        let (at, len) = match menu {
            TextMenu::Redact(at) => (at, RedactItem::ALL.len()),
            TextMenu::Context { highlighted, .. } => (highlighted, ContextItem::ALL.len()),
        };
        let to = match key {
            "up" => (at + len - 1) % len,
            "down" => (at + 1) % len,
            "escape" => {
                self.close_text_menu();
                cx.notify();
                return true;
            }
            "enter" | "space" => {
                match menu {
                    TextMenu::Redact(_) => self.choose_redact(RedactItem::ALL[at], window, cx),
                    TextMenu::Context { .. } => {
                        self.choose_context_item(ContextItem::ALL[at], window, cx)
                    }
                }
                return true;
            }
            _ => return false,
        };
        if let Some(texting) = self.texting_mut() {
            texting.menu = Some(match menu {
                TextMenu::Redact(_) => TextMenu::Redact(to),
                TextMenu::Context { at, .. } => TextMenu::Context {
                    at,
                    highlighted: to,
                },
            });
        }
        cx.notify();
        true
    }

    /// Close Text actions' open menu. Returns whether one was open.
    pub(super) fn close_text_menu(&mut self) -> bool {
        self.texting_mut()
            .is_some_and(|texting| texting.menu.take().is_some())
    }

    fn toggle_redact_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(texting) = self.texting_mut() {
            texting.menu = match texting.menu {
                Some(TextMenu::Redact(_)) => None,
                _ => Some(TextMenu::Redact(0)),
            };
            cx.notify();
        }
    }

    fn choose_redact(&mut self, item: RedactItem, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            // The options stay open, to change both.
            RedactItem::Emails => self.change(cx, |s| s.redact_emails = !s.redact_emails),
            RedactItem::Phones => self.change(cx, |s| s.redact_phones = !s.redact_phones),
            RedactItem::RemoveAll => {
                self.close_text_menu();
                self.remove_redactions(window, cx);
            }
        }
    }

    fn choose_context_item(
        &mut self,
        item: ContextItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_text_menu();
        match item {
            ContextItem::Copy => self.copy_text(false, cx),
            ContextItem::Redact => self.redact_picked(window, cx),
        }
    }

    fn pick_all(&mut self, cx: &mut Context<Self>) {
        if let Some(texting) = self.texting_mut()
            && let Some(all) = texting.text.as_deref().and_then(text::all)
        {
            texting.pick = Some((all.first, all.last));
            cx.notify();
        }
    }

    /// An arrow key: pick the next word that way (with `extend`, carry the
    /// pick on to it). With nothing picked, the first word.
    fn step_pick(&mut self, step: Step, extend: bool, cx: &mut Context<Self>) {
        let Some(texting) = self.texting_mut() else {
            return;
        };
        let Some(text) = texting.text.clone() else {
            return;
        };
        texting.pick = match texting.pick {
            Some((from, to)) => {
                let next = text::step(&text, to, step);
                Some(if extend { (from, next) } else { (next, next) })
            }
            None => text::all(&text).map(|all| (all.first, all.first)),
        };
        cx.notify();
    }

    /// Copy the words picked (or, with `all` or nothing picked, all the
    /// text) as lines.
    fn copy_text(&mut self, all: bool, cx: &mut Context<Self>) {
        let Some(texting) = self.texting() else {
            return;
        };
        let Some(text) = texting.text.clone() else {
            return;
        };
        let (span, whole) = match texting.picked().filter(|_| !all) {
            Some(span) => (span, false),
            None => match text::all(&text) {
                Some(span) => (span, true),
                None => return,
            },
        };
        let hidden = self
            .shown
            .as_ref()
            .map(Shown::redactions)
            .unwrap_or_default();
        (self.hooks.copy_text)(text::copy(&text, span, &hidden), cx);
        self.show_notice(
            if whole {
                "All text copied"
            } else {
                "Text copied"
            },
            cx,
        );
    }

    /// The redactions' reach past the text, screenshot pixels.
    fn redact_pad(&self) -> f32 {
        self.shown
            .as_ref()
            .map_or(REDACT_PAD, |shown| shown.width_for(REDACT_PAD))
    }

    /// Black out the words picked, and let go of them.
    fn redact_picked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pad = self.redact_pad();
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some(texting) = &mut shown.texting else {
            return;
        };
        let (Some(text), Some(span)) = (texting.text.clone(), texting.picked()) else {
            return;
        };
        texting.pick = None;
        if shown.add_redactions(text::cover(&text, span, pad)) > 0 {
            self.redraw(window, cx);
        }
        cx.notify();
    }

    /// Black out the email addresses and phone numbers the options ask
    /// for, as one change, and say how many.
    fn quick_redact(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let finds = self.finds();
        if !(finds.emails || finds.phones) {
            // Nothing chosen: the options, to choose.
            if let Some(texting) = self.texting_mut() {
                texting.menu = Some(TextMenu::Redact(0));
            }
            cx.notify();
            return;
        }
        let pad = self.redact_pad();
        let Some(shown) = &mut self.shown else {
            return;
        };
        let Some(text) = shown.texting.as_ref().and_then(|t| t.text.clone()) else {
            return;
        };
        let found = text::sensitive(&text, finds);
        let boxes = found.iter().flat_map(|&span| text::cover(&text, span, pad));
        let added = shown.add_redactions(boxes.collect());
        let message: SharedString = match (found.len(), added) {
            (0, _) => match (finds.emails, finds.phones) {
                (true, true) => "No email addresses or phone numbers found".into(),
                (true, false) => "No email addresses found".into(),
                _ => "No phone numbers found".into(),
            },
            (_, 0) => "Already redacted".into(),
            (_, 1) => "1 item redacted".into(),
            (_, n) => format!("{n} items redacted").into(),
        };
        if added > 0 {
            self.redraw(window, cx);
        }
        self.show_notice(message, cx);
    }

    /// Take every redaction off, as one change undo takes back.
    fn remove_redactions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shown) = &mut self.shown else {
            return;
        };
        if shown.marks.remove_redactions() > 0 {
            self.redraw(window, cx);
            self.show_notice("All redactions removed", cx);
        }
    }

    /// The pointer in Text actions, over a canvas of size `canvas`: the
    /// text cursor over text and while picking it; otherwise an open hand
    /// where the screenshot can move. `None` when Text actions are closed.
    pub(super) fn text_pointer(&self, shown: &Shown, canvas: Xy) -> Option<Pointer> {
        let texting = shown.texting.as_ref()?;
        if matches!(shown.gesture, Some(Gesture::Text)) {
            return Some(Pointer::Style(CursorStyle::IBeam));
        }
        let over_text = self
            .pointer
            .zip(texting.text.as_ref())
            .is_some_and(|(at, text)| {
                text::word_near(text, shown.placing(canvas).pixel(at)).is_some()
            });
        Some(if over_text {
            Pointer::Style(CursorStyle::IBeam)
        } else if shown.view.can_pan(canvas) {
            Pointer::Hand(cursors::Hand::Open)
        } else {
            Pointer::Style(CursorStyle::Arrow)
        })
    }

    /// The toolbar's Text actions button, after crop, outlined in coral
    /// while open.
    pub(super) fn text_button(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let open = self.is_texting();
        div()
            .id("tool-text")
            .role(Role::Button)
            .aria_label("Text actions (I)")
            .test_support()
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded_md()
            .border_1()
            .border_color(if open {
                coral()
            } else {
                gpui_kit::transparent_black()
            })
            .when(open, |d| d.bg(tile()))
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_text(window, cx)))
            .child(
                div()
                    .absolute()
                    .top(px(1.))
                    .right(px(3.))
                    .text_size(px(9.))
                    .text_color(muted())
                    .child(self.shown_key("I")),
            )
            .child(Icon::new(IconName::ScanText).size(px(18.)))
    }

    /// Text actions over the screenshot: the screenshot dimmed but for its
    /// lines of text, each outlined; the words picked, highlighted; the
    /// bar of actions, and any menu open.
    pub(super) fn text_overlay(
        &self,
        shown: &Shown,
        placing: Placing,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(texting) = &shown.texting else {
            return Vec::new();
        };
        let mut overlay = Vec::new();
        if let Some(text) = texting.text.as_ref().filter(|t| !t.lines.is_empty()) {
            overlay.extend(lines(text, texting.picked(), placing));
        }
        overlay.push(self.text_bar(shown, texting, cx).into_any_element());
        if let Some(TextMenu::Context { at, highlighted }) = texting.menu {
            overlay.push(Self::text_context_menu(at, highlighted, cx).into_any_element());
        }
        overlay
    }

    /// Text actions' bar, over the top of the screenshot: Copy all text,
    /// Quick redact and its options, and Close; or, until the text is
    /// read, what is happening.
    fn text_bar(
        &self,
        shown: &Shown,
        texting: &TextActions,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let close = bar_button("text-close", "Close", "Esc", IconName::X, |d| {
            d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stop_text(cx)))
        });
        let status = |icon: IconName, text: SharedString| {
            div()
                .id("text-status")
                .aria_label(text.clone())
                .test_support()
                .flex()
                .items_center()
                .gap_2()
                .h(px(36.))
                .px_3()
                .text_sm()
                .text_color(muted())
                .child(Icon::new(icon).size(px(16.)))
                .child(text)
                .into_any_element()
        };
        let read = texting.text.as_ref().filter(|t| !t.lines.is_empty());
        let content: Vec<AnyElement> = match (&shown.reading, read) {
            (_, Some(_)) => self.text_buttons(shown, texting, cx),
            (Some(Reading::Failed(why)), _) => vec![status(IconName::X, why.clone())],
            (Some(Reading::Read(_)), None) => {
                vec![status(IconName::ScanText, "No text found".into())]
            }
            _ => vec![status(IconName::LoaderCircle, "Reading text…".into())],
        };
        div()
            .absolute()
            .top(px(6.))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("text-bar")
                    .role(Role::Toolbar)
                    .test_support()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p(px(4.))
                    .rounded_lg()
                    .bg(rgb(0x2C2C2C))
                    .border_1()
                    .border_color(border())
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(content)
                    .child(div().w(px(1.)).h(px(24.)).mx_1().bg(border()))
                    .child(close),
            )
    }

    /// The bar's buttons once there is text: Copy all text, Quick redact,
    /// and its options, open below their button.
    fn text_buttons(
        &self,
        shown: &Shown,
        texting: &TextActions,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let menu = match texting.menu {
            Some(TextMenu::Redact(at)) => Some(at),
            _ => None,
        };
        let copy_key = if texting.pick.is_none() { "Ctrl+C" } else { "" };
        let copy_all = bar_button(
            "text-copy-all",
            "Copy all text",
            copy_key,
            IconName::ClipboardType,
            |d| d.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_text(true, cx))),
        );
        let quick_redact = bar_button(
            "text-quick-redact",
            "Quick redact",
            self.shown_key("Q"),
            IconName::EyeOff,
            |d| {
                d.on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.quick_redact(window, cx)),
                )
            },
        );
        let options = div()
            .relative()
            .child(
                div()
                    .id("text-redact-options")
                    .role(Role::Button)
                    .aria_label("Quick redact options (Shift+Q)")
                    .test_support()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(36.))
                    .rounded_md()
                    .when(menu.is_some(), |d| d.bg(rgb(0x383838)))
                    .hover(|s| s.bg(rgb(0x383838)))
                    .cursor_pointer()
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_redact_menu(cx)),
                    )
                    .child(Icon::new(IconName::ChevronDown).size(px(14.))),
            )
            .when_some(menu, |d, at| {
                d.child(deferred(self.redact_menu(shown, at, cx)).with_priority(1))
            })
            .into_any_element();
        vec![copy_all, quick_redact, options]
    }

    /// Quick redact's options, below its button: what it looks for, ticked,
    /// and Remove all redactions.
    fn redact_menu(
        &self,
        shown: &Shown,
        highlighted: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let finds = self.finds();
        let any = shown.marks.redactions() > 0;
        let items = RedactItem::ALL.into_iter().enumerate().map(|(i, item)| {
            let (ticked, enabled) = match item {
                RedactItem::Emails => (finds.emails, true),
                RedactItem::Phones => (finds.phones, true),
                RedactItem::RemoveAll => (false, any),
            };
            let mark = match item {
                RedactItem::RemoveAll => {
                    Icon::new(IconName::Trash).size(px(16.)).into_any_element()
                }
                _ if ticked => Icon::new(IconName::Check).size(px(16.)).into_any_element(),
                _ => div().size(px(16.)).into_any_element(),
            };
            let id = SharedString::from(format!("redact-{i}"));
            menu_item(id, item.label(), i == highlighted, |d| {
                d.when(item == RedactItem::RemoveAll, |d| {
                    d.mt_1().border_t_1().border_color(border())
                })
                .when(!enabled, |d| d.opacity(0.4))
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        if let Some(texting) = this.texting_mut() {
                            texting.menu = Some(TextMenu::Redact(i));
                        }
                        this.choose_redact(item, window, cx)
                    }))
                })
                .child(mark)
                .child(item.label())
            })
        });
        div()
            .id("redact-menu")
            .role(Role::Menu)
            .test_support()
            .absolute()
            .top(px(42.))
            .right(px(0.))
            .min_w(px(230.))
            .p(px(5.))
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(items)
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .text_xs()
                    .text_color(muted())
                    .child("Shift+Q opens this; arrows and Enter choose"),
            )
    }

    /// The right-click menu on picked text, at canvas point `at`.
    fn text_context_menu(at: Xy, highlighted: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let items = ContextItem::ALL.into_iter().enumerate().map(|(i, item)| {
            let (label, key, icon) = match item {
                ContextItem::Copy => ("Copy text", "Ctrl+C", IconName::Copy),
                ContextItem::Redact => ("Redact text", "R", IconName::EyeOff),
            };
            let id = SharedString::from(format!("text-context-{i}"));
            menu_item(id, label, i == highlighted, |d| {
                d.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.choose_context_item(item, window, cx)
                }))
                .child(Icon::new(icon).size(px(16.)))
                .child(div().flex_1().child(label))
                .child(div().text_xs().text_color(muted()).child(key))
            })
        });
        deferred(
            div()
                .id("text-context")
                .role(Role::Menu)
                .test_support()
                .absolute()
                .left(px(at.x))
                .top(px(at.y))
                .min_w(px(190.))
                .p(px(5.))
                .rounded_lg()
                .bg(rgb(0x2C2C2C))
                .border_1()
                .border_color(border())
                .shadow_lg()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .children(items),
        )
        .with_priority(1)
    }
}

/// A button of the Text actions bar: an icon, its label and its key;
/// `with` gives it its handler.
fn bar_button(
    id: &'static str,
    label: &'static str,
    key: &'static str,
    icon: IconName,
    with: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
) -> AnyElement {
    let aria = if key.is_empty() {
        SharedString::from(label)
    } else {
        SharedString::from(format!("{label} ({key})"))
    };
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h(px(36.))
        .px_3()
        .rounded_md()
        .hover(|s| s.bg(rgb(0x383838)))
        .cursor_pointer()
        .child(Icon::new(icon).size(px(16.)))
        .child(div().text_sm().child(label))
        .when(!key.is_empty(), |d| {
            d.child(div().text_xs().text_color(muted()).child(key))
        });
    with(button)
        .role(Role::Button)
        .aria_label(aria)
        .test_support()
        .into_any_element()
}

/// A menu item, highlighted while the arrow keys are on it; `with` gives
/// it its handler and content.
fn menu_item(
    id: SharedString,
    label: &'static str,
    highlighted: bool,
    with: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
) -> AnyElement {
    let item = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .h(px(34.))
        .px_3()
        .rounded_md()
        .text_sm()
        .whitespace_nowrap()
        .when(highlighted, |d| d.bg(rgb(0x383838)))
        .hover(|s| s.bg(rgb(0x383838)))
        .cursor_pointer();
    with(item)
        .role(Role::MenuItem)
        .aria_label(label)
        .test_support()
        .into_any_element()
}

/// The text's lines over the screenshot: the rest of it dimmed, each line
/// outlined, and the words `picked` highlighted.
fn lines(text: &Text, picked: Option<Span>, placing: Placing) -> Vec<AnyElement> {
    let (seen, size) = (placing.seen_origin, placing.seen_size);
    // Each line's box in the canvas, a little outside its text, and kept to
    // the part of the screenshot shown.
    let on_screen = |r: Rect| {
        let top_left = placing.at((r.x, r.y));
        let bottom_right = placing.at((r.x + r.width, r.y + r.height));
        let left = (top_left.x - LINE_PAD).max(seen.x);
        let top = (top_left.y - LINE_PAD).max(seen.y);
        let right = (bottom_right.x + LINE_PAD).min(seen.x + size.x);
        let bottom = (bottom_right.y + LINE_PAD).min(seen.y + size.y);
        (left < right && top < bottom).then_some((left, top, right, bottom))
    };
    let boxes: Vec<(f32, f32, f32, f32)> = text
        .lines
        .iter()
        .filter_map(text::line_box)
        .filter_map(on_screen)
        .collect();
    let holes = boxes.clone();
    // The dimming: the part shown, with a hole for each line.
    let dim = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let at = |x: f32, y: f32| point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            let mut path = PathBuilder::fill().with_style(PathStyle::Fill(FillOptions::even_odd()));
            let mut rect = |(l, t, r, b): (f32, f32, f32, f32)| {
                path.move_to(at(l, t));
                path.line_to(at(r, t));
                path.line_to(at(r, b));
                path.line_to(at(l, b));
                path.close();
            };
            rect((seen.x, seen.y, seen.x + size.x, seen.y + size.y));
            for hole in &holes {
                rect(*hole);
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, gpui_kit::black().opacity(0.5));
            }
        },
    )
    .absolute()
    .size_full()
    .into_any_element();
    let mut overlay = vec![dim];
    overlay.extend(boxes.iter().map(|&(l, t, r, b)| {
        div()
            .absolute()
            .left(px(l))
            .top(px(t))
            .w(px(r - l))
            .h(px(b - t))
            .rounded(px(3.))
            .border_1()
            .border_color(gpui_kit::white().opacity(0.75))
            .into_any_element()
    }));
    // The pick: from its first word to its last on each line it reaches.
    if let Some(span) = picked {
        for (index, line) in text.lines.iter().enumerate() {
            let picked = line.words.iter().enumerate().filter(|(word, _)| {
                span.contains(WordAt {
                    line: index,
                    word: *word,
                })
            });
            let Some(across) = picked.map(|(_, w)| w.rect).reduce(text::union) else {
                continue;
            };
            let Some(band) = text::line_box(line) else {
                continue;
            };
            let highlight = Rect {
                y: band.y,
                height: band.height,
                ..across
            };
            if let Some((l, t, r, b)) = on_screen(highlight) {
                overlay.push(
                    // No id: an element with one would take the pointer.
                    div()
                        .absolute()
                        .left(px(l))
                        .top(px(t))
                        .w(px(r - l))
                        .h(px(b - t))
                        .rounded(px(2.))
                        .bg(Hsla::from(rgb(0x3B82F6)).opacity(0.45))
                        .into_any_element(),
                );
            }
        }
    }
    overlay
}
