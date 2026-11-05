//! Above a saved screenshot or a recording, a bar saying which folder it
//! went to, as Snipping Tool says of its recordings (issue #26), with ways
//! to the file, to the folder's setting (lit as the settings open), and to
//! stop saying so. Its × hides it until Shuttercrab starts again;
//! "Don't show again" turns it off in the settings, where it can be turned
//! back on.

use super::{ButtonFace, MainWindow, Page, TOOLBAR_HEIGHT, home::More};
use crate::ui::palette::{border, hover, muted, tile};
use gpui_kit::{
    ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    assets::IconName, component::Icon, div, px,
};
use std::path::{Path, PathBuf};

/// The bar's height, logical pixels: fixed, so the window can be sized
/// around a screenshot with it.
const NOTE_HEIGHT: f32 = 44.;

/// What the bar says: what was saved, and the folder it went to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SavedNote {
    recording: bool,
    folder: PathBuf,
}

impl MainWindow {
    /// What the bar says now, if it shows: on the home page, with a
    /// screenshot saved to its folder or a recording shown, unless it was
    /// closed or turned off.
    pub(super) fn saved_note(&self) -> Option<SavedNote> {
        if self.page != Page::Home || self.saved_note_closed || !self.settings().saved_note {
            return None;
        }
        let (recording, file) = match (&self.recording, self.shot()) {
            (Some(video), _) => (true, &video.path),
            (None, Some(shot)) => (false, shot.saved.as_ref()?),
            (None, None) => return None,
        };
        Some(SavedNote {
            recording,
            folder: file.parent()?.to_path_buf(),
        })
    }

    /// How far down the window the screenshot or the recording starts: under
    /// the toolbar, and the bar when it shows.
    pub(super) fn canvas_top(&self) -> f32 {
        let note = if self.saved_note().is_some() {
            NOTE_HEIGHT
        } else {
            0.
        };
        TOOLBAR_HEIGHT + note
    }

    /// The bar: where it was saved, Show in folder, the way to the setting,
    /// Don't show again, and ×.
    pub(super) fn saved_note_bar(
        &self,
        note: SavedNote,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let what = if note.recording {
            "Recordings"
        } else {
            "Screenshots"
        };
        let text = SharedString::from(format!(
            "{what} are saved to {}.",
            shown_folder(&note.folder)
        ));
        let page = if note.recording {
            Page::RecordingSettings
        } else {
            Page::ScreenshotSettings
        };
        let show_in_folder = Self::hint_button(
            ButtonFace {
                id: "saved-note-folder",
                label: "Show in folder",
                icon: IconName::FolderOpen,
                key: self.shown_key("o"),
            },
            cx.listener(|this, _: &ClickEvent, window, cx| {
                this.choose_more(More::OpenFolder, window, cx)
            }),
        );
        let settings = Self::hint_button(
            ButtonFace {
                id: "saved-note-settings",
                label: "Change folder",
                icon: IconName::Settings,
                key: "",
            },
            cx.listener(move |this, _: &ClickEvent, window, cx| this.show(page, window, cx)),
        );
        let never = Self::hint_button(
            ButtonFace {
                id: "saved-note-never",
                label: "Don't show again",
                icon: IconName::EyeOff,
                key: "",
            },
            cx.listener(|this, _: &ClickEvent, _, cx| this.change(cx, |s| s.saved_note = false)),
        );
        let close = div()
            .id("saved-note-close")
            .role(Role::Button)
            .aria_label("Close")
            .test_support()
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.))
            .rounded_md()
            .hover(|s| s.bg(hover()))
            .cursor_pointer()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.saved_note_closed = true;
                cx.notify();
            }))
            .child(Icon::new(IconName::X).size(px(16.)));
        div()
            .id("saved-note")
            .aria_label(text.clone())
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(NOTE_HEIGHT))
            .px_3()
            .overflow_hidden()
            .bg(tile())
            .border_b_1()
            .border_color(border())
            .child(Icon::new(IconName::Info).size(px(16.)).text_color(muted()))
            .child(div().flex_none().mr_2().child(text))
            .child(show_in_folder)
            .child(settings)
            .child(never)
            .child(div().flex_1())
            .child(close)
    }
}

/// `folder` as the bar shows it: from the home folder down when it is in
/// there (`Pictures\Shuttercrab`), otherwise in full.
fn shown_folder(folder: &Path) -> String {
    dirs::home_dir()
        .and_then(|home| folder.strip_prefix(home).ok().map(Path::to_path_buf))
        .filter(|inside| !inside.as_os_str().is_empty())
        .unwrap_or_else(|| folder.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_in_the_home_folder_are_shown_from_there() {
        let home = dirs::home_dir().unwrap();
        let pictures = Path::new("Pictures").join("Shuttercrab");
        assert_eq!(
            shown_folder(&home.join(&pictures)),
            pictures.display().to_string()
        );
        // The home folder itself, and folders elsewhere, in full.
        assert_eq!(shown_folder(&home), home.display().to_string());
        let elsewhere = std::env::temp_dir().join("elsewhere");
        if !elsewhere.starts_with(&home) {
            assert_eq!(shown_folder(&elsewhere), elsewhere.display().to_string());
        }
    }
}
