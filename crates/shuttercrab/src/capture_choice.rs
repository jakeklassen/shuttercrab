//! What the user chooses to capture: a screenshot or a recording, and of
//! what. The Capture Bar, the main window and the settings share these.

use gpui_kit::assets::IconName;
use serde::{Deserialize, Serialize};

/// Whether to take a screenshot or start a recording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    #[default]
    Screenshot,
    Record,
}

impl CaptureMode {
    pub const ALL: [CaptureMode; 2] = [Self::Screenshot, Self::Record];

    pub fn label(self) -> &'static str {
        match self {
            Self::Screenshot => "Screenshot",
            Self::Record => "Record",
        }
    }

    /// The letter that switches to it from the keyboard.
    pub fn key(self) -> &'static str {
        match self {
            Self::Screenshot => "s",
            Self::Record => "r",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Screenshot => IconName::Camera,
            Self::Record => IconName::Video,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Screenshot => "mode-screenshot",
            Self::Record => "mode-record",
        }
    }

    /// Whether `target` can be captured this way: recordings are of an
    /// area or a display (PRD §7.7).
    pub fn offers(self, target: CaptureTarget) -> bool {
        self == Self::Screenshot || matches!(target, CaptureTarget::Area | CaptureTarget::Display)
    }
}

/// What a screenshot or recording captures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureTarget {
    #[default]
    Area,
    Window,
    Display,
    /// Drawn around by hand; screenshots only.
    Freeform,
}

impl CaptureTarget {
    pub const ALL: [CaptureTarget; 4] = [Self::Area, Self::Window, Self::Display, Self::Freeform];

    pub fn label(self) -> &'static str {
        match self {
            Self::Area => "Area",
            Self::Window => "Window",
            Self::Display => "Display",
            Self::Freeform => "Freeform",
        }
    }

    /// The letter that picks it from the keyboard.
    pub fn key(self) -> &'static str {
        match self {
            Self::Area => "a",
            Self::Window => "w",
            Self::Display => "d",
            Self::Freeform => "f",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Self::Area => IconName::SquareDashed,
            Self::Window => IconName::AppWindow,
            Self::Display => IconName::Monitor,
            Self::Freeform => IconName::Lasso,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Area => "target-area",
            Self::Window => "target-window",
            Self::Display => "target-display",
            Self::Freeform => "target-freeform",
        }
    }
}
