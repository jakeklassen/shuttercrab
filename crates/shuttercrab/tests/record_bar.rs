//! Drives the recording controls in a headless GPUI window with native
//! pointer and keyboard events (PRD §16).
//!
//!   cargo test -p shuttercrab --test record_bar

use gpui_kit::{
    App, AppContext as _, TestAppContext, VisualTestContext, Window, WindowHandle, px, size,
    test::TestWindowExt as _,
};
use shuttercrab::{
    record_bar::{
        BarMode, Destructive, RECORD_BAR_HEIGHT, RECORD_BAR_WIDTH, RecordBar, RecordBarEvent,
        RecordKeys, Sound, default_microphone,
    },
    recording::Clock,
};
use shuttercrab_capture::record::{Microphone, Source};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

struct Opened {
    handle: WindowHandle<RecordBar>,
    clock: Rc<Cell<Clock>>,
    events: Rc<RefCell<Vec<RecordBarEvent>>>,
}

/// Controls for a recording `running` long.
fn open(cx: &mut TestAppContext, running: Duration) -> Opened {
    open_bar(cx, running, None)
}

/// The ready bar, recording `sound` unless switched.
fn open_ready(cx: &mut TestAppContext, sound: Sound) -> Opened {
    open_bar(cx, Duration::ZERO, Some((sound, microphones())))
}

fn microphones() -> Vec<Microphone> {
    [
        ("{yeti}", "Yeti Stereo Microphone"),
        ("{c920}", "C920 webcam"),
    ]
    .map(|(id, name)| Microphone {
        id: id.into(),
        name: name.into(),
    })
    .into()
}

fn open_bar(
    cx: &mut TestAppContext,
    running: Duration,
    ready: Option<(Sound, Vec<Microphone>)>,
) -> Opened {
    cx.update(gpui_kit::init);
    let clock = Rc::new(Cell::new(Clock::new(Instant::now() - running)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let (sink, shared) = (events.clone(), clock.clone());
    let handle = cx.open_window(
        size(px(RECORD_BAR_WIDTH), px(RECORD_BAR_HEIGHT)),
        move |window, cx| {
            cx.subscribe_self(move |_, event: &RecordBarEvent, _| sink.borrow_mut().push(*event))
                .detach();
            let keys = RecordKeys {
                pause: "Ctrl+Alt+P".into(),
                stop: "Ctrl+Alt+R".into(),
                restart: "Ctrl+Alt+N".into(),
                discard: "Ctrl+Alt+D".into(),
                undo: "Ctrl+Alt+Z".into(),
            };
            let bar = RecordBar::new(shared, keys, window, cx);
            match ready {
                Some((sound, microphones)) => bar.ready(sound, microphones),
                None => bar,
            }
        },
    );
    Opened {
        handle,
        clock,
        events,
    }
}

fn update(cx: &mut TestAppContext, opened: &Opened, f: impl FnOnce(&mut Window, &mut App)) {
    cx.update_window(opened.handle.into(), |_, window, cx| {
        window.render_frame(cx);
        f(window, cx);
    })
    .unwrap();
}

fn label(window: &Window, id: &'static str) -> Option<String> {
    window
        .try_find(id)
        .filter(|e| e.visible())
        .and_then(|e| e.label().map(|l| l.to_string()))
}

fn events(opened: &Opened) -> Vec<RecordBarEvent> {
    opened.events.borrow().clone()
}

/// Show `mode` on the bar.
fn set_mode(cx: &mut TestAppContext, opened: &Opened, mode: BarMode) {
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| bar.set_mode(mode, cx))
            .unwrap()
    });
}

fn mode(cx: &mut TestAppContext, opened: &Opened) -> BarMode {
    cx.update(|cx| opened.handle.read(cx).unwrap().mode())
}

/// Give the bar the keyboard, as a click does.
fn activate(cx: &mut TestAppContext, opened: &Opened) {
    update(cx, opened, |window, _| window.activate_window());
    cx.run_until_parked();
}

