//! Drives the real main window in a headless GPUI window, with fake hooks
//! standing in for the rest of Shuttercrab.
//!
//!   cargo test -p shuttercrab --test main_window
#![cfg(windows)]

use gpui_kit::{
    App, AppContext as _, Entity, TestAppContext, Window, component::Root, test::TestWindowExt as _,
};
use shuttercrab::{
    capture_choice::{CaptureMode, CaptureTarget},
    main_window::{HOME_SIZE, MainHooks, MainWindow, Page, SHOT_MIN_WIDTH, Shot},
    settings::Settings,
    settings_window::{Diagnostics, Hooks},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

/// What the fake hooks saw.
#[derive(Default)]
struct Seen {
    captures: RefCell<Vec<(CaptureMode, CaptureTarget)>>,
    folders: Cell<u32>,
    quits: Cell<u32>,
    restarts: Cell<u32>,
    copies: Cell<u32>,
    /// How many marks the last copied screenshot had.
    copied_marks: Cell<usize>,
    saves: Cell<u32>,
    paints: Cell<u32>,
    opens: Cell<u32>,
    /// The client sizes the window was fitted to, physical pixels.
    fits: RefCell<Vec<(u32, u32)>>,
    /// The version the fake updater has ready, if any.
    update: RefCell<Option<String>>,
}

struct Opened {
    handle: gpui_kit::AnyWindowHandle,
    view: Entity<MainWindow>,
    settings: Rc<RefCell<Settings>>,
    seen: Rc<Seen>,
}

fn open(cx: &mut TestAppContext) -> Opened {
    open_sized(cx, HOME_SIZE)
}

/// The window at `size`: the test window keeps it, whatever the app asks.
fn open_sized(cx: &mut TestAppContext, size: gpui_kit::Size<gpui_kit::Pixels>) -> Opened {
    cx.update(gpui_kit::init);
    let settings = Rc::new(RefCell::new(Settings::default()));
    let seen = Rc::new(Seen::default());
    let (s1, s2, s3) = (seen.clone(), seen.clone(), seen.clone());
    let (s4, s5) = (seen.clone(), seen.clone());
    let (s6, s7, s8) = (seen.clone(), seen.clone(), seen.clone());
    let (s9, s10) = (seen.clone(), seen.clone());
    let hooks = Rc::new(MainHooks {
        settings: Rc::new(Hooks {
            settings: settings.clone(),
            changed: Rc::new(|_| {}),
            pause_hotkeys: Rc::new(|_| {}),
            apply_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
            probe_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
            windows_takes_print_screen: Rc::new(|| false),
            launch_at_startup: Rc::new(|| false),
            set_launch_at_startup: Rc::new(|_| {}),
            diagnostics: Diagnostics::default(),
        }),
        capture: Rc::new(move |mode, target, _, _| s1.captures.borrow_mut().push((mode, target))),
        open_folder: Rc::new(move |_, _| s2.folders.set(s2.folders.get() + 1)),
        quit: Rc::new(move |_| s3.quits.set(s3.quits.get() + 1)),
        update_ready: Rc::new(move || s4.update.borrow().clone()),
        restart_to_update: Rc::new(move |_| s5.restarts.set(s5.restarts.get() + 1)),
        copy: Rc::new(move |shot, _| {
            s6.copies.set(s6.copies.get() + 1);
            s6.copied_marks.set(shot.marks.len());
            gpui_kit::Task::ready(true)
        }),
        save_as: Rc::new(move |_, _| s7.saves.set(s7.saves.get() + 1)),
        edit_in_paint: Rc::new(move |_, _| s9.paints.set(s9.paints.get() + 1)),
        open_with: Rc::new(move |_, _| s10.opens.set(s10.opens.get() + 1)),
        fit_window: Rc::new(move |_, _, fit| s8.fits.borrow_mut().push((fit.width, fit.height))),
    });
    let out = Rc::new(RefCell::new(None));
    let slot = out.clone();
    let handle = cx.open_window(size, move |window, cx| {
        let view = cx.new(|cx| MainWindow::new(hooks, window, cx));
        slot.replace(Some(view.clone()));
        Root::new(view, window, cx)
    });
    let view = out.borrow_mut().take().unwrap();
    Opened {
        handle: handle.into(),
        view,
        settings,
        seen,
    }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle, |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn press(cx: &mut TestAppContext, opened: &Opened, keys: &[&str]) {
    for key in keys {
        update(cx, opened, |window, cx| window.press(key, cx));
    }
}

fn choice(opened: &Opened) -> (CaptureMode, CaptureTarget, u32, u32) {
    let s = opened.settings.borrow();
    (
        s.last_mode,
        s.last_target,
        s.screenshot_delay,
        s.recording_countdown,
    )
}

#[gpui_kit::test]
fn letters_pick_the_mode_and_target_and_n_starts(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["f"]);
    assert_eq!(choice(&opened).1, CaptureTarget::Freeform);
    press(cx, &opened, &["n"]);
    press(cx, &opened, &["r", "w", "enter"]);
    // Record offers no Window or Freeform: it records an area.
    assert_eq!(
        *opened.seen.captures.borrow(),
        [
            (CaptureMode::Screenshot, CaptureTarget::Freeform),
            (CaptureMode::Record, CaptureTarget::Area)
        ]
    );
    press(cx, &opened, &["d", "s"]);
    assert_eq!(choice(&opened).0, CaptureMode::Screenshot);
    assert_eq!(choice(&opened).1, CaptureTarget::Display);
}

#[gpui_kit::test]
fn clicking_works_like_the_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| {
        window.click("mode-record", cx);
        window.click("target-display", cx);
        window.click("new", cx);
    });
    assert_eq!(
        *opened.seen.captures.borrow(),
        [(CaptureMode::Record, CaptureTarget::Display)]
    );
}

