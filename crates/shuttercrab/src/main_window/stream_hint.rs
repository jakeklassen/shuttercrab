//! Stream's part of the home page: without a host key, what streaming is,
//! what the key is for and the way to paste it in; with one, what happens
//! after New. Under either, what came of pasting a key, or why a stream
//! did not start.

use super::{ButtonFace, MainWindow, Page};
use crate::{
    capture_choice::CaptureMode,
    palette::{border, coral, hover, muted, tile},
    settings_window::HostKeyNote,
};
use gpui_kit::{
    App, ClickEvent, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, assets::IconName,
    component::Icon, div, px,
};

impl MainWindow {
    /// Whether Stream is chosen with no host key to stream with yet.
    pub(super) fn needs_host_key(&self) -> bool {
        self.mode() == CaptureMode::Stream && self.settings().stream_host_key.is_empty()
    }

    /// Keep the host key that is on the clipboard, and say under Stream's
    /// steps what came of it.
    pub(super) fn paste_host_key(&mut self, cx: &mut Context<Self>) {
        let pasted = self.hooks.settings.paste_host_key(cx);
        self.stream_note = Some(pasted.into());
        cx.notify();
    }

    /// A stream did not start, for why: said under Stream's steps. A key
    /// the meeting point turned away is forgotten, so the window asks for
    /// the right one.
    pub fn stream_failed(&mut self, why: &str, cx: &mut Context<Self>) {
        let refused = why.contains(shuttercrab_stream::NOT_THE_HOST_KEY);
        self.change(cx, |s| {
            s.last_mode = CaptureMode::Stream;
            if refused {
                let _ = s.set_host_key("");
            }
        });
        let note = if refused {
            "The meeting point didn't accept that host key. Copy the whole key you were "
                .to_string()
                + "given, then paste it again."
        } else {
            format!("The stream didn't start: {why}")
        };
        self.stream_note = Some(HostKeyNote::Problem(note.into()));
        cx.notify();
    }

    /// With Stream chosen, the middle of the window: without a host key,
    /// what streaming is, what the key is for and the way to add it; with
    /// one, what happens after New.
    pub(super) fn stream_hint(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let column = div().flex().flex_col().items_center().gap_2().px_4();
        let content = if self.needs_host_key() {
            self.host_key_ask(column, cx).into_any_element()
        } else {
            self.stream_steps(column).into_any_element()
        };
        div()
            .id("hint")
            .test_support()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .child(content)
    }

    /// Stream with a host key: what happens after New.
    fn stream_steps(&self, column: Div) -> Div {
        let new = if self.single_keys() {
            "Choose what to stream above, then press N"
        } else {
            "Choose what to stream above, then click New"
        };
        column
            .children(self.stream_note_line())
            .child(new)
            .child(Self::hint_paragraph(
                "Pick the area, check the sound and quality, then Go live. The link is copied \
                 for you to send; whoever opens it watches as it happens.",
            ))
    }

    /// Stream without a host key: what the key is for, and pasting it in
    /// here or in the settings.
    fn host_key_ask(&self, column: Div, cx: &mut Context<Self>) -> Div {
        let paste = Self::hint_button(
            ButtonFace {
                id: "paste-host-key",
                label: "Paste host key",
                icon: IconName::ClipboardPaste,
                key: "Ctrl+V",
            },
            cx.listener(|this, _: &ClickEvent, _, cx| this.paste_host_key(cx)),
        );
        let settings = Self::hint_button(
            ButtonFace {
                id: "streaming-settings",
                label: "Streaming settings",
                icon: IconName::Settings,
                key: "",
            },
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.show(Page::StreamingSettings, window, cx);
            }),
        );
        column
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::KeyRound)
                            .size(px(18.))
                            .text_color(coral()),
                    )
                    .child(
                        div()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Streaming needs your host key"),
                    ),
            )
            .child(Self::hint_paragraph(
                "Stream an area, a window or a display, with its sound, to anyone you send the \
             link to. The key lets this PC open a stream; you paste it in once, and it is \
             kept encrypted to your Windows account. Watching needs no key.",
            ))
            .child(
                div()
                    .mt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(paste)
                    .child(settings),
            )
            .children(self.stream_note_line())
            .child(Self::hint_paragraph(
                "Copy your key first. Don't have one? Ask whoever set up your streaming.",
            ))
    }

    /// A few lines of explanation in the middle of the window.
    fn hint_paragraph(text: &'static str) -> impl IntoElement + use<> {
        div()
            .max_w(px(500.))
            .text_sm()
            .text_center()
            .text_color(muted())
            .child(text)
    }

    /// A button in the middle of the window: an icon, a label and its key.
    fn hint_button<F: Fn(&ClickEvent, &mut Window, &mut App) + 'static>(
        face: ButtonFace,
        click: F,
    ) -> impl IntoElement + use<F> {
        div()
            .id(face.id)
            .role(Role::Button)
            .aria_label(face.label)
            .test_support()
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(32.))
            .px_3()
            .rounded_md()
            .text_sm()
            .bg(tile())
            .border_1()
            .border_color(border())
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(click)
            .child(Icon::new(face.icon).size(px(16.)))
            .child(face.label)
            .children(Self::key_hint(face.key))
    }

    /// What came of pasting a host key, if one was pasted.
    fn stream_note_line(&self) -> Option<impl IntoElement + use<>> {
        let (icon, text, color) = match self.stream_note.clone()? {
            HostKeyNote::Done(what) => (IconName::Check, what.into(), gpui_kit::white()),
            HostKeyNote::Problem(why) => (IconName::X, why, coral()),
        };
        Some(
            div()
                .id("stream-note")
                .test_support()
                .max_w(px(500.))
                .flex()
                .items_center()
                .gap_1p5()
                .text_xs()
                .text_color(color)
                .child(Icon::new(icon).size(px(14.)))
                .child(div().flex_1().child(text)),
        )
    }
}