#[gpui_kit::test]
fn shows_the_time_and_all_four_actions(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(84));
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "1:24");
        for id in [
            "record-pause",
            "record-stop",
            "record-restart",
            "record-discard",
        ] {
            assert!(window.try_find(id).is_some(), "{id}");
        }
    });
}

#[gpui_kit::test]
fn a_paused_recording_offers_resume(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(5));
    let mut clock = opened.clock.get();
    clock.pause(Instant::now());
    opened.clock.set(clock);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "0:05");
        assert_eq!(label(window, "record-pause").unwrap(), "Resume");
    });
}

#[gpui_kit::test]
fn a_recording_paused_by_itself_says_why(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(12));
    let mut clock = opened.clock.get();
    clock.pause(Instant::now());
    opened.clock.set(clock);
    let why = |cx: &mut TestAppContext, why: Option<&'static str>| {
        cx.update(|cx| {
            opened
                .handle
                .update(cx, |bar, _, cx| bar.pause_reason(why.map(Into::into), cx))
                .unwrap()
        })
    };
    why(cx, Some("the window is minimised"));
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "0:12 · Paused: the window is minimised"
        );
        assert_eq!(label(window, "record-pause").unwrap(), "Resume");
    });
    why(cx, None);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "0:12");
    });
}

#[gpui_kit::test]
fn hints_show_the_chords_until_the_bar_has_the_keyboard(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    // As it appears: the app being recorded keeps the keyboard.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        for (id, chord) in [
            ("record-pause-key", "Ctrl+Alt+P"),
            ("record-stop-key", "Ctrl+Alt+R"),
            ("record-restart-key", "Ctrl+Alt+N"),
            ("record-discard-key", "Ctrl+Alt+D"),
        ] {
            assert_eq!(label(window, id).unwrap(), chord, "{id}");
        }
    });
    // Clicked: the letters work, and say so.
    activate(cx, &opened);
    update(cx, &opened, |window, _| {
        for (id, letter) in [
            ("record-pause-key", "P"),
            ("record-stop-key", "S"),
            ("record-restart-key", "N"),
            ("record-discard-key", "D"),
        ] {
            assert_eq!(label(window, id).unwrap(), letter, "{id}");
        }
    });
}

#[gpui_kit::test]
fn the_buttons_stay_put_when_the_bar_gains_the_keyboard(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    let ids = [
        "record-microphone",
        "record-system-sound",
        "record-pause",
        "record-stop",
        "record-restart",
        "record-discard",
    ];
    let bounds = |cx: &mut TestAppContext| {
        let mut found = Vec::new();
        update(cx, &opened, |window, _| {
            found = ids.map(|id| window.find(id).bounds()).to_vec();
        });
        found
    };
    // The click that gives the bar the keyboard must be released on the
    // button it pressed.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    let before = bounds(cx);
    activate(cx, &opened);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-stop-key").unwrap(), "S");
    });
    assert_eq!(bounds(cx), before);
}

#[gpui_kit::test]
fn letters_ask_for_each_action(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        for key in ["p", "space", "s", "n", "d", "z", "enter", "escape"] {
            window.press(key, cx);
        }
    });
    // Z, Enter and Escape mean nothing until there is something to undo
    // or a question.
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::TogglePause,
            RecordBarEvent::Stop,
            RecordBarEvent::Restart,
            RecordBarEvent::Discard,
        ]
    );
}

#[gpui_kit::test]
fn clicking_the_buttons(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    for id in [
        "record-pause",
        "record-stop",
        "record-restart",
        "record-discard",
    ] {
        update(cx, &opened, |window, cx| window.click(id, cx));
    }
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::TogglePause,
            RecordBarEvent::Stop,
            RecordBarEvent::Restart,
            RecordBarEvent::Discard,
        ]
    );
}

