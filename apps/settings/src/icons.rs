//! Kit's default icons plus the small Lucide subset used by this window.
use gpui_kit::AssetSource;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use std::borrow::Cow;

/// The full catalogue's names. Only the ones listed in [`Extra`] and in Kit's
/// own default bundle resolve to bytes at runtime.
pub use gpui_kit::assets::IconName;

gpui_kit::assets::icon_assets!(
    Extra,
    [
        Ban,
        CircleDot,
        FlaskConical,
        Gauge,
        Github,
        Globe,
        IdCard,
        Image,
        ListChecks,
        Lightbulb,
        Mouse,
        MousePointer,
        MousePointer2,
        MousePointerClick,
        Plug,
        Radar,
        Unplug,
        Wifi,
        WifiOff,
        Zap
    ]
);

/// Kit's default icons, plus [`Extra`].
#[derive(Clone, Copy)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = Extra.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<gpui_kit::SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(Extra.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// A filled Lucide `zap`, which reads more clearly over the battery fill.
const BOLT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor"><path d="M15.914 4a1.5 1.5 0 00-2.474-1.561l-9 9A1.5 1.5 0 005.5 14h4.002a.5.5 0 01.471.666L8.086 20a1.5 1.5 0 002.475 1.56l9-9A1.5 1.5 0 0018.5 10h-3.997a.5.5 0 01-.472-.667z"/></svg>"#;

/// The app-owned filled bolt as a sizeable, colorable icon.
pub fn bolt() -> Icon {
    // Icons are laid out as flex items, which shrink to their container by
    // default. This one is sized against the cell it crosses, not by it.
    Icon::default().data(BOLT).flex_shrink_0()
}
