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
            pause_hotkeys: Rc::new(|| {}),
            apply_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
            probe_hotkeys: Rc::new(|| Box::pin(async { Vec::new() })),
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
            .update(cx, |view, cx| view.show_shot(shot(2400, 1600), window, cx));
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
            expected(2400. / scale + 34.),
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

fn marks(cx: &mut TestAppContext, opened: &Opened) -> Vec<shuttercrab::markup::Stroke> {
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
    assert_eq!(drawn[0].tool, Tool::Pen);
    assert_eq!(drawn[0].color, Brush::PEN.color);
    assert!(drawn[0].points.len() >= 2);

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
    assert_eq!(drawn[1].tool, Tool::Highlighter);
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
}