#[gpui_kit::test]
fn asking_before_a_discard(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(134));
    set_mode(
        cx,
        &opened,
        BarMode::Confirm(Destructive::Discard, Duration::from_secs(134)),
    );
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Discard this 2:14 recording?"
        );
        assert_eq!(label(window, "record-keep-key").unwrap(), "Esc");
        assert_eq!(label(window, "record-confirm-key").unwrap(), "Enter");
        assert!(window.try_find("record-pause").is_none());
        // The other actions' letters do nothing while it asks.
        window.press("p", cx);
        window.press("n", cx);
        window.press("escape", cx);
        window.press("enter", cx);
        window.press("d", cx);
    });
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::Cancel,
            RecordBarEvent::Confirm,
            RecordBarEvent::Confirm
        ]
    );
    // Without the keyboard, the hints are the chords that answer.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-confirm-key").unwrap(), "Ctrl+Alt+D");
        assert_eq!(label(window, "record-keep-key").unwrap(), "Ctrl+Alt+P");
    });
}

#[gpui_kit::test]
fn asking_before_a_restart(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(61));
    set_mode(
        cx,
        &opened,
        BarMode::Confirm(Destructive::Restart, Duration::from_secs(61)),
    );
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Restart? The 1:01 take is thrown away."
        );
        window.press("d", cx);
        window.press("n", cx);
    });
    assert_eq!(events(&opened), [RecordBarEvent::Confirm]);
}

#[gpui_kit::test]
fn a_discard_counts_down_and_offers_undo(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::from_secs(30));
    let until = Instant::now() + Duration::from_millis(9_500);
    set_mode(cx, &opened, BarMode::Discarded { until });
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-time").unwrap(),
            "Discarded · deleted in 10 s"
        );
        assert_eq!(label(window, "record-undo-key").unwrap(), "Z");
        assert_eq!(label(window, "record-discard-now-key").unwrap(), "Enter");
        assert!(window.try_find("record-discard").is_none());
        window.press("p", cx);
        window.press("z", cx);
        window.press("enter", cx);
        window.press("d", cx);
    });
    // Z undoes; Enter, or D again, discards at once.
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::Undo,
            RecordBarEvent::Confirm,
            RecordBarEvent::Confirm
        ]
    );
    assert_eq!(mode(cx, &opened), BarMode::Discarded { until });
    // Without the keyboard: the undo chord, and the discard chord again.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-undo-key").unwrap(), "Ctrl+Alt+Z");
        assert_eq!(
            label(window, "record-discard-now-key").unwrap(),
            "Ctrl+Alt+D"
        );
    });
}

#[gpui_kit::test]
fn after_a_restart_the_previous_take_can_be_kept(cx: &mut TestAppContext) {
    let opened = open(cx, Duration::ZERO);
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| {
                bar.offer_previous(Some(Instant::now() + Duration::from_secs(10)), cx)
            })
            .unwrap()
    });
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-keep-previous").unwrap(),
            "Keep previous take"
        );
        // It takes Restart's place.
        assert!(window.try_find("record-restart").is_none());
        window.press("n", cx);
        window.press("z", cx);
    });
    assert_eq!(events(&opened), [RecordBarEvent::Undo]);
    // Once withdrawn, Restart is back.
    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| bar.offer_previous(None, cx))
            .unwrap()
    });
    update(cx, &opened, |window, _| {
        assert!(window.try_find("record-restart").is_some());
        assert!(window.try_find("record-keep-previous").is_none());
    });
}

fn sound(cx: &mut TestAppContext, opened: &Opened) -> Sound {
    cx.update(|cx| opened.handle.read(cx).unwrap().sound().clone())
}

/// The sound the settings record: the microphone only, the Yeti.
fn from_settings() -> Sound {
    Sound {
        system: false,
        app_only: true,
        microphone: true,
        device: Some("{yeti}".into()),
    }
}