#[gpui_kit::test]
fn t_steps_through_the_delays_for_the_mode(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["t", "t"]);
    assert_eq!(choice(&opened).2, 5);
    press(cx, &opened, &["t", "t"]);
    assert_eq!(choice(&opened).2, 0);
    // Recordings step through the countdowns, and keep their own.
    press(cx, &opened, &["r", "t"]);
    assert_eq!((choice(&opened).2, choice(&opened).3), (0, 3));
}

#[gpui_kit::test]
fn the_delay_menu_chooses_by_click_or_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    update(cx, &opened, |window, cx| window.click("delay", cx));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_some())
    });
    update(cx, &opened, |window, cx| window.click("delay-3", cx));
    assert_eq!(choice(&opened).2, 10);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_none())
    });
    update(cx, &opened, |window, cx| window.click("delay", cx));
    press(cx, &opened, &["down", "enter"]);
    assert_eq!(choice(&opened).2, 0);
    // Escape closes the menu and changes nothing.
    update(cx, &opened, |window, cx| window.click("delay", cx));
    press(cx, &opened, &["escape"]);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("delay-menu").is_none())
    });
    assert_eq!(choice(&opened).2, 0);
}

#[gpui_kit::test]
fn settings_open_in_the_window_and_escape_comes_back(cx: &mut TestAppContext) {
    let opened = open(cx);
    let page = |cx: &mut TestAppContext| cx.update(|cx| opened.view.read(cx).page());
    press(cx, &opened, &[","]);
    assert_eq!(page(cx), Page::Settings);
    update(cx, &opened, |window, _| {
        assert!(window.try_find("settings-window").is_some());
        assert!(window.try_find("toolbar").is_none());
    });
    press(cx, &opened, &["escape"]);
    assert_eq!(page(cx), Page::Home);
    // And through the ⋯ menu and the back button.
    update(cx, &opened, |window, cx| window.click("more", cx));
    update(cx, &opened, |window, cx| window.click("more-0", cx));
    assert_eq!(page(cx), Page::Settings);
    update(cx, &opened, |window, cx| window.click("back", cx));
    assert_eq!(page(cx), Page::Home);
}

#[gpui_kit::test]
fn the_folder_and_quit_have_keys(cx: &mut TestAppContext) {
    let opened = open(cx);
    press(cx, &opened, &["o", "q"]);
    assert_eq!(opened.seen.folders.get(), 1);
    // Q alone does nothing; Ctrl+Q quits.
    assert_eq!(opened.seen.quits.get(), 0);
    press(cx, &opened, &["ctrl-q"]);
    assert_eq!(opened.seen.quits.get(), 1);
}

#[gpui_kit::test]
fn the_version_shows_until_an_update_is_ready_then_u_restarts(cx: &mut TestAppContext) {
    let opened = open(cx);
    let label = |window: &Window, id: &'static str| {
        window
            .try_find(id)
            .and_then(|e| e.label().map(|l| l.to_string()))
    };
    update(cx, &opened, |window, _| {
        let expected = format!("Shuttercrab {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(label(window, "version"), Some(expected));
        assert!(window.try_find("update").is_none());
    });
    // U does nothing without an update.
    press(cx, &opened, &["u"]);
    assert_eq!(opened.seen.restarts.get(), 0);

    opened.seen.update.replace(Some("9.9.9".into()));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("version").is_none());
        assert_eq!(
            label(window, "update").as_deref(),
            Some("Restart to update to 9.9.9")
        );
    });
    press(cx, &opened, &["u"]);
    update(cx, &opened, |window, cx| window.click("update", cx));
    assert_eq!(opened.seen.restarts.get(), 2);
}

