//! The M7 background keep-alive tray (D10 amendment, 2026-11-11).
//!
//! The tray exists **exactly while** the "keep running in the background"
//! Settings option is on: it is built at boot (when the saved option is
//! on) and reconciled on every successful `settings_save`. It is the
//! only surface while the window is hidden, so its menu carries the
//! three things a hidden app must answer: show, sync, quit.
//!
//! - *Show Mailchat Vault* — reveals and focuses the main window (the
//!   same action a second launch takes through the single-instance
//!   guard, and what a tray left-click does).
//! - *Sync now* — submits an ordinary `imap_sync_all` job through the
//!   same path as the manual round; nothing scheduler-specific, so it
//!   shows up in the jobs panel like any manual sync.
//! - *Quit* — exits the process (`app.exit(0)`), identical to today's
//!   window-close exit; the pool and jobs end exactly as they always
//!   have on exit.
//!
//! Honest degradation is the rule (see the D10 amendment): keep-alive
//! must never leave a hidden unreachable process — the close handler
//! hides only when the tray icon actually exists ([`exists()`]), so
//! wherever the tray could not be built the window close behaves
//! exactly as before the option existed. The Linux tray renders through
//! appindicator, which the tray-icon stack loads with `dlopen` and
//! *panics* on when absent — hence the explicit probe below instead of
//! letting the first tray build crash the app on a minimal system.

use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

/// Stable id of the one tray icon this app manages.
pub const TRAY_ID: &str = "mc-tray";

/// Menu item ids (`MenuEvent::id` values) the handler dispatches on.
const MENU_SHOW: &str = "mc-show";
const MENU_SYNC: &str = "mc-sync-now";
const MENU_QUIT: &str = "mc-quit";

/// Whether this desktop can actually show the tray icon. `true` where
/// the platform tray is native (macOS, Windows); on Linux the
/// appindicator stack is `dlopen`ed at first use and *panics* when the
/// library is missing, so probe the same two sonames it loads before
/// anything is built. Cached: the answer cannot change while the
/// process lives.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return true;
        }
        // The exact sonames libappindicator-sys tries, in its order.
        ["libayatana-appindicator3.so.1", "libappindicator3.so.1"]
            .iter()
            .any(|name| unsafe { libloading::Library::new(name) }.is_ok())
    })
}

/// Build the tray if it is not up yet (idempotent). `Err` carries a
/// user-facing message for the log; callers degrade — keep-alive never
/// hard-fails because the tray could not be built.
pub fn ensure(app: &AppHandle) -> Result<(), String> {
    if !available() {
        return Err("no tray stack (appindicator) on this system".into());
    }
    if app.tray_by_id(TRAY_ID).is_some() {
        return Ok(());
    }
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "no default window icon for the tray".to_string())?;
    let show = menu_item(app, MENU_SHOW, "Show Mailchat Vault");
    let sync = menu_item(app, MENU_SYNC, "Sync now");
    let quit = menu_item(app, MENU_QUIT, "Quit");
    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&sync)
        .item(&quit)
        .build()
        .map_err(|e| e.to_string())?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("Mailchat Vault")
        .menu(&menu)
        // Left-click shows the window (the menu opens on right-click,
        // the platform convention for appindicator/StatusNotifier).
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether the tray icon is actually up — the result of a successful
/// [`ensure`]. Keep-alive trusts only this, never the library probe: a
/// hidden window must always have its way back in place.
pub fn exists(app: &AppHandle) -> bool {
    app.tray_by_id(TRAY_ID).is_some()
}

/// Drop the tray icon (the window close exits the app again, exactly as
/// before the keep-alive option existed).
pub fn remove(app: &AppHandle) {
    drop(app.remove_tray_by_id(TRAY_ID));
}

fn menu_item(app: &AppHandle, id: &str, text: &str) -> tauri::menu::MenuItem<tauri::Wry> {
    // Menu items are static text here; failures can only be allocation
    // class, and a broken tray menu must not take the app down.
    MenuItem::with_id(app, id, text, true, None::<&str>).expect("static menu item builds")
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_SYNC => {
            // An ordinary imap_sync_all round — the same submission path
            // the jobs panel uses. Best-effort from a menu click: a
            // refusal (e.g. the isolated sample archive is open) is
            // logged, never crashes the tray.
            if let Err(e) = crate::submit_sync_now(app) {
                eprintln!("tray Sync now refused: {e}");
            }
        }
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

/// Reveal, unminimize and focus the main window — the shared "bring the
/// app back" action for the tray menu, tray left-click, and a second
/// launch through the single-instance guard.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