#[gpui_kit::test]
fn the_ready_bar_offers_start_and_the_sound(cx: &mut TestAppContext) {
    let opened = open_ready(cx, from_settings());
    activate(cx, &opened);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "Ready to record");
        assert_eq!(label(window, "record-microphone").unwrap(), "Microphone on");
        assert_eq!(
            label(window, "record-microphones").unwrap(),
            "Which microphone: Yeti Stereo Microphone"
        );
        assert_eq!(
            label(window, "record-system-sound").unwrap(),
            "System sound off"
        );
        for (id, key) in [
            ("record-microphone-key", "M"),
            ("record-system-sound-key", "A"),
            ("record-start-key", "Enter"),
            ("record-close-key", "Esc"),
        ] {
            assert_eq!(label(window, id).unwrap(), key, "{id}");
        }
        // Nothing is recording yet.
        assert!(window.try_find("record-pause").is_none());
        assert!(window.try_find("record-stop").is_none());
    });
}

#[gpui_kit::test]
fn the_ready_bar_switches_the_sound_by_key(cx: &mut TestAppContext) {
    let opened = open_ready(cx, from_settings());
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        for key in ["m", "a", "a", "down", "p", "s"] {
            window.press(key, cx);
        }
    });
    // P and S mean nothing before recording.
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::SetSound(Source::Microphone, false),
            RecordBarEvent::SetSound(Source::System, true),
            RecordBarEvent::SetSound(Source::System, false),
            RecordBarEvent::ChooseMicrophone,
        ]
    );
    assert_eq!(
        sound(cx, &opened),
        Sound {
            microphone: false,
            ..from_settings()
        }
    );
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "record-microphone").unwrap(),
            "Microphone off"
        );
    });
}

#[gpui_kit::test]
fn enter_starts_and_the_switches_stay(cx: &mut TestAppContext) {
    let opened = open_ready(cx, from_settings());
    activate(cx, &opened);
    update(cx, &opened, |window, cx| {
        window.press("enter", cx);
        // Starting: Start and Cancel are gone; the switches stay.
        window.press("enter", cx);
        window.press("escape", cx);
        window.press("a", cx);
    });
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::Start,
            RecordBarEvent::SetSound(Source::System, true),
        ]
    );
    assert_eq!(mode(cx, &opened), BarMode::Starting);
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-time").unwrap(), "Starting…");
        assert!(window.try_find("record-start").is_none());
        assert!(window.try_find("record-microphones").is_none());
        assert_eq!(
            label(window, "record-system-sound").unwrap(),
            "System sound on"
        );
    });
    // Recording: the switches sit before Pause, and still work.
    set_mode(cx, &opened, BarMode::Controls);
    update(cx, &opened, |window, cx| {
        assert!(window.try_find("record-pause").is_some());
        window.press("m", cx);
    });
    assert_eq!(
        events(&opened)[2..],
        [RecordBarEvent::SetSound(Source::Microphone, false)]
    );
}

#[gpui_kit::test]
fn escape_closes_the_ready_bar(cx: &mut TestAppContext) {
    let opened = open_ready(cx, Sound::default());
    activate(cx, &opened);
    update(cx, &opened, |window, cx| window.press("escape", cx));
    assert_eq!(events(&opened), [RecordBarEvent::Close]);
}

#[gpui_kit::test]
fn clicking_the_ready_bar(cx: &mut TestAppContext) {
    let opened = open_ready(cx, Sound::default());
    // Without the keyboard (the microphone list has it), the hints stay,
    // so the buttons do not move.
    VisualTestContext::from_window(opened.handle.into(), cx).deactivate_window();
    update(cx, &opened, |window, _| {
        assert_eq!(label(window, "record-start-key").unwrap(), "Enter");
        assert_eq!(label(window, "record-microphone-key").unwrap(), "M");
    });
    for id in [
        "record-microphone",
        "record-system-sound",
        "record-microphones",
        "record-start",
    ] {
        update(cx, &opened, |window, cx| window.click(id, cx));
    }
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::SetSound(Source::Microphone, true),
            RecordBarEvent::SetSound(Source::System, true),
            RecordBarEvent::ChooseMicrophone,
            RecordBarEvent::Start,
        ]
    );
}