/// A plain grey screenshot of `width` × `height` pixels.
fn shot(width: u32, height: u32) -> Shot {
    let rgba = vec![128; (width * height * 4) as usize];
    Shot {
        image: shuttercrab::pixels::render_image(&rgba, width, height),
        png: Arc::new(Vec::new()),
        taken_at: chrono::NaiveDate::from_ymd_opt(2026, 10, 4)
            .unwrap()
            .and_hms_opt(10, 30, 0)
            .unwrap(),
        saved: None,
        scale: None,
        marks: Vec::new(),
        crop: None,
    }
}

#[gpui_kit::test]
fn a_screenshot_shows_with_copy_and_save_as(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    // Nothing to copy or save before a screenshot is shown.
    press(cx, &opened, &["ctrl-c", "ctrl-s"]);
    assert_eq!((opened.seen.copies.get(), opened.seen.saves.get()), (0, 0));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("hint").is_some());
        assert!(window.try_find("copy").is_none());
    });

    let mut scale = 1.;
    update(cx, &opened, |window, cx| {
        scale = window.scale_factor();
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(2800, 1600), window, cx));
    });
    update(cx, &opened, |window, _| {
        assert!(window.try_find("canvas").is_some());
        assert!(window.try_find("hint").is_none());
    });
    // Sized for the screenshot at full size, plus the toolbar, the footer,
    // the padding and the border.
    let expected = |logical: f32| (logical * scale).ceil() as u32;
    assert_eq!(
        opened.seen.fits.borrow().last().copied(),
        Some((
            expected(2800. / scale + 34.),
            expected(1600. / scale + 34. + 59. + 32.)
        ))
    );

    press(cx, &opened, &["ctrl-c", "ctrl-s"]);
    update(cx, &opened, |window, cx| {
        window.click("copy", cx);
        window.click("save-as", cx);
    });
    assert_eq!((opened.seen.copies.get(), opened.seen.saves.get()), (2, 2));
    // The capture keys still work with a screenshot shown.
    press(cx, &opened, &["n"]);
    assert_eq!(opened.seen.captures.borrow().len(), 1);
}

#[gpui_kit::test]
fn a_small_screenshot_keeps_the_home_size(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    let mut scale = 1.;
    update(cx, &opened, |window, cx| {
        scale = window.scale_factor();
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(40, 30), window, cx));
    });
    let physical = |side: f32| (side * scale).ceil() as u32;
    assert_eq!(
        opened.seen.fits.borrow().last().copied(),
        Some((
            physical(SHOT_MIN_WIDTH),
            physical(f32::from(HOME_SIZE.height))
        ))
    );
    // Wide enough for the whole toolbar.
    update(cx, &opened, |window, _| {
        assert!(window.try_find("more").is_some_and(|e| e.visible()));
    });
}

#[gpui_kit::test]
fn ctrl_keys_zoom_the_screenshot(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    let zoom = |cx: &mut TestAppContext| {
        let mut label = None;
        update(cx, &opened, |window, _| {
            label = window
                .try_find("zoom")
                .and_then(|e| e.label().map(|l| l.to_string()));
        });
        label
    };
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(4000, 3000), window, cx));
    });
    // Fitted: a big screenshot in the test window is well under full size.
    let fitted = zoom(cx).unwrap();
    assert_ne!(fitted, "100%");
    press(cx, &opened, &["ctrl-1"]);
    assert_eq!(zoom(cx).as_deref(), Some("100%"));
    press(cx, &opened, &["ctrl-="]);
    assert_eq!(zoom(cx).as_deref(), Some("125%"));
    press(cx, &opened, &["ctrl--", "ctrl--"]);
    assert_eq!(zoom(cx).as_deref(), Some("75%"));
    press(cx, &opened, &["ctrl-0"]);
    assert_eq!(zoom(cx), Some(fitted.clone()));
    // The button switches between fitted and full size.
    update(cx, &opened, |window, cx| window.click("zoom", cx));
    assert_eq!(zoom(cx).as_deref(), Some("100%"));
    update(cx, &opened, |window, cx| window.click("zoom", cx));
    assert_eq!(zoom(cx), Some(fitted));
}

