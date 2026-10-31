# Testing

How Shuttercrab is tested: what runs on its own, what runs by hand, what CI
runs, and what still needs a person. The old test matrix, as of
2026-10-17, with its measurements and the owner's acceptance tests, is in
[history/TEST_MATRIX.md](history/TEST_MATRIX.md).

## Run everything

From the repository root:

```powershell
mise run check
```

It runs, in order (`mise.toml`):

1. `mise run trim`, which empties `target/` once it has grown past 30 GiB.
   Cargo never removes old build outputs, and each version bump leaves a
   whole set of them behind.
2. `cargo fmt --all --check`.
3. `cargo clippy --all-targets --locked -- -D warnings`.
4. `cargo test --locked -- --skip a_second_instance_signals_the_first`.
   That test signals as a second start of Shuttercrab does, so on your PC
   it would reach the copy you have running. CI runs it.

Some tests are ignored by default (see [below](#ignored-tests)); `mise run
check` does not run them.

## Where the tests live

| Where | What | Runs on |
|---|---|---|
| `#[cfg(test)]` modules beside the code, in every crate | Unit tests: logical to physical pixels, settings, file names, the tray menu, markup, crop, text actions, the color transform's CPU reference, cutting and PNG, recording sizes and exposure, hotkey parsing, the border's strips, links, the stream's codec choice and quality, and more. An OS backend's tests sit in its own `sys/windows` or `sys/linux` module | Every OS; a backend's tests on its own OS |
| [`crates/shuttercrab/tests/ui`](../crates/shuttercrab/tests/ui) | UI tests: the real selection overlay, Capture Bar, main window, settings, thumbnail, recording controls, countdown and choice list, each in a headless GPUI window. They open the window with GPUI Kit's `TestAppContext` (the `test-support` feature), send it native pointer and keyboard events, and find its elements by id. The main window and settings get fake hooks in place of the rest of the app. They are one test binary, since each binary links all of GPUI | Every OS |
| [`crates/shuttercrab-capture/tests/gpu_transform.rs`](../crates/shuttercrab-capture/tests/gpu_transform.rs) | Golden tests for the GPU transform: known scRGB in, known sRGB codes out, and agreement with the CPU reference. They run on WARP, Direct3D's software adapter, so they give the same result on any machine, and on the hardware adapter when there is one | Windows |
| [`crates/shuttercrab-capture/tests/service.rs`](../crates/shuttercrab-capture/tests/service.rs) | The capture service against the real desktop: listing monitors | Windows |
| [`crates/shuttercrab-platform/tests/platform.rs`](../crates/shuttercrab-platform/tests/platform.rs) | The platform thread against real Windows: registering hotkeys and reporting a conflict | Windows |
| [`crates/shuttercrab-platform/tests/instance.rs`](../crates/shuttercrab-platform/tests/instance.rs) | A second start reaches the running one with its request. In a test binary of its own, so no other test's platform window receives the signal | Windows |
| [`crates/shuttercrab-platform/tests/x11.rs`](../crates/shuttercrab-platform/tests/x11.rs), [`crates/shuttercrab-capture/tests/x11_record.rs`](../crates/shuttercrab-capture/tests/x11_record.rs) | The Linux backend against a real X server: hotkeys, the clipboard, the windows on screen, and a recording read back by GStreamer | Linux, through `scripts/linux-x11-tests.sh` |
| [`crates/shuttercrab-capture/tests/live.rs`](../crates/shuttercrab-capture/tests/live.rs) | A live recording played as it is recorded | Windows, by hand |
| [`crates/shuttercrab-connect/tests/online.rs`](../crates/shuttercrab-connect/tests/online.rs) | Both ends of a connection in one process, through the real meeting point | By hand |

To run one crate's tests, one test binary or one module of it:

```powershell
mise exec -- cargo test -p shuttercrab-capture
mise exec -- cargo test -p shuttercrab --test ui
mise exec -- cargo test -p shuttercrab --test ui record_bar::
```

### The X11 tests

The Linux tests that need an X server send keys and open windows, so they
run only on a private, invisible one, never on the desktop in use. The
script starts Xvfb with openbox as its window manager, sets
`SHUTTERCRAB_TEST_X11`, and runs the ignored tests in both files:

```sh
sudo apt install xvfb openbox gstreamer1.0-plugins-good gstreamer1.0-plugins-ugly
scripts/linux-x11-tests.sh
```

The report is also kept in `tmp/linux-x11-tests.log`.

## Ignored tests

These are `#[ignore]`d because they change something on your PC, need
something a test run cannot assume, or measure in real time. Run them on
purpose, with `-- --ignored`:

| Test | Why it is ignored | Run it with |
|---|---|---|
| `shuttercrab-capture`, `service`: `freezes_and_cuts_a_screenshot` | Captures the screen (in memory; nothing is saved) | `mise exec -- cargo test --release -p shuttercrab-capture --test service -- --ignored --nocapture` |
| `shuttercrab-platform`, `platform`: `puts_png_and_bitmap_on_the_clipboard` | Replaces the clipboard | `mise exec -- cargo test -p shuttercrab-platform --test platform -- --ignored` |
| `shuttercrab-capture`, `live`: three tests | Record the screen and play it. `live_pictures_at_each_quality_can_be_compared` saves pictures for a person to judge, into the folder `LIVE_COMPARE` names, and does nothing without it | `mise exec -- cargo test -p shuttercrab-capture --test live -- --ignored --nocapture` |
| `shuttercrab-capture`, the Windows encoder's unit tests: `live_frames_come_out_soon_after_they_go_in` and five `the_live_player_shows_pictures_soon_after_they_go_in_with_…` | Measure the live path's delay in real time | `mise exec -- cargo test -p shuttercrab-capture --lib soon_after -- --ignored --nocapture` |
| `shuttercrab-connect`, `online`: `two_apps_meet_and_connect_directly` | Meets through the real meeting point. Needs the internet and a host key, from `SHUTTERCRAB_HOST_KEY` or `meet/.env`; the key is never printed | `mise exec -- cargo test -p shuttercrab-connect --test online -- --ignored --nocapture` |
| `shuttercrab-platform`, `x11`: five tests; `shuttercrab-capture`, `x11_record`: one | Need a private X server | `scripts/linux-x11-tests.sh` |

## What CI runs

`.github/workflows/ci.yml` runs on pull requests, on demand, and from the
Release workflow on every push to `main`, which tags nothing unless CI
passes.

| Job | Runner | Runs |
|---|---|---|
| Test | `windows-latest` | `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`: every test that is not ignored, `a_second_instance_signals_the_first` and the GPU golden tests (on WARP) among them |
| Build on ubuntu-latest | `ubuntu-latest` | Clippy and `cargo test --locked`, then `scripts/linux-x11-tests.sh` on Xvfb |
| Build on macos-latest | `macos-latest` | Clippy and `cargo test --locked`. macOS has no backend, so this keeps the code building there |
| Linux build to try | `ubuntu-24.04`, pull requests only | A release build of the app, packed as `shuttercrab-linux-x86_64.tar.gz` and kept for 14 days, to try on a desktop. Not a release |

No ignored test runs in CI except the X11 ones. CI has no HDR display and
no second PC, and runs the GPU tests on WARP.

## What still needs a person

Tests cannot see a real HDR display, press keys in other apps, or reach a
second PC. Check these by hand when a change touches them.

| Area | What to check | Tools |
|---|---|---|
| HDR color | Screenshots with HDR on look the same as with HDR off, at different SDR content brightness settings; HDR content (video, games) is tone mapped, not washed out or clipped. After a change to the transform or to capture, run the Milestone 0 gate | `capture-spike gate`; the procedure is in [history/TEST_MATRIX.md](history/TEST_MATRIX.md#milestone-0-the-hdr-off--hdr-on-gate) |
| Several monitors | Every monitor freezes, and the one under the pointer takes the keyboard; a selection stops at its monitor's edges; screenshots are exact at 100%, 125%, 150% and 175% scaling, and after a scale change while running; recordings stay evenly timed at 120 and 144 Hz; a recording whose display is unplugged stops and says so | `capture-spike hdr`, `scale` and `refresh` set a display's state |
| The tray | The icon and its menu; a click opens the window; the icon comes back after Explorer restarts; notifications show, and a click on one is handled | |
| Hotkeys | Each default hotkey works in other apps, full-screen ones too; a hotkey another app owns brings the "is in use" notice | |
| Clipboard and drag | A screenshot pastes into browsers and chat apps, and the thumbnail drags into them and into Explorer | |
| Recording | The speakers' sound and the microphone are in the file; a recorded window is followed as it moves; the recording controls and the border are not in the video, since keeping them out is the OS's work | `capture-spike record` |
| Streaming between two PCs | On two PCs, one behind a VPN: Go live, the link, joining, the delay shown, sound in step with the pictures, the quality switch, Stop, and the viewer saying the stream ended. On one PC, mute the viewer or the sound plays twice | [STREAMING.md](STREAMING.md); `capture-spike stream-send` and `stream-watch --sound off` |
| Installer and updates | `Shuttercrab-win-Setup.exe` installs for the user with no administrator prompt (it is unsigned, so SmartScreen asks first), adds a Start menu entry and starts; `shuttercrab://` links open the viewer; an update is offered as "Restart to update" in the tray, not while recording, and applies; uninstalling removes the startup entry and the link scheme | `vpk pack` a folder of packages and set `SHUTTERCRAB_UPDATE_SOURCE` to it, as in the [README](../README.md#releases) |
| A lost graphics device | After sleep and wake, or a graphics driver update, the next screenshot and playback recover. The retry is unit tested with made-up errors only | |
| Linux | The pull request's Linux build on a desktop (Ubuntu 24.04 first), under X11: the tray (GNOME needs its AppIndicator extension), hotkeys, screenshots, recording | |