#[gpui_kit::test]
fn choosing_a_microphone(cx: &mut TestAppContext) {
    let opened = open_ready(cx, from_settings());
    let choices = |cx: &mut TestAppContext| {
        cx.update(|cx| opened.handle.read(cx).unwrap().microphone_choices())
    };
    let (names, chosen) = choices(cx);
    assert_eq!(
        names,
        [
            default_microphone(),
            "Yeti Stereo Microphone",
            "C920 webcam"
        ]
    );
    assert_eq!(chosen, 1);
    let choose = |cx: &mut TestAppContext, index| {
        cx.update(|cx| {
            opened
                .handle
                .update(cx, |bar, _, cx| bar.choose_microphone(index, cx))
                .unwrap()
        })
    };
    choose(cx, 2);
    assert_eq!(sound(cx, &opened).device.as_deref(), Some("{c920}"));
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "record-microphones").unwrap(),
            "Which microphone: C920 webcam"
        );
    });
    choose(cx, 0);
    assert_eq!(sound(cx, &opened).device, None);
    assert_eq!(choices(cx).1, 0);
}

#[gpui_kit::test]
fn a_microphone_not_plugged_in_gives_way_to_the_default(cx: &mut TestAppContext) {
    let opened = open_ready(
        cx,
        Sound {
            device: Some("{gone}".into()),
            ..from_settings()
        },
    );
    assert_eq!(sound(cx, &opened).device, None);
    update(cx, &opened, |window, _| {
        assert_eq!(
            label(window, "record-microphones").unwrap(),
            format!("Which microphone: {}", default_microphone())
        );
    });
}

#[gpui_kit::test]
fn closing_the_window_to_record_closes_the_ready_bar(cx: &mut TestAppContext) {
    let opened = open_ready(cx, Sound::default());
    let close = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            opened
                .handle
                .update(cx, |bar, _, cx| bar.close(cx))
                .unwrap()
        })
    };
    close(cx);
    assert_eq!(events(&opened), [RecordBarEvent::Close]);
    // Once recording, the bar stays: the recording ends by itself.
    set_mode(cx, &opened, BarMode::Controls);
    close(cx);
    assert_eq!(events(&opened), [RecordBarEvent::Close]);
}

#[gpui_kit::test]
fn a_window_recording_offers_its_app_sound_alone(cx: &mut TestAppContext) {
    let opened = open_ready(cx, from_settings());
    activate(cx, &opened);
    // Not for an area: nothing to choose, and B does nothing.
    update(cx, &opened, |window, cx| {
        assert!(window.try_find("record-app-sound").is_none());
        window.press("b", cx);
    });
    assert!(events(&opened).is_empty());

    cx.update(|cx| {
        opened
            .handle
            .update(cx, |bar, _, cx| bar.offer_app_sound(cx))
            .unwrap()
    });
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-app-sound").unwrap(),
            "System sound: this app only"
        );
        assert_eq!(label(window, "record-app-sound-key").unwrap(), "B");
        window.press("b", cx);
    });
    update(cx, &opened, |window, cx| {
        assert_eq!(
            label(window, "record-app-sound").unwrap(),
            "System sound: all of it"
        );
        window.click("record-app-sound", cx);
    });
    assert_eq!(
        events(&opened),
        [
            RecordBarEvent::SetAppOnly(false),
            RecordBarEvent::SetAppOnly(true),
        ]
    );
    assert!(sound(cx, &opened).app_only);
    // Everything fits, the time readable.
    let fits = |window: &mut Window, last: &'static str| {
        let right = window.find(last).bounds();
        assert!(f32::from(right.origin.x + right.size.width) <= RECORD_BAR_WIDTH);
        assert!(window.find("record-time").bounds().size.width >= gpui_kit::px(60.));
    };
    update(cx, &opened, |window, _| fits(window, "record-close"));
    // While recording, it stays beside the switch, and B still works.
    set_mode(cx, &opened, BarMode::Controls);
    update(cx, &opened, |window, cx| {
        assert!(window.try_find("record-app-sound").is_some());
        fits(window, "record-discard");
        window.press("b", cx);
    });
    assert_eq!(events(&opened)[2..], [RecordBarEvent::SetAppOnly(false)]);
}
