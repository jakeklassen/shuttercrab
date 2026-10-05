//! The toolbar's drawing tools, as Snipping Tool's: the pen and the
//! highlighter, each showing its colour, then undo and redo.
//!
//! Clicking the tool in hand, or pressing its key again, opens its flyout:
//! its colours and a size slider over a preview stroke. Arrow keys move
//! through the colours and Enter picks one; [ and ] make the tool in hand
//! smaller and bigger, flyout or not. Choices are kept in the settings.

use super::MainWindow;
use crate::{
    markup::{Brush, Rgb, Tool},
    palette::{border, coral, hover, muted, tile},
};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, MouseButton,
    MouseUpEvent, ParentElement as _, PathBuilder, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
    assets::IconName,
    canvas,
    component::{
        Icon,
        slider::{Slider, SliderEvent, SliderState},
    },
    deferred, div, point,
    prelude::FluentBuilder as _,
    px, rgb,
};
use std::time::Duration;

/// A tool's flyout, open below its button.
pub(super) struct Flyout {
    tool: Tool,
    /// The colour the arrow keys are on.
    highlighted: usize,
    size: Entity<SliderState>,
    _size_changes: Subscription,
}

/// Swatches in a row, as in Snipping Tool.
const COLUMNS: usize = 6;

/// How long the size label stays after [ or ].
const SIZE_NOTE_FOR: Duration = Duration::from_millis(1200);

impl MainWindow {
    /// The colour and size `tool` draws with.
    pub(super) fn brush(&self, tool: Tool) -> Brush {
        self.settings().brush(tool)
    }

    fn set_brush(&mut self, tool: Tool, brush: Brush, cx: &mut Context<Self>) {
        self.change(cx, |settings| settings.set_brush(tool, brush));
    }