#[gpui_kit::test]
fn the_menu_offers_paint_and_open_with_for_a_screenshot(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    let labels = |cx: &mut TestAppContext| {
        let mut labels = Vec::new();
        update(cx, &opened, |window, cx| window.click("more", cx));
        update(cx, &opened, |window, _| {
            for i in 0..6 {
                if let Some(label) = window
                    .try_find(gpui_kit::SharedString::from(format!("more-{i}")))
                    .and_then(|e| e.label().map(|l| l.to_string()))
                {
                    labels.push(label);
                }
            }
        });
        press(cx, &opened, &["escape"]);
        labels
    };
    assert_eq!(
        labels(cx),
        ["Settings", "Open screenshots folder", "Quit Shuttercrab"]
    );
    // E does nothing without a screenshot.
    press(cx, &opened, &["e"]);
    assert_eq!(opened.seen.paints.get(), 0);

    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    assert_eq!(
        labels(cx),
        [
            "Settings",
            "Open screenshots folder",
            "Edit in Paint",
            "Open with…",
            "Quit Shuttercrab"
        ]
    );
    press(cx, &opened, &["e"]);
    update(cx, &opened, |window, cx| window.click("more", cx));
    update(cx, &opened, |window, cx| window.click("more-3", cx));
    assert_eq!((opened.seen.paints.get(), opened.seen.opens.get()), (1, 1));

    // A saved screenshot is shown in its folder.
    update(cx, &opened, |window, cx| {
        let saved = Shot {
            saved: Some(std::path::PathBuf::from(r"C:\Shots\a.png")),
            ..shot(800, 600)
        };
        opened
            .view
            .update(cx, |view, cx| view.show_shot(saved, window, cx));
    });
    assert_eq!(labels(cx)[1], "Show in folder");
}

/// The window's least size with a screenshot shown.
fn shot_size() -> gpui_kit::Size<gpui_kit::Pixels> {
    gpui_kit::size(gpui_kit::px(SHOT_MIN_WIDTH), HOME_SIZE.height)
}

#[gpui_kit::test]
fn copy_shows_a_check_mark_for_a_moment(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    let label = |cx: &mut TestAppContext| {
        let mut label = None;
        update(cx, &opened, |window, _| {
            label = window
                .try_find("copy")
                .and_then(|e| e.label().map(|l| l.to_string()));
        });
        label
    };
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    assert_eq!(label(cx).as_deref(), Some("Copy"));
    press(cx, &opened, &["ctrl-c"]);
    cx.run_until_parked();
    assert_eq!(label(cx).as_deref(), Some("Copied"));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1600));
    cx.run_until_parked();
    assert_eq!(label(cx).as_deref(), Some("Copy"));
}

/// Drag across the middle of the canvas, `dx` logical pixels sideways.
fn drag_across(cx: &mut TestAppContext, opened: &Opened, dx: f32) {
    update(cx, opened, |window, cx| {
        let canvas = window.find("canvas").bounds();
        let middle = canvas.center();
        let to = gpui_kit::point(middle.x + gpui_kit::px(dx), middle.y);
        window.drag(middle, to, cx);
    });
}

fn marks(cx: &mut TestAppContext, opened: &Opened) -> Vec<shuttercrab::markup::Mark> {
    cx.update(|cx| {
        opened
            .view
            .read(cx)
            .marked_shot()
            .map(|shot| shot.marks)
            .unwrap_or_default()
    })
}

#[gpui_kit::test]
fn the_pen_draws_and_undo_takes_it_back(cx: &mut TestAppContext) {
    use shuttercrab::markup::{Brush, Tool};
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    // Without a tool, a drag on a screenshot that fits draws nothing.
    drag_across(cx, &opened, 40.);
    assert!(marks(cx, &opened).is_empty());

    press(cx, &opened, &["p"]);
    assert_eq!(cx.update(|cx| opened.view.read(cx).tool()), Some(Tool::Pen));
    drag_across(cx, &opened, 40.);
    let drawn = marks(cx, &opened);
    assert_eq!(drawn.len(), 1);
    let stroke = drawn[0].as_stroke().unwrap();
    assert_eq!(stroke.tool, Tool::Pen);
    assert_eq!(stroke.color, Brush::PEN.color);
    assert!(stroke.points.len() >= 2);

    // Copy gives the screenshot with its mark.
    press(cx, &opened, &["ctrl-c"]);
    assert_eq!(opened.seen.copied_marks.get(), 1);

    press(cx, &opened, &["ctrl-z"]);
    assert!(marks(cx, &opened).is_empty());
    press(cx, &opened, &["ctrl-y"]);
    assert_eq!(marks(cx, &opened).len(), 1);
    update(cx, &opened, |window, cx| window.click("undo", cx));
    assert!(marks(cx, &opened).is_empty());
    update(cx, &opened, |window, cx| window.click("redo", cx));
    assert_eq!(marks(cx, &opened).len(), 1);

    // The highlighter draws its own strokes; Escape puts the tool down.
    press(cx, &opened, &["h"]);
    drag_across(cx, &opened, -40.);
    let drawn = marks(cx, &opened);
    assert_eq!(drawn.len(), 2);
    assert_eq!(drawn[1].as_stroke().unwrap().tool, Tool::Highlighter);
    press(cx, &opened, &["escape"]);
    assert_eq!(cx.update(|cx| opened.view.read(cx).tool()), None);
}

