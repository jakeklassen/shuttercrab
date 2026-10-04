//! A finished screenshot: copied and saved as the settings say, then shown
//! as a thumbnail that opens it or drags it out.

use super::{State, failure::Failure, screenshot::exclude_from_capture};
use crate::{
    files,
    popup::{self, Activation},
    thumbnail::{self, Thumbnail, ThumbnailEvent},
};
use chrono::NaiveDateTime;
use futures::StreamExt as _;
use gpui_kit::{AppContext as _, AsyncApp};
use shuttercrab_capture::{MonitorInfo, PhysicalRect, Screenshot};
use shuttercrab_platform::{drag::DragImage, window as platform_window};
use std::{path::PathBuf, rc::Rc, sync::Arc, time::Instant};

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

    // Start the file write and the thumbnail image first; they run while
    // the clipboard is written. All three share the screenshot's buffers.
    let saved = settings.auto_save.then(|| {
        let (dir, png) = (settings.output_dir(), shot.png.clone());
        cx.background_executor()
            .spawn(async move { files::save_screenshot(&dir, taken_at, &png) })
    });
    let preview = settings.show_thumbnail.then(|| {
        let rgba = shot.rgba.clone();
        let (w, h) = thumbnail::image_size(width, height);
        let scale = monitor.scale_factor;
        let (max_w, max_h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
        cx.background_executor()
            .spawn(async move { thumbnail::scale_down(&rgba, width, height, max_w, max_h) })
    });
    // Without auto-save, the thumbnail writes a temporary file only if it
    // is opened or dragged.
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
    // A screenshot that was neither copied nor saved is still shown, so it
    // is never lost: the thumbnail opens it and drags it out.
    let copy_failed = matches!(copied, Some(Err(_)));
    let rescued = copy_failed && saved_path.is_none();
    let thumbnail = settings.show_thumbnail || copy_failed;
    // PRD §24: say what happened and what to do; the causes go to the log.
    let busy = "Another app is holding the clipboard.";
    let in_thumbnail = "It's in the thumbnail: click to open it, or drag it out.";
    let folder = settings.output_dir();
    let unwritable = format!(
        "Could not save to {}. Check the folder in Settings.",
        folder.display()
    );
    let result = match (copied, saved) {
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
    };

    if thumbnail {
        let small = match preview {
            Some(preview) => preview.await,
            None => {
                let (w, h) = thumbnail::image_size(width, height);
                let scale = monitor.scale_factor;
                let (max_w, max_h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
                cx.background_executor()
                    .spawn(async move { thumbnail::scale_down(&rgba, width, height, max_w, max_h) })
                    .await
            }
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
            small,
            size: (width, height),
            file,
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

/// The file behind a thumbnail: saved already, or written to the temporary
/// folder the first time it is opened or dragged.
enum CaptureFile {
    Saved(PathBuf),
    Unsaved {
        png: Arc<Vec<u8>>,
        taken_at: NaiveDateTime,
    },
}

impl CaptureFile {
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
    small: thumbnail::Small,
    /// The screenshot's size, physical pixels.
    size: (u32, u32),
    file: CaptureFile,
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
            small,
            size,
            mut file,
            monitor,
            seconds,
        } = pending;
        let work = platform_window::work_area(monitor.id.0)
            .map(|(x, y, w, h)| PhysicalRect::new(x, y, w, h))
            .unwrap_or(monitor.bounds);
        let rect = thumbnail_rect(work, monitor.scale_factor, size);
        let image = thumbnail::render_image(&small);
        let drag_picture = thumbnail::soften(&small, thumbnail::DRAG_LOOK);
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
                ThumbnailEvent::Open => {
                    match file.path(cx).await {
                        Ok(path) => {
                            log::info!("thumbnail: opening the screenshot");
                            cx.update(|cx| cx.open_with_system(&path));
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
}