    /// Pick up `tool`; if it is in hand already, open or close its flyout.
    pub(super) fn take_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.shown.is_none() {
            return;
        }
        if self.tool == Some(tool) {
            match self.flyout.take() {
                Some(_) => {}
                None => self.open_flyout(tool, cx),
            }
        } else {
            self.tool = Some(tool);
            self.flyout = None;
        }
        cx.notify();
    }

    /// Escape: close the flyout, or put the tool down.
    pub(super) fn put_down_tool(&mut self, cx: &mut Context<Self>) {
        if self.flyout.take().is_some() || self.tool.take().is_some() {
            cx.notify();
        }
    }

    fn open_flyout(&mut self, tool: Tool, cx: &mut Context<Self>) {
        let brush = self.brush(tool);
        let sizes = tool.sizes();
        let size = cx.new(|_| {
            SliderState::new()
                .min(*sizes.start())
                .max(*sizes.end())
                .step(1.)
                .default_value(brush.size)
        });
        let _size_changes = cx.subscribe(&size, move |this, _, event: &SliderEvent, cx| {
            if let SliderEvent::Change(value) = event {
                let brush = Brush {
                    size: value.end().round(),
                    ..this.brush(tool)
                };
                this.set_brush(tool, brush, cx);
            }
        });
        let highlighted = tool
            .colors()
            .iter()
            .position(|c| *c == brush.color)
            .unwrap_or(0);
        self.flyout = Some(Flyout {
            tool,
            highlighted,
            size,
            _size_changes,
        });
    }

    fn pick_color(&mut self, tool: Tool, color: Rgb, cx: &mut Context<Self>) {
        let brush = Brush {
            color,
            ..self.brush(tool)
        };
        self.set_brush(tool, brush, cx);
    }

    /// [ and ]: the tool in hand a size smaller or bigger, by a step its
    /// range suits.
    pub(super) fn step_size(&mut self, steps: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool) = self.tool else {
            return;
        };
        let sizes = tool.sizes();
        let step = match tool {
            Tool::Pen => 1.,
            Tool::Highlighter => 4.,
        };
        let brush = self.brush(tool);
        let size = (brush.size + steps * step).clamp(*sizes.start(), *sizes.end());
        self.set_brush(tool, Brush { size, ..brush }, cx);
        self.note_size(cx);
        if let Some(flyout) = &self.flyout {
            flyout
                .size
                .update(cx, |slider, cx| slider.set_value(size, window, cx));
        }
    }

    /// Show the tip, labelled with its size, for a moment.
    fn note_size(&mut self, cx: &mut Context<Self>) {
        self.size_notes += 1;
        self.size_note = true;
        let note = self.size_notes;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SIZE_NOTE_FOR).await;
            // Unless a later change is showing its own.
            let _ = this.update(cx, |this, cx| {
                if this.size_notes == note {
                    this.size_note = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// A key while a flyout is open: arrows move through the colours, Enter
    /// or Space picks one and closes it. Returns whether the key was used.
    pub(super) fn on_flyout_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(flyout) = &mut self.flyout else {
            return false;
        };
        let count = flyout.tool.colors().len();
        let at = flyout.highlighted;
        flyout.highlighted = match key {
            "left" => (at + count - 1) % count,
            "right" => (at + 1) % count,
            "up" => (at + count - COLUMNS.min(count)) % count,
            "down" => (at + COLUMNS.min(count)) % count,
            "enter" | "space" => {
                let (tool, color) = (flyout.tool, flyout.tool.colors()[at]);
                self.flyout = None;
                self.pick_color(tool, color, cx);
                return true;
            }
            _ => return false,
        };
        cx.notify();
        true
    }

    /// The pen and highlighter, then undo and redo, for the toolbar.
    pub(super) fn drawing_tools(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let marks = self.shown.as_ref().map(|shown| &shown.marks);
        let (can_undo, can_redo) = marks.map_or((false, false), |m| (m.can_undo(), m.can_redo()));
        let separator = || div().w(px(1.)).h(px(28.)).mx_1().bg(border());
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(separator())
            .child(self.tool_button(Tool::Pen, cx))
            .child(self.tool_button(Tool::Highlighter, cx))
            .child(separator())
            .child(Self::history_button(
                "undo",
                "Undo (Ctrl+Z)",
                IconName::Undo2,
                can_undo,
                cx,
                Self::undo,
            ))
            .child(Self::history_button(
                "redo",
                "Redo (Ctrl+Y)",
                IconName::Redo2,
                can_redo,
                cx,
                Self::redo,
            ))
    }

    /// A drawing tool's button: its icon over a bar of its colour, and a
    /// coral outline while it is in hand; its flyout below it when open.
    fn tool_button(&self, tool: Tool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (id, label, icon) = match tool {
            Tool::Pen => ("tool-pen", "Pen (P)", IconName::Pen),
            Tool::Highlighter => ("tool-highlighter", "Highlighter (H)", IconName::Highlighter),
        };
        let brush = self.brush(tool);
        let in_hand = self.tool == Some(tool);
        let flyout = self.flyout.as_ref().filter(|f| f.tool == tool);
        div()
            .relative()
            .child(
                div()
                    .id(id)
                    .role(Role::Button)
                    .aria_label(label)
                    .test_support()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_0p5()
                    .size(px(40.))
                    .rounded_md()
                    .border_1()
                    .border_color(if in_hand {
                        coral()
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(in_hand, |d| d.bg(tile()))
                    .hover(|s| s.bg(hover()))
                    .cursor_pointer()
                    // The press stays here: the window's own press handler
                    // closes the flyout, which the release would reopen.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseUpEvent, _, cx| this.take_tool(tool, cx)),
                    )
                    .child(Icon::new(icon).size(px(18.)))
                    .child(
                        div()
                            .w(px(16.))
                            .h(px(3.))
                            .rounded_full()
                            .bg(rgb(brush.color.hex())),
                    ),
            )
            .when_some(flyout, |d, flyout| {
                d.child(deferred(self.flyout_panel(flyout, brush, cx)).with_priority(1))
            })
            // [ or ] with the pointer off the screenshot, where the tip
            // would show it: the size, under the button, for a moment.
            .when(
                in_hand && flyout.is_none() && self.size_note && self.pointer.is_none(),
                |d| d.child(size_note(tool, brush)),
            )
    }

    /// The flyout: "Colours" in a grid of swatches, the chosen one ringed,
    /// then "Size", a preview stroke and the slider.
    fn flyout_panel(
        &self,
        flyout: &Flyout,
        brush: Brush,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tool = flyout.tool;
        let swatches = tool.colors().iter().enumerate().map(|(i, &color)| {
            let chosen = color == brush.color;
            let highlighted = i == flyout.highlighted;
            div()
                .id(SharedString::from(format!("color-{i}")))
                .role(Role::Button)
                .aria_label(SharedString::from(color.to_hex_string()))
                .test_support()
                .size(px(36.))
                .rounded_full()
                .p(px(3.))
                .border_2()
                .border_color(if chosen {
                    gpui_kit::white()
                } else if highlighted {
                    muted()
                } else {
                    gpui_kit::transparent_black()
                })
                .cursor_pointer()
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                        if let Some(flyout) = &mut this.flyout {
                            flyout.highlighted = i;
                        }
                        this.pick_color(tool, color, cx);
                    }),
                )
                .child(div().size_full().rounded_full().bg(rgb(color.hex())))
        });
        div()
            .id("flyout")
            .test_support()
            .absolute()
            .top(px(46.))
            .left(px(0.))
            .w(px(COLUMNS as f32 * 46. + 30.))
            .p(px(15.))
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .bg(rgb(0x2C2C2C))
            .border_1()
            .border_color(border())
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_sm().child("Colours"))
            .child(div().flex().flex_wrap().gap(px(10.)).children(swatches))
            .child(div().text_sm().child("Size"))
            .child(preview(brush))
            .child(Slider::new(&flyout.size).horizontal())
            .child(
                div()
                    .text_xs()
                    .text_color(muted())
                    .child("Arrows and Enter pick a colour; [ and ] change the size"),
            )
    }

    /// Undo or redo, dimmed when there is nothing to undo or redo.
    fn history_button(
        id: &'static str,
        label: &'static str,
        icon: IconName,
        enabled: bool,
        cx: &mut Context<Self>,
        action: fn(&mut Self, &mut Window, &mut Context<Self>),
    ) -> impl IntoElement + use<> {
        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .test_support()
            .flex()
            .items_center()
            .justify_center()
            .size(px(40.))
            .rounded_md()
            .when(!enabled, |d| d.opacity(0.35))
            .when(enabled, |d| d.hover(|s| s.bg(hover())).cursor_pointer())
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, window, cx| action(this, window, cx)),
            )
            .child(Icon::new(icon).size(px(18.)))
    }
}

