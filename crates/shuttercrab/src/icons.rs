//! The icons Shuttercrab draws, embedded: GPUI Kit's component icons plus the
//! few Lucide icons the main window, the Capture Bar and the recording
//! controls use.

use gpui_kit::{AssetSource, SharedString, assets::icon_assets};
use std::borrow::Cow;

icon_assets!(
    Extra,
    [
        SquareDashed,
        AppWindow,
        Monitor,
        Camera,
        Video,
        X,
        Square,
        RotateCcw,
        Trash,
        Lasso,
        Timer,
        TimerOff,
        Plus,
        ArrowLeft,
        ChevronDown,
        FolderOpen,
        Settings,
        Power,
        Copy,
        Save,
        ZoomIn,
        Brush,
        Check,
        Pen,
        Highlighter,
        Undo2,
        Redo2,
        Eraser,
        Shapes,
        Circle,
        Slash,
        MoveUpLeft,
        Ban,
        RotateCw,
        MousePointer2,
        FaceSlightlySmiling,
        Crop,
        Hand,
        HandGrab,
        Mic,
        MicOff,
        Volume2,
        VolumeX
    ]
);

/// Shuttercrab's asset source.
pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = Extra.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(Extra.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::assets::IconName;

    #[test]
    fn the_icons_shuttercrab_draws_are_embedded() {
        for icon in [
            IconName::SquareDashed,
            IconName::AppWindow,
            IconName::Monitor,
            IconName::Camera,
            IconName::Video,
            IconName::X,
            IconName::Square,
            IconName::RotateCcw,
            IconName::Trash,
            IconName::Pause,
            IconName::Play,
            IconName::Ellipsis,
            IconName::Lasso,
            IconName::Timer,
            IconName::TimerOff,
            IconName::Plus,
            IconName::ArrowLeft,
            IconName::ChevronDown,
            IconName::FolderOpen,
            IconName::Settings,
            IconName::Power,
            IconName::Copy,
            IconName::Save,
            IconName::ZoomIn,
            IconName::Brush,
            IconName::Check,
            IconName::Pen,
            IconName::Highlighter,
            IconName::Undo2,
            IconName::Redo2,
            IconName::Eraser,
            IconName::Shapes,
            IconName::Circle,
            IconName::Slash,
            IconName::MoveUpLeft,
            IconName::Ban,
            IconName::RotateCw,
            IconName::MousePointer2,
            IconName::FaceSlightlySmiling,
            IconName::Crop,
            IconName::Hand,
            IconName::HandGrab,
            IconName::Mic,
            IconName::MicOff,
            IconName::Volume2,
            IconName::VolumeX,
        ] {
            let path = icon.path();
            assert!(Icons.load(&path).unwrap().is_some(), "{path}");
        }
    }
}
