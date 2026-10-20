//! The one window, and what it may navigate to.

use desktop_core::validate;
use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{AppHandle, Manager as _, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

use crate::state::AppState;

pub const LABEL: &str = "main";

/// The renderer is the only page this window shows: the app's own origin on each system.
fn own_page(url: &Url) -> bool {
    matches!(
        (url.scheme(), url.host_str()),
        ("tauri", Some("localhost")) | ("https", Some("tauri.localhost"))
    )
}

/// A link or `window.open` in the renderer goes to the user's browser, if it is a web link, and nowhere else.
fn open_externally(app: &AppHandle, url: &Url) {
    if let Ok(safe) = validate::safe_external_url(url.as_str()) {
        let _ = app.opener().open_url(safe, None::<&str>);
    }
}

/// `#rrggbb` as a colour; anything else is `None`.
fn hex_colour(text: &str) -> Option<Color> {
    let digits = text.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color(channel(0)?, channel(2)?, channel(4)?, 255))
}

/// The colour the page will paint, so the window does not flash white before it loads. The theme is the user's setting;
/// `system` follows the system, which is not known before the window exists, so dark is assumed (the renderer repaints at
/// once either way).
fn background(app: &AppHandle) -> Option<Color> {
    let state = app.try_state::<AppState>()?;
    let settings = state.store.state().settings;
    let mode = settings
        .get("themeMode")
        .and_then(|mode| mode.as_str())
        .unwrap_or("system");
    let theme = if mode == "light" { "lightTheme" } else { "darkTheme" };
    hex_colour(settings.get(theme)?.get("surface")?.as_str()?)
}

pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let (nav_app, popup_app) = (app.clone(), app.clone());
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title(&desktop_core::product::product().name)
        .inner_size(1280.0, 860.0)
        .min_inner_size(980.0, 640.0)
        .use_https_scheme(true)
        .on_navigation(move |url| {
            if own_page(url) {
                return true;
            }
            open_externally(&nav_app, url);
            false
        })
        .on_new_window(move |url, _features| {
            open_externally(&popup_app, &url);
            NewWindowResponse::Deny
        });
    if let Some(colour) = background(app) {
        builder = builder.background_color(colour);
    }
    // Windows draws its own top bar (see `tauriChrome.ts`); macOS keeps its traffic lights over the page; Linux keeps the
    // system frame.
    #[cfg(windows)]
    {
        builder = builder.decorations(false);
    }
    #[cfg(target_os = "macos")]
    {
        use tauri::{LogicalPosition, TitleBarStyle};
        builder = builder
            .title_bar_style(TitleBarStyle::Overlay)
            .hidden_title(true)
            .traffic_light_position(LogicalPosition::new(16.0, 18.0));
    }
    builder.build()
}

pub fn main_window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(LABEL)
}

/// Brings the window to the front, for a second launch of the app.
pub fn focus(app: &AppHandle) {
    if let Some(window) = main_window(app) {
        if window.is_minimized().unwrap_or(false) {
            let _ = window.unminimize();
        }
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_origin_is_its_page() {
        let page = |text: &str| own_page(&Url::parse(text).unwrap());
        assert!(page("https://tauri.localhost/index.html"));
        assert!(page("tauri://localhost/index.html"));
        assert!(!page("http://tauri.localhost/"));
        assert!(!page("https://tauri.localhost.evil.example/"));
        assert!(!page("https://example.com/"));
        assert!(!page("tauri://evil/"));
        assert!(!page("file:///etc/passwd"));
    }

    #[test]
    fn colours_are_six_digit_hex() {
        assert_eq!(
            hex_colour("#1e1e2e").map(|Color(r, g, b, a)| (r, g, b, a)),
            Some((0x1e, 0x1e, 0x2e, 255))
        );
        assert!(hex_colour("1e1e2e").is_none());
        assert!(hex_colour("#fff").is_none());
        assert!(hex_colour("#gggggg").is_none());
    }
}
