use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconEvent;
use tauri::{App, Manager};
use tracing::info;

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
    }

    Ok(())
}