#[gpui_kit::test]
fn the_drawing_tools_fit_beside_the_rest_of_the_toolbar(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    update(cx, &opened, |window, _| {
        for id in [
            "tool-pen",
            "tool-highlighter",
            "tool-select",
            "tool-eraser",
            "tool-shapes",
            "undo",
            "redo",
            "copy",
            "more",
        ] {
            assert!(
                window.try_find(id).is_some_and(|e| e.visible()),
                "{id} is out of view"
            );
        }
    });
}

#[gpui_kit::test]
fn the_flyout_picks_colours_and_sizes_and_remembers_them(cx: &mut TestAppContext) {
    use shuttercrab::markup::{Brush, PEN_COLORS, Tool};
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let flyout_open = |cx: &mut TestAppContext| {
        let mut open = false;
        update(cx, &opened, |window, _| {
            open = window.try_find("flyout").is_some()
        });
        open
    };
    // P picks up the pen; P again opens its flyout.
    press(cx, &opened, &["p"]);
    assert!(!flyout_open(cx));
    press(cx, &opened, &["p"]);
    assert!(flyout_open(cx));

    // A click on a swatch: the blue in the third row.
    update(cx, &opened, |window, cx| window.click("color-16", cx));
    let brush = |opened: &Opened| opened.settings.borrow().brush(Tool::Pen);
    assert_eq!(brush(&opened).color, PEN_COLORS[16]);
    // Arrows from there, then Enter: one down and one left.
    press(cx, &opened, &["down", "left", "enter"]);
    assert_eq!(brush(&opened).color, PEN_COLORS[21]);
    assert!(!flyout_open(cx));

    // ] and [ change the size, within the pen's 1 to 24.
    press(cx, &opened, &["]", "]"]);
    assert_eq!(brush(&opened).size, Brush::PEN.size + 2.);
    for _ in 0..30 {
        press(cx, &opened, &["["]);
    }
    assert_eq!(brush(&opened).size, 1.);

    // Escape closes the flyout first, then puts the pen down.
    press(cx, &opened, &["p"]);
    assert!(flyout_open(cx));
    press(cx, &opened, &["escape"]);
    assert!(!flyout_open(cx));
    assert_eq!(cx.update(|cx| opened.view.read(cx).tool()), Some(Tool::Pen));
    press(cx, &opened, &["escape"]);
    assert_eq!(cx.update(|cx| opened.view.read(cx).tool()), None);

    // Clicking the pen picks it up, then opens and closes its flyout, as
    // its key does.
    for open in [false, true, false, true] {
        update(cx, &opened, |window, cx| window.click("tool-pen", cx));
        assert_eq!(flyout_open(cx), open);
    }
}

#[gpui_kit::test]
fn changing_the_size_by_key_shows_it_for_a_moment(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let note = |cx: &mut TestAppContext| {
        let mut note = None;
        update(cx, &opened, |window, _| {
            note = window
                .try_find("size-note")
                .and_then(|e| e.label().map(|l| l.to_string()));
        });
        note
    };
    press(cx, &opened, &["h", "]"]);
    assert_eq!(note(cx).as_deref(), Some("Highlighter 20"));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(1300));
    cx.run_until_parked();
    assert_eq!(note(cx), None);
}

/// Drag across the canvas `dy` logical pixels below its middle, from `x0`
/// to `x1` pixels right of the middle.
fn drag_at(cx: &mut TestAppContext, opened: &Opened, dy: f32, x0: f32, x1: f32) {
    update(cx, opened, |window, cx| {
        let middle = window.find("canvas").bounds().center();
        let at =
            |dx: f32| gpui_kit::point(middle.x + gpui_kit::px(dx), middle.y + gpui_kit::px(dy));
        window.drag(at(x0), at(x1), cx);
    });
}

#[gpui_kit::test]
fn the_eraser_takes_whole_marks_and_erase_all_takes_every_one(cx: &mut TestAppContext) {
    use shuttercrab::main_window::Hand;
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    // Two pen strokes, one above the other.
    press(cx, &opened, &["p"]);
    drag_at(cx, &opened, -30., -40., 40.);
    drag_at(cx, &opened, 30., -40., 40.);
    assert_eq!(marks(cx, &opened).len(), 2);

    // The eraser across the lower one takes it whole, and only it.
    press(cx, &opened, &["x"]);
    assert_eq!(
        cx.update(|cx| opened.view.read(cx).hand()),
        Some(Hand::Erase)
    );
    drag_at(cx, &opened, 30., 0., 5.);
    let left = marks(cx, &opened);
    assert_eq!(left.len(), 1);
    assert!(
        left[0].as_stroke().unwrap().points[0].1 < 300.,
        "the upper stroke stays"
    );
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(marks(cx, &opened).len(), 2);

    // X again opens the eraser's flyout; its one action takes every mark,
    // says so, and is undone in one go.
    press(cx, &opened, &["x"]);
    update(cx, &opened, |window, cx| window.click("erase-all", cx));
    assert!(marks(cx, &opened).is_empty());
    let mut notice = None;
    update(cx, &opened, |window, _| {
        notice = window
            .try_find("notice")
            .and_then(|e| e.label().map(|l| l.to_string()));
    });
    assert_eq!(notice.as_deref(), Some("All mark-ups erased"));
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(marks(cx, &opened).len(), 2);
    // And from the keyboard: X opens the flyout, Enter takes every mark.
    press(cx, &opened, &["x", "enter"]);
    assert!(marks(cx, &opened).is_empty());
}

