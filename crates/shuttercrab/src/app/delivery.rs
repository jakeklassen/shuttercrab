//! A finished screenshot: copied and saved as the settings say, then shown
//! in the main window if it was taken from there, or as a thumbnail that
//! opens it in the window or drags it out.

use super::{State, failure::Failure, screenshot::exclude_from_capture, windows::show_in_main};
use crate::{
    files,
    main_window::Shot,
    pixels,
    popup::{self, Activation},
    thumbnail::{self, Thumbnail, ThumbnailEvent},
};
use chrono::NaiveDateTime;
use futures::StreamExt as _;
use gpui_kit::{AppContext as _, AsyncApp, Task};
use shuttercrab_capture::{MonitorInfo, PhysicalRect, Screenshot};
use shuttercrab_platform::{drag::DragImage, window as platform_window};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::Instant,
};

/// The least time, seconds, a thumbnail stays when it is the screenshot's
/// only copy (neither copied nor saved).
const RESCUE_SECONDS: u32 = 30;

/// The thumbnail's distance from the work area's edges, logical pixels.
const THUMBNAIL_MARGIN: f32 = 16.0;

/// Copy and save a finished screenshot as the settings say, then show the
/// thumbnail and the notification. `released` is when the user finished
/// choosing, for the log.
pub(super) async fn deliver(
    state: &Rc<State>,
    shot: Screenshot,
    monitor: &MonitorInfo,
    released: Instant,
    taken_at: NaiveDateTime,
    cx: &mut AsyncApp,
) -> Result<(), Failure> {
    let settings = state.settings.borrow().clone();
    let (width, height) = (shot.width, shot.height);
    // Taken from the main window, which comes back showing it, as Snipping
    // Tool does; otherwise the thumbnail shows it.
    let in_window = state.hidden_main.get().is_some();

    // Start the file write and the thumbnail image first; they run while
    // the clipboard is written. All three share the screenshot's buffers.
    let saved = settings.auto_save.then(|| {
        let (dir, png) = (settings.output_dir(), shot.png.clone());
        cx.background_executor()
            .spawn(async move { files::save_screenshot(&dir, taken_at, &png) })
    });
    let scaled = (settings.show_thumbnail && !in_window)
        .then(|| scale_for_thumbnail(shot.rgba.clone(), (width, height), monitor, cx));
    // Kept for the window or the thumbnail: without auto-save, the thumbnail
    // writes a temporary file only if it is dragged.
    let png = shot.png.clone();
    let rgba = shot.rgba.clone();
    let copied = if settings.copy_to_clipboard {
        let result = state
            .platform
            .copy_image(shot.png, shot.rgba, width, height)
            .await;
        if result.is_ok() {
            log::info!(
                "{width}×{height} screenshot on the clipboard {} ms after release",
                released.elapsed().as_millis()
            );
        }
        Some(result)
    } else {
        None
    };
    let saved = match saved {
        Some(task) => Some(task.await),
        None => None,
    };
    if let Some(Ok(path)) = &saved {
        log::info!(
            "saved {} {} ms after release",
            path.file_name().unwrap_or_default().to_string_lossy(),
            released.elapsed().as_millis()
        );
    }

    let saved_path = saved.as_ref().and_then(|r| r.as_ref().ok().cloned());
    // A failed copy shows the thumbnail even when thumbnails are off, so the
    // screenshot can still be opened or dragged out. Without a saved file,
    // the thumbnail is then its only copy.
    let copy_failed = matches!(copied, Some(Err(_)));
    let rescued = copy_failed && saved_path.is_none();
    let thumbnail = !in_window && (settings.show_thumbnail || copy_failed);
    let result = outcome(copied, saved, &settings.output_dir(), in_window);

    if in_window {
        let image = cx
            .background_executor()
            .spawn(async move { pixels::render_image(&rgba, width, height) })
            .await;
        show_in_main(
            state,
            Shot {
                image,
                png,
                taken_at,
                saved: saved_path.clone(),
            },
            cx,
        );
    } else if thumbnail {
        let picture = match scaled {
            Some(task) => task.await,
            None => scale_for_thumbnail(rgba, (width, height), monitor, cx).await,
        };
        let file = match saved_path.clone() {
            Some(path) => CaptureFile::Saved(path),
            None => CaptureFile::Unsaved { png, taken_at },
        };
        // When the thumbnail is the only copy, leave time to read the
        // message and use it.
        let seconds = if rescued {
            settings.thumbnail_seconds.max(RESCUE_SECONDS)
        } else {
            settings.thumbnail_seconds
        };
        let pending = PendingThumbnail {
            picture,
            size: (width, height),
            file,
            taken_at,
            monitor: monitor.clone(),
            seconds,
        };
        show_thumbnail(state.clone(), pending, cx);
    }
    result?;

    if settings.notify_after_capture {
        let message = match &saved_path {
            Some(path) => format!(
                "Saved as {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            None => "On the clipboard".to_string(),
        };
        state.notify(
            format!("Screenshot {width} × {height}"),
            message,
            saved_path,
        );
    }
    Ok(())
}

/// The screenshot scaled down to the thumbnail's size on `monitor`, on a
/// background thread.
fn scale_for_thumbnail(
    rgba: Arc<Vec<u8>>,
    (width, height): (u32, u32),
    monitor: &MonitorInfo,
    cx: &AsyncApp,
) -> Task<thumbnail::Picture> {
    let (w, h) = thumbnail::image_size(width, height);
    let scale = monitor.scale_factor;
    let (max_w, max_h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    cx.background_executor()
        .spawn(async move { thumbnail::scale_down(&rgba, width, height, max_w, max_h) })
}

/// What to tell the user about copying (`copied`) and saving (`saved`) to
/// `folder`; `None` is a step the settings turned off. `in_window`: the
/// main window shows the screenshot, rather than the thumbnail. PRD §24:
/// say what happened and what to do; the causes go to the log.
fn outcome(
    copied: Option<anyhow::Result<()>>,
    saved: Option<anyhow::Result<PathBuf>>,
    folder: &Path,
    in_window: bool,
) -> Result<(), Failure> {
    let busy = "Another app is holding the clipboard.";
    let in_thumbnail = if in_window {
        "It's in Shuttercrab's window: Ctrl+C copies it, Ctrl+S saves it."
    } else {
        "It's in the thumbnail: click to open it, or drag it out."
    };
    let unwritable = format!(
        "Could not save to {}. Check the folder in Settings.",
        folder.display()
    );
    match (copied, saved) {
        (Some(Err(copy)), Some(Err(save))) => Err(Failure::new(
            format!("Could not copy or save the screenshot. {unwritable} {in_thumbnail}"),
            format!("copy: {copy:#}; save: {save:#}"),
        )),
        (Some(Err(copy)), Some(Ok(_))) => Err(Failure::new(
            format!("The screenshot was saved, but not copied. {busy} {in_thumbnail}"),
            format!("copy: {copy:#}"),
        )),
        (Some(Err(copy)), None) => Err(Failure::new(
            format!("The screenshot was not copied. {busy} {in_thumbnail}"),
            format!("copy: {copy:#}"),
        )),
        (Some(Ok(())), Some(Err(save))) => Err(Failure::new(
            format!("The screenshot was copied, but not saved. {unwritable}"),
            format!("save: {save:#}"),
        )),
        (None, Some(Err(save))) => Err(Failure::new(
            format!("The screenshot was not saved. {unwritable}"),
            format!("save: {save:#}"),
        )),
        (None, None) => {
            log::warn!("screenshot discarded: copying and saving are both off");
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Show the screenshot saved at `path` in the main window, as clicking its
/// notification does in Snipping Tool.
pub(super) async fn show_file_in_main(state: &Rc<State>, path: PathBuf, cx: &mut AsyncApp) {
    // Only Save as uses the time, to suggest a name; the file's is close.
    let taken_at = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).naive_local())
        .unwrap_or_else(|_| chrono::Local::now().naive_local());
    match CaptureFile::Saved(path).shot(taken_at, cx).await {
        Ok(shot) => show_in_main(state, shot, cx),
        Err(e) => log::error!("{e}"),
    }
}

/// The file behind a thumbnail: saved already, or written to the temporary
/// folder the first time it is dragged.
enum CaptureFile {
    Saved(PathBuf),
    Unsaved {
        png: Arc<Vec<u8>>,
        taken_at: NaiveDateTime,
    },
}

impl CaptureFile {
    /// The screenshot, ready for the main window to show: its PNG, read
    /// back from the file if it was saved, and decoded.
    async fn shot(&self, taken_at: NaiveDateTime, cx: &mut AsyncApp) -> Result<Shot, String> {
        let saved = match self {
            CaptureFile::Saved(path) => Some(path.clone()),
            CaptureFile::Unsaved { .. } => None,
        };
        let png = match self {
            CaptureFile::Saved(path) => {
                let path = path.clone();
                let read = cx
                    .background_executor()
                    .spawn(async move { std::fs::read(&path) })
                    .await;
                Arc::new(read.map_err(|e| format!("Could not read the screenshot: {e}"))?)
            }
            CaptureFile::Unsaved { png, .. } => png.clone(),
        };
        let decoding = png.clone();
        let image = cx
            .background_executor()
            .spawn(async move {
                let (rgba, width, height) = pixels::decode_png(&decoding)?;
                anyhow::Ok(pixels::render_image(&rgba, width, height))
            })
            .await
            .map_err(|e| format!("Could not read the screenshot: {e:#}"))?;
        Ok(Shot {
            image,
            png,
            taken_at,
            saved,
        })
    }

    async fn path(&mut self, cx: &mut AsyncApp) -> Result<PathBuf, String> {
        match self {
            CaptureFile::Saved(path) => Ok(path.clone()),
            CaptureFile::Unsaved { png, taken_at } => {
                let (png, taken_at) = (png.clone(), *taken_at);
                let path = cx
                    .background_executor()
                    .spawn(
                        async move { files::save_screenshot(&files::temp_dir(), taken_at, &png) },
                    )
                    .await
                    .map_err(|e| format!("Could not write the screenshot: {e:#}"))?;
                *self = CaptureFile::Saved(path.clone());
                Ok(path)
            }
        }
    }
}

/// A thumbnail about to be shown.
struct PendingThumbnail {
    /// The screenshot scaled down: the card's picture and the drag image.
    picture: thumbnail::Picture,
    /// The screenshot's size, physical pixels.
    size: (u32, u32),
    file: CaptureFile,
    taken_at: NaiveDateTime,
    monitor: MonitorInfo,
    seconds: u32,
}

/// Where the thumbnail card goes: the bottom-right corner of the monitor's
/// work area (above the taskbar), physical pixels.
pub fn thumbnail_rect(work: PhysicalRect, scale: f32, size: (u32, u32)) -> PhysicalRect {
    let (w, h) = thumbnail::image_size(size.0, size.1);
    let card = |side: f32| ((side + 2.0 * thumbnail::PADDING) * scale).round() as u32;
    let (width, height) = (card(w), card(h));
    let margin = (THUMBNAIL_MARGIN * scale).round() as i32;
    PhysicalRect::new(
        work.x + work.width as i32 - width as i32 - margin,
        work.y + work.height as i32 - height as i32 - margin,
        width,
        height,
    )
}

/// Show the thumbnail, replacing any previous one, and handle it until it
/// closes. Runs on its own: the next capture does not wait for it.
fn show_thumbnail(state: Rc<State>, pending: PendingThumbnail, cx: &mut AsyncApp) {
    cx.spawn(async move |cx| {
        let PendingThumbnail {
            picture,
            size,
            mut file,
            taken_at,
            monitor,
            seconds,
        } = pending;
        let work = platform_window::work_area(monitor.id.0)
            .map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h))
            .unwrap_or(monitor.bounds);
        let rect = thumbnail_rect(work, monitor.scale_factor, size);
        let image = thumbnail::render_image(&picture);
        let drag_picture = thumbnail::soften(&picture, thumbnail::DRAG_LOOK);
        let opened = popup::open(&monitor, rect, Activation::Never, cx, move |_, cx| {
            cx.new(|cx| Thumbnail::new(image, seconds, cx))
        });
        let (card, mut events) = match opened {
            Ok(opened) => opened,
            Err(e) => {
                log::warn!("could not show the thumbnail: {e}");
                return;
            }
        };
        if let Some(hwnd) = card.hwnd() {
            platform_window::round_corners(hwnd);
            // Drags start here; it is never where they end.
            if let Err(e) = shuttercrab_platform::drag::refuse_drops(hwnd) {
                log::warn!("the thumbnail still accepts drops: {e:#}");
            }
            exclude_from_capture(hwnd, "thumbnail");
        }
        log::debug!("thumbnail up for {seconds} s");
        let generation = state.thumbnail_generation.get() + 1;
        state.thumbnail_generation.set(generation);
        if let Some(previous) = state.thumbnail.replace(Some(card)) {
            previous.close(cx);
        }

        while let Some(event) = events.next().await {
            match event {
                // Shown in Shuttercrab's window, as Snipping Tool opens a
                // snip from its notification.
                ThumbnailEvent::Open => {
                    match file.shot(taken_at, cx).await {
                        Ok(shot) => {
                            log::info!("thumbnail: showing the screenshot in the window");
                            show_in_main(&state, shot, cx);
                        }
                        Err(e) => log::error!("{e}"),
                    }
                    break;
                }
                ThumbnailEvent::Drag => match file.path(cx).await {
                    // A modal loop until the drop; this task is outside any
                    // GPUI update, so the windows keep working meanwhile.
                    Ok(path) => match shuttercrab_platform::drag::drag_file(
                        &path,
                        Some(DragImage {
                            width: drag_picture.width,
                            height: drag_picture.height,
                            rgba: &drag_picture.rgba,
                        }),
                    ) {
                        Ok(true) => {
                            log::info!("thumbnail: dropped into another application");
                            break;
                        }
                        Ok(false) => log::info!("thumbnail: drag cancelled"),
                        Err(e) => log::warn!("thumbnail drag failed: {e:#}"),
                    },
                    Err(e) => log::error!("{e}"),
                },
                ThumbnailEvent::Close => break,
            }
        }
        // Close it unless a newer thumbnail has replaced it.
        if state.thumbnail_generation.get() == generation
            && let Some(card) = state.thumbnail.take()
        {
            card.close(cx);
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumbnail_sits_above_the_taskbar_at_the_right() {
        // 150%, work area 3840×2088 (a 72-pixel taskbar), a 16:9 screenshot:
        // a 240×135 image plus 6 pixels of padding, 16 from the edges.
        let work = PhysicalRect::new(0, 0, 3840, 2088);
        let rect = thumbnail_rect(work, 1.5, (3840, 2160));
        assert_eq!((rect.width, rect.height), (378, 221));
        assert_eq!((rect.x, rect.y), (3840 - 378 - 24, 2088 - 221 - 24));
    }

    #[test]
    fn a_failed_copy_points_to_the_thumbnail_and_logs_the_cause() {
        let folder = Path::new(r"C:\Shots");
        let copy_failed = || Some(Err(anyhow::anyhow!("OpenClipboard: access denied")));
        let saved = Some(Ok(PathBuf::from(r"C:\Shots\a.png")));
        let failure = outcome(copy_failed(), saved, folder, false).unwrap_err();
        assert!(
            failure
                .message
                .starts_with("The screenshot was saved, but not copied.")
        );
        assert!(failure.message.contains("in the thumbnail"));
        assert_eq!(failure.detail, "copy: OpenClipboard: access denied");

        let failure = outcome(
            copy_failed(),
            Some(Err(anyhow::anyhow!("denied"))),
            folder,
            false,
        )
        .unwrap_err();
        assert!(failure.message.contains(r"Could not save to C:\Shots."));
    }

    #[test]
    fn steps_that_worked_or_were_off_are_not_failures() {
        let folder = Path::new(r"C:\Shots");
        let saved = || Some(Ok(PathBuf::from(r"C:\Shots\a.png")));
        assert!(outcome(Some(Ok(())), saved(), folder, false).is_ok());
        assert!(outcome(None, saved(), folder, false).is_ok());
        assert!(outcome(Some(Ok(())), None, folder, false).is_ok());
        assert!(outcome(None, None, folder, false).is_ok());
    }
}
