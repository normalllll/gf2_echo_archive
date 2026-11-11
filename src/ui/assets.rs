//! Embedded SVG icons: the few gpui-component needs plus the navigation icons
//! from the design mock-ups. Everything is compiled in so the release build
//! stays a single executable.

use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

fn stroke(path: &str, width: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round"><path d="{path}"/></svg>"#
    )
}

pub fn icon_svg(path: &str) -> Option<String> {
    let svg = match path {
        // gpui-component internals.
        "icons/check.svg" => stroke("M20 6 9 17l-5-5", "2.4"),
        "icons/chevron-down.svg" => stroke("m6 9 6 6 6-6", "2"),
        "icons/chevron-up.svg" => stroke("m18 15-6-6-6 6", "2"),
        "icons/chevron-left.svg" => stroke("m15 18-6-6 6-6", "2"),
        "icons/chevron-right.svg" => stroke("m9 18 6-6-6-6", "2"),
        "icons/chevrons-up-down.svg" => stroke("m7 15 5 5 5-5M7 9l5-5 5 5", "2"),
        "icons/search.svg" => stroke("M11 18a7 7 0 1 0 0-14a7 7 0 0 0 0 14zM20 20l-4-4", "2"),
        "icons/close.svg" => stroke("M18 6 6 18M6 6l12 12", "2"),
        "icons/circle-x.svg" => stroke(
            "M12 22a10 10 0 1 0 0-20a10 10 0 0 0 0 20zM15 9l-6 6M9 9l6 6",
            "2",
        ),
        "icons/minus.svg" => stroke("M5 12h14", "2"),
        "icons/plus.svg" => stroke("M5 12h14M12 5v14", "2"),
        "icons/inbox.svg" => stroke(
            "M22 12h-6l-2 3h-4l-2-3H2M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z",
            "2",
        ),
        "icons/loader.svg" => stroke(
            "M12 2v4M12 18v4M4.9 4.9l2.8 2.8M16.3 16.3l2.8 2.8M2 12h4M18 12h4M4.9 19.1l2.8-2.8M16.3 7.7l2.8-2.8",
            "2",
        ),
        "icons/window-minimize.svg" => stroke("M5 12h14", "1.2"),
        "icons/window-maximize.svg" => stroke("M5 5h14v14H5z", "1.2"),
        "icons/window-restore.svg" => stroke("M8 8h11v11H8zM5 16V5h11", "1.2"),
        "icons/window-close.svg" => stroke("M6 6l12 12M18 6 6 18", "1.2"),
        // Echo Archive navigation (paths from the design).
        "app/home.svg" => stroke(
            "M3 11l9-7 9 7v9a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z",
            "1.8",
        ),
        "app/audio.svg" => stroke(
            "M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0a3 3 0 0 1 6 0zM21 16a3 3 0 1 1-6 0a3 3 0 0 1 6 0z",
            "1.8",
        ),
        "app/story.svg" => stroke(
            "M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z",
            "1.8",
        ),
        "app/portrait.svg" => stroke("M4 4h16v16H4zM4 16l5-5 4 4 3-3 4 4M15.5 8.5h.01", "1.8"),
        "app/live2d.svg" => stroke(
            "M12 12a4 4 0 1 0 0-8a4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0",
            "1.8",
        ),
        "app/tasks.svg" => stroke("M4 6h16M4 12h16M4 18h10", "1.8"),
        "app/folder.svg" => stroke("M3 6h7l2 2h9v11H3z", "1.8"),
        // v2 navigation and controls.
        "app/chars.svg" => stroke(
            "M12 12a4 4 0 1 0 0-8a4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0",
            "1.8",
        ),
        "app/book.svg" => stroke(
            "M4 5a2 2 0 0 1 2-2h13v16H6a2 2 0 0 0-2 2zM4 5v16M8 7h7",
            "1.8",
        ),
        "app/settings.svg" => stroke("M4 7h10M18 7h2M4 17h4M12 17h8M16 5v4M10 15v4", "1.8"),
        "app/sidebar.svg" => stroke("M4 4h16v16H4zM9 4v16M15 10l-2 2 2 2", "1.8"),
        "app/check-bold.svg" => stroke("M5 12l5 5 9-10", "3"),
        "app/chevron-down.svg" => stroke("M6 9l6 6 6-6", "2"),
        "app/chevron-left.svg" => stroke("m15 18-6-6 6-6", "2"),
        "app/chevron-right.svg" => stroke("m9 18 6-6-6-6", "2"),
        "app/close.svg" => stroke("M18 6 6 18M6 6l12 12", "2"),
        "app/search.svg" => stroke("M11 18a7 7 0 1 0 0-14a7 7 0 0 0 0 14zM20 20l-4-4", "2"),
        "app/file.svg" => stroke("M6 3h8l4 4v14H6z", "1.8"),
        "app/info.svg" => stroke(
            "M12 22a10 10 0 1 0 0-20a10 10 0 0 0 0 20zM12 16v-4M12 8h.01",
            "2",
        ),
        _ => return None,
    };
    Some(svg)
}

/// The two-colour app icon shown in the title bar.
pub const APP_ICON: &str = "app/icon.png";
const APP_ICON_PNG: &[u8] = include_bytes!("../../resources/icon/app-icon-64.png");

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(APP_ICON_PNG)));
        }
        Ok(icon_svg(path).map(|svg| Cow::Owned(svg.into_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICON_PATHS
            .iter()
            .filter(|icon| icon.starts_with(path))
            .map(|icon| SharedString::from(*icon))
            .collect())
    }
}

const ICON_PATHS: &[&str] = &[
    "icons/check.svg",
    "icons/chevron-down.svg",
    "icons/chevron-up.svg",
    "icons/chevron-left.svg",
    "icons/chevron-right.svg",
    "icons/chevrons-up-down.svg",
    "icons/search.svg",
    "icons/close.svg",
    "icons/circle-x.svg",
    "icons/minus.svg",
    "icons/plus.svg",
    "icons/inbox.svg",
    "icons/loader.svg",
    "icons/window-minimize.svg",
    "icons/window-maximize.svg",
    "icons/window-restore.svg",
    "icons/window-close.svg",
    "app/home.svg",
    "app/audio.svg",
    "app/story.svg",
    "app/portrait.svg",
    "app/live2d.svg",
    "app/tasks.svg",
    "app/folder.svg",
    "app/chars.svg",
    "app/book.svg",
    "app/settings.svg",
    "app/sidebar.svg",
    "app/check-bold.svg",
    "app/chevron-down.svg",
    "app/chevron-left.svg",
    "app/chevron-right.svg",
    "app/close.svg",
    "app/search.svg",
    "app/file.svg",
    "app/info.svg",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_icon_is_embedded() {
        for path in ICON_PATHS {
            assert!(icon_svg(path).is_some(), "{path}");
        }
        assert!(icon_svg("icons/unknown.svg").is_none());
    }
}
