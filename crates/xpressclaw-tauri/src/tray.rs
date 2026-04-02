use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconEvent;
use tauri::{App, Manager};
use tracing::{info, warn};

/// Set up the system tray with menu items.
pub fn setup_tray(app: &App, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let dashboard = MenuItem::with_id(app, "dashboard", "Open Dashboard", true, None::<&str>)?;
    let browser = MenuItem::with_id(app, "browser", "Open in Browser", true, None::<&str>)?;
    let separator = MenuItem::with_id(app, "sep", "─────────────", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit xpressclaw", true, None::<&str>)?;

    let menu = Menu::with_items(app, &[&dashboard, &browser, &separator, &quit])?;

    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_menu(Some(menu))?;
        tray.set_tooltip(Some(&format!("xpressclaw - localhost:{port}")))?;

        // On Linux, the default tray icon is a black monochrome template
        // designed for macOS (which auto-inverts it). On dark panels (GNOME,
        // KDE, etc.) it's invisible. Use the inverted (white) version instead.
        #[cfg(target_os = "linux")]
        {
            let icon_bytes = include_bytes!("../icons/tray-icon-light.png");
            match tauri::image::Image::from_bytes(icon_bytes) {
                Ok(img) => {
                    let _ = tray.set_icon(Some(img));
                    info!("tray icon set to light variant for Linux");
                }
                Err(e) => warn!(error = %e, "failed to set Linux tray icon"),
            }
        }

        let handle = app.handle().clone();
        tray.on_menu_event(move |_app, event| {
            match event.id().as_ref() {
                "dashboard" => {
                    // Show/focus the main window
                    if let Some(window) = handle.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "browser" => {
                    let url = format!("http://localhost:{port}");
                    let _ = open::that(&url);
                }
                "quit" => {
                    info!("quit requested from tray");
                    crate::confirm_quit(&handle);
                }
                _ => {}
            }
        });

        let handle = app.handle().clone();
        tray.on_tray_icon_event(move |_tray, event| {
            if let TrayIconEvent::Click { .. } = event {
                if let Some(window) = handle.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });

        info!("tray icon registered (id=main-tray)");
    } else {
        warn!("tray icon 'main-tray' not found — system tray will be unavailable");
    }

    Ok(())
}