#[gpui_kit::test]
fn the_shapes_tool_draws_a_shape_with_a_drag_and_nothing_with_a_click(cx: &mut TestAppContext) {
    use shuttercrab::{
        main_window::Hand,
        markup::{Ink, ShapeKind, ShapeStyle},
    };
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let bar_shown = |cx: &mut TestAppContext| {
        let mut shown = false;
        update(cx, &opened, |window, _| {
            shown = window.try_find("shapes-bar").is_some()
        });
        shown
    };
    assert!(!bar_shown(cx));
    // G picks up the shapes, and their bar shows over the screenshot.
    press(cx, &opened, &["g"]);
    assert_eq!(
        cx.update(|cx| opened.view.read(cx).hand()),
        Some(Hand::Shape)
    );
    assert!(bar_shown(cx));

    // A click draws nothing; a drag draws the rectangle, red, unfilled.
    drag_at(cx, &opened, 0., 0., 1.);
    assert!(marks(cx, &opened).is_empty());
    drag_at(cx, &opened, 0., -60., 60.);
    let drawn = marks(cx, &opened);
    assert_eq!(drawn.len(), 1);
    let shape = drawn[0].as_shape().unwrap();
    assert_eq!(shape.kind, ShapeKind::Rectangle);
    assert_eq!(shape.outline, ShapeStyle::DEFAULT.outline);
    assert_eq!(shape.fill, Ink::TRANSPARENT);
    assert!(shape.end.0 - shape.start.0 > 100.);

    // Each shape has its key; the choice is remembered.
    for (key, kind) in [
        ("o", ShapeKind::Oval),
        ("l", ShapeKind::Line),
        ("a", ShapeKind::Arrow),
    ] {
        press(cx, &opened, &[key]);
        assert_eq!(opened.settings.borrow().shape_style().kind, kind);
    }
    drag_at(cx, &opened, 40., -60., 60.);
    let drawn = marks(cx, &opened);
    assert_eq!(drawn[1].as_shape().unwrap().kind, ShapeKind::Arrow);
    // A shape is undone like any mark.
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(marks(cx, &opened).len(), 1);

    // Escape puts the shapes down, and the bar goes.
    press(cx, &opened, &["escape"]);
    assert_eq!(cx.update(|cx| opened.view.read(cx).hand()), None);
    assert!(!bar_shown(cx));
}

#[gpui_kit::test]
fn fill_and_outline_pick_colours_opacity_and_size_from_the_keyboard(cx: &mut TestAppContext) {
    use shuttercrab::markup::{SHAPE_COLORS, ShapeKind};
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let menu_open = |cx: &mut TestAppContext| {
        let mut open = false;
        update(cx, &opened, |window, _| {
            open = window.try_find("shape-menu").is_some()
        });
        open
    };
    let style = |opened: &Opened| opened.settings.borrow().shape_style();
    press(cx, &opened, &["g", "r"]);

    // F opens Fill: Transparent is chosen; one right and Enter picks black.
    press(cx, &opened, &["f"]);
    assert!(menu_open(cx));
    press(cx, &opened, &["right", "enter"]);
    assert!(!menu_open(cx));
    assert_eq!(style(&opened).fill.color, SHAPE_COLORS[1]);
    // Minus and plus change the open menu's opacity, by tens.
    press(cx, &opened, &["f", "-", "-"]);
    assert_eq!(style(&opened).fill.opacity, 80);
    press(cx, &opened, &["+"]);
    assert_eq!(style(&opened).fill.opacity, 90);
    // A click on a swatch picks it, and the menu stays.
    update(cx, &opened, |window, cx| window.click("shape-color-0", cx));
    assert_eq!(style(&opened).fill.color, None);
    assert!(menu_open(cx));

    // T switches to Outline; [ and ] change its size, from 1 to 24.
    press(cx, &opened, &["t"]);
    assert!(menu_open(cx));
    press(cx, &opened, &["]", "]"]);
    assert_eq!(style(&opened).size, 6.);
    for _ in 0..30 {
        press(cx, &opened, &["]"]);
    }
    assert_eq!(style(&opened).size, 24.);

    // A line has no fill: F does nothing, and Fill's menu closes.
    press(cx, &opened, &["escape", "f"]);
    assert!(menu_open(cx));
    press(cx, &opened, &["l"]);
    assert_eq!(style(&opened).kind, ShapeKind::Line);
    assert!(!menu_open(cx));
    press(cx, &opened, &["f"]);
    assert!(!menu_open(cx));

    // Escape closes a menu first, then puts the shapes down.
    press(cx, &opened, &["t", "escape"]);
    assert!(!menu_open(cx));
    assert!(cx.update(|cx| opened.view.read(cx).hand()).is_some());
}