/// "Pen 5": a tool's size, labelled, below its button.
fn size_note(tool: Tool, brush: Brush) -> impl IntoElement {
    let name = match tool {
        Tool::Pen => "Pen",
        Tool::Highlighter => "Highlighter",
    };
    let note = SharedString::from(format!("{name} {}", brush.size));
    div()
        .id("size-note")
        .aria_label(note.clone())
        .test_support()
        .absolute()
        .top(px(46.))
        .left(px(0.))
        .px_1p5()
        .py_0p5()
        .rounded_md()
        .bg(rgb(0x2C2C2C))
        .border_1()
        .border_color(border())
        .text_xs()
        .whitespace_nowrap()
        .child(note)
}

/// A wavy stroke in the brush's colour and size, as Snipping Tool's flyout
/// shows. The highlighter's appears solid: on the flyout's dark background
/// its multiply would hide it.
fn preview(brush: Brush) -> impl IntoElement {
    let width = brush.size;
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let (x0, y0) = (bounds.origin.x, bounds.origin.y);
            let (w, h) = (bounds.size.width, bounds.size.height);
            let at = |fx: f32, fy: f32| point(x0 + w * fx, y0 + h * fy);
            let mut path = PathBuilder::stroke(px(width));
            path.move_to(at(0.08, 0.7));
            path.cubic_bezier_to(at(0.5, 0.5), at(0.25, -0.1), at(0.35, 1.1));
            path.cubic_bezier_to(at(0.92, 0.3), at(0.7, -0.1), at(0.8, 1.1));
            if let Ok(path) = path.build() {
                let color: gpui_kit::Hsla = rgb(brush.color.hex()).into();
                window.paint_path(path, color);
            }
        },
    )
    .h(px(70.))
    .w_full()
}
