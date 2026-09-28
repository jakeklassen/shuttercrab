//! The icons Framecut draws, embedded: GPUI Kit's component icons plus the
//! few Lucide icons the Capture Bar uses.

use gpui_kit::{AssetSource, SharedString, assets::icon_assets};
use std::borrow::Cow;

icon_assets!(Extra, [SquareDashed, AppWindow, Monitor, Camera, Video, X]);

/// Framecut's asset source.
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
    fn the_capture_bar_icons_are_embedded() {
        for icon in [
            IconName::SquareDashed,
            IconName::AppWindow,
            IconName::Monitor,
            IconName::Camera,
            IconName::Video,
            IconName::X,
        ] {
            let path = icon.path();
            assert!(Icons.load(&path).unwrap().is_some(), "{path}");
        }
    }
}