/// Drag on the canvas from `from` to `to`, logical pixels from its middle.
fn drag_between(cx: &mut TestAppContext, opened: &Opened, from: (f32, f32), to: (f32, f32)) {
    update(cx, opened, |window, cx| {
        let middle = window.find("canvas").bounds().center();
        let at = |(dx, dy): (f32, f32)| {
            gpui_kit::point(middle.x + gpui_kit::px(dx), middle.y + gpui_kit::px(dy))
        };
        window.drag(at(from), at(to), cx);
    });
}

fn first_shape(cx: &mut TestAppContext, opened: &Opened) -> shuttercrab::markup::Shape {
    marks(cx, opened)[0].as_shape().unwrap().clone()
}

#[gpui_kit::test]
fn a_new_shape_stays_picked_up_for_keys_to_move_resize_turn_and_delete(cx: &mut TestAppContext) {
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    press(cx, &opened, &["g"]);
    drag_between(cx, &opened, (-60., -40.), (60., 40.));
    let drawn = first_shape(cx, &opened);
    // Its handles show: four corners, and the rotate button.
    update(cx, &opened, |window, _| {
        assert!(window.try_find("handle-3").is_some());
        assert!(window.try_find("turn-handle").is_some());
    });

    // A pixel right; ten wider; a 15° turn.
    press(cx, &opened, &["right"]);
    assert_eq!(first_shape(cx, &opened).start.0, drawn.start.0 + 1.);
    press(cx, &opened, &["shift-right"]);
    let wider = first_shape(cx, &opened);
    assert!((wider.end.0 - wider.start.0 - (drawn.end.0 - drawn.start.0) - 10.).abs() < 0.01);
    press(cx, &opened, &["alt-right"]);
    assert_eq!(first_shape(cx, &opened).angle, 15.);

    // The run of keys undoes in one go, and the shape stays picked up.
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(first_shape(cx, &opened), drawn);
    press(cx, &opened, &["delete"]);
    assert!(marks(cx, &opened).is_empty());
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(marks(cx, &opened).len(), 1);
}

#[gpui_kit::test]
fn select_picks_up_a_shape_to_move_recolour_and_turn_from_its_menu(cx: &mut TestAppContext) {
    use shuttercrab::{main_window::Hand, markup::SHAPE_COLORS};
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let hand = |cx: &mut TestAppContext| cx.update(|cx| opened.view.read(cx).hand());
    let shown = |cx: &mut TestAppContext, id: &'static str| {
        let mut shown = false;
        update(cx, &opened, |window, _| {
            shown = window.try_find(id).is_some()
        });
        shown
    };
    press(cx, &opened, &["g"]);
    drag_between(cx, &opened, (-60., -40.), (60., 40.));
    // Escape lets the shape go, then puts the shapes down.
    press(cx, &opened, &["escape"]);
    assert!(!shown(cx, "turn-handle"));
    assert_eq!(hand(cx), Some(Hand::Shape));
    press(cx, &opened, &["escape"]);
    assert_eq!(hand(cx), None);

    // V, then a click inside the shape picks it up, with the Shapes bar.
    press(cx, &opened, &["v"]);
    assert_eq!(hand(cx), Some(Hand::Select));
    assert!(!shown(cx, "shapes-bar"));
    update(cx, &opened, |window, cx| window.click("canvas", cx));
    assert!(shown(cx, "turn-handle") && shown(cx, "shapes-bar"));

    // Outline's next colour recolours it.
    press(cx, &opened, &["t", "right", "enter"]);
    assert_eq!(first_shape(cx, &opened).outline.color, SHAPE_COLORS[8]);

    // A drag inside moves it.
    let before = first_shape(cx, &opened);
    drag_between(cx, &opened, (0., 0.), (30., 0.));
    let moved = first_shape(cx, &opened);
    assert!(moved.start.0 > before.start.0 + 10.);
    assert_eq!(moved.start.1, before.start.1);

    // Right-click: its menu turns it a quarter.
    update(cx, &opened, |window, cx| window.right_click("canvas", cx));
    assert!(shown(cx, "shape-context"));
    update(cx, &opened, |window, cx| {
        window.click("context-turn-right", cx)
    });
    assert!(!shown(cx, "shape-context"));
    assert_eq!(first_shape(cx, &opened).angle, 90.);
}

