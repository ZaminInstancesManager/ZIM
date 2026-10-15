//! The tray icon: show/hide, new-tab navigation, and an explicit
//! "engine keeps running" quit.
//!
//! The tray exists because the shell is disposable by design —
//! the engine (and every server) is not. Closing the window just
//! hides ZIM from the taskbar area; the tray is how you get it
//! back, and the quit label says exactly what quitting does (and
//! does not do).

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, WebviewWindow};

/// Build and attach the tray for the app.
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show ZIM", true, None::<&str>)?;
    let newtab = MenuItem::with_id(app, "newtab", "Open zim://newtab", true, None::<&str>)?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        "Quit ZIM — the engine and servers keep running",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(
        app,
        &[&show, &newtab, &PredefinedMenuItem::separator(app)?, &quit],
    )?;

    let mut builder = TrayIconBuilder::with_id("zim-tray")
        .tooltip("ZIM — engine: starting…")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "newtab" => {
                if let Err(e) = crate::navigate_to_zim_url(app, "zim://newtab") {
                    eprintln!("zim_shell: tray new-tab navigation failed: {e}");
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click { button, .. } = event {
                if button == tauri::tray::MouseButton::Left {
                    show_main(tray.app_handle());
                }
            }
        });

    // The tray icon falls back to the app's configured window icon
    // (tauri.conf.json → bundle.icon); absent icon = default tray
    // glyph, not a panic.
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }

    builder.build(app)?;

    Ok(())
}

/// Show and focus the main window (creating it if the platform
/// closed it — the window is disposable; the engine is not).
fn show_main(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(window) => {
            let _ = window.show();
            let _ = window.set_focus();
        }
        None => {
            // M21 keeps a single window; M22 (multiple windows and
            // session restore) generalizes this.
            let _ = WebviewWindow::builder(app, "main", tauri::WebviewUrl::App("boot.html".into()))
                .title("ZIM")
                .inner_size(1280.0, 800.0)
                .build();
        }
    }
}

/// Update the tray tooltip from the engine status.
pub fn set_engine_tooltip(app: &AppHandle, tooltip: &str) {
    if let Some(tray) = app.tray_by_id("zim-tray") {
        let _ = tray.set_tooltip(Some(tooltip));
    }
}