#[gpui_kit::test]
fn emoji_land_picked_up_in_the_middle_one_after_another(cx: &mut TestAppContext) {
    use shuttercrab::markup::{Emoji, ShapeKind};
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let shown = |cx: &mut TestAppContext, id: &'static str| {
        let mut shown = false;
        update(cx, &opened, |window, _| {
            shown = window.try_find(id).is_some()
        });
        shown
    };
    // G, then E opens the emoji; one right and Enter places the star.
    press(cx, &opened, &["g", "e"]);
    assert!(shown(cx, "emoji-menu"));
    press(cx, &opened, &["right", "enter"]);
    assert!(!shown(cx, "emoji-menu"));
    let star = first_shape(cx, &opened);
    assert_eq!(star.kind, ShapeKind::Emoji(Emoji::Star));
    let (x, y) = star.center();
    assert!(
        (x - 400.).abs() < 0.01 && (y - 300.).abs() < 0.01,
        "{x} {y}"
    );
    assert_eq!(star.end.0 - star.start.0, star.end.1 - star.start.1);
    // Picked up: it can turn, and has no outline to choose.
    assert!(shown(cx, "turn-handle"));
    press(cx, &opened, &["t"]);
    assert!(!shown(cx, "shape-menu"));

    // A second lands below and right of the first, unmoved; a click on
    // the picker places it too.
    press(cx, &opened, &["e"]);
    update(cx, &opened, |window, cx| window.click("emoji-0", cx));
    let drawn = marks(cx, &opened);
    let heart = drawn[1].as_shape().unwrap();
    assert_eq!(heart.kind, ShapeKind::Emoji(Emoji::RedHeart));
    assert!(heart.center().0 > star.center().0 && heart.center().1 > star.center().1);

    // The next shape is still the one chosen before.
    assert_eq!(
        opened.settings.borrow().shape_style().kind,
        ShapeKind::Rectangle
    );
    press(cx, &opened, &["delete"]);
    assert_eq!(marks(cx, &opened).len(), 1);
}

#[gpui_kit::test]
fn crop_frames_the_part_kept_applies_with_enter_and_undoes(cx: &mut TestAppContext) {
    use shuttercrab::markup::Region;
    let opened = open_sized(cx, shot_size());
    update(cx, &opened, |window, cx| {
        opened
            .view
            .update(cx, |view, cx| view.show_shot(shot(800, 600), window, cx));
    });
    let label = |cx: &mut TestAppContext, id: &'static str| {
        let mut label = None;
        update(cx, &opened, |window, _| {
            label = window
                .try_find(id)
                .and_then(|e| e.label().map(|l| l.to_string()))
        });
        label
    };
    let crop = |cx: &mut TestAppContext| {
        cx.update(|cx| opened.view.read(cx).marked_shot().and_then(|s| s.crop))
    };
    // C opens crop on the whole screenshot; the drawing tools step aside.
    press(cx, &opened, &["c"]);
    assert_eq!(label(cx, "crop-size").as_deref(), Some("800 × 600"));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("crop-bar").is_some());
        assert!(window.try_find("tool-pen").is_none());
    });

    // Tab to the top-left corner: arrows resize by 5. Then the bottom-right.
    press(cx, &opened, &["tab", "right", "right", "right", "right"]);
    assert_eq!(label(cx, "crop-size").as_deref(), Some("780 × 600"));
    press(cx, &opened, &["tab", "up", "up"]);
    assert_eq!(label(cx, "crop-size").as_deref(), Some("780 × 590"));
    // It stops at 48 pixels.
    for _ in 0..200 {
        press(cx, &opened, &["left"]);
    }
    assert_eq!(label(cx, "crop-size").as_deref(), Some("48 × 590"));
    for _ in 0..200 {
        press(cx, &opened, &["right"]);
    }

    // Enter keeps it, and the tools come back.
    press(cx, &opened, &["enter"]);
    let kept = Region {
        x: 20,
        y: 0,
        width: 780,
        height: 590,
    };
    assert_eq!(crop(cx), Some(kept));
    update(cx, &opened, |window, _| {
        assert!(window.try_find("crop-bar").is_none());
        assert!(window.try_find("tool-pen").is_some());
    });

    // Opening crop again starts from it; Escape leaves it as it was.
    press(cx, &opened, &["c"]);
    assert_eq!(label(cx, "crop-size").as_deref(), Some("780 × 590"));
    press(cx, &opened, &["left", "escape"]);
    assert_eq!(crop(cx), Some(kept));

    // Undo takes the crop back, redo keeps it again.
    press(cx, &opened, &["ctrl-z"]);
    assert_eq!(crop(cx), None);
    press(cx, &opened, &["ctrl-y"]);
    assert_eq!(crop(cx), Some(kept));
}
