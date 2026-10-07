//! Application wiring: bridge → (webviews, Flow bar, tray).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use dictate_proto::{Command, State};
use serde_json::Value;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::Notify;

use crate::bridge::{Bridge, BridgeConfig, Connection, EventSink};
use crate::hud::{placement, HudAction, HudVisibility};
use crate::icons::{self, TrayState};

/// Logical size of the Flow bar window (the pill plus a margin for its
/// shadow). Mirrored in `ui/src/hud/hud.css`.
pub const HUD_SIZE: (u32, u32) = (232, 56);
/// Logical gap between the bar and the bottom of the monitor — clear of a
/// bottom i3bar.
pub const HUD_BOTTOM_MARGIN: f64 = 44.0;

/// Webview event carrying a raw daemon event.
pub const EVENT_DAEMON: &str = "daemon-event";
/// Webview event carrying a [`Connection`].
pub const EVENT_CONNECTION: &str = "daemon-connection";
/// Webview event asking the hub to show a page.
pub const EVENT_NAVIGATE: &str = "hub-navigate";

struct TrayHandles {
    tray: TrayIcon,
    toggle: MenuItem<tauri::Wry>,
    cancel: MenuItem<tauri::Wry>,
}

/// Receives everything the daemon says and fans it out.
pub struct AppSink {
    app: AppHandle,
    hud: Mutex<HudVisibility>,
    hud_changed: Notify,
    last_session: Mutex<Option<Value>>,
    connected: Mutex<bool>,
    tray: Mutex<Option<TrayHandles>>,
}

impl AppSink {
    fn new(app: AppHandle) -> Self {
        Self {
            app,
            hud: Mutex::new(HudVisibility::default()),
            hud_changed: Notify::new(),
            last_session: Mutex::new(None),
            connected: Mutex::new(false),
            tray: Mutex::new(None),
        }
    }

    /// The last `state_changed` event seen.
    #[must_use]
    pub fn last_session_event(&self) -> Option<Value> {
        self.last_session.lock().ok()?.clone()
    }

    fn apply(&self, action: HudAction) {
        match action {
            HudAction::Show => show_hud(&self.app),
            HudAction::Hide => {
                if let Some(w) = self.app.get_webview_window("hud") {
                    let _ = w.hide();
                }
            }
            HudAction::Keep => {}
        }
        self.hud_changed.notify_one();
    }

    fn set_tray(&self, state: TrayState, session: Option<&State>) {
        let Ok(guard) = self.tray.lock() else { return };
        let Some(t) = guard.as_ref() else { return };
        let _ = t.tray.set_icon(Some(tauri::image::Image::new_owned(
            icons::rgba(state),
            icons::SIZE,
            icons::SIZE,
        )));
        let _ = t.tray.set_tooltip(Some(state.tooltip()));
        let connected = state != TrayState::Disconnected;
        let recording = matches!(session, Some(State::Recording));
        let busy = matches!(
            session,
            Some(State::Transcribing | State::Formatting | State::Injecting)
        );
        let _ = t.toggle.set_text(if recording {
            "Stop dictation"
        } else {
            "Start dictation"
        });
        let _ = t.toggle.set_enabled(connected && !busy);
        let _ = t.cancel.set_enabled(connected && (recording || busy));
    }

    /// Hide the bar when its linger elapses. Sleeps until the next deadline
    /// rather than polling, so an idle UI costs no wakeups.
    async fn hud_timer(self: Arc<Self>) {
        loop {
            let due = self.hud.lock().ok().and_then(|v| v.hide_at());
            match due {
                Some(at) => {
                    tokio::select! {
                        () = tokio::time::sleep_until(at.into()) => {
                            let action = self.hud.lock().map(|mut v| v.on_tick(Instant::now()));
                            if let Ok(action) = action {
                                if action == HudAction::Hide {
                                    if let Some(w) = self.app.get_webview_window("hud") {
                                        let _ = w.hide();
                                    }
                                }
                            }
                        }
                        () = self.hud_changed.notified() => {}
                    }
                }
                None => self.hud_changed.notified().await,
            }
        }
    }
}

impl EventSink for AppSink {
    fn connection(&self, state: &Connection) {
        let _ = self.app.emit(EVENT_CONNECTION, state);
        let connected = state.is_connected();
        if let Ok(mut c) = self.connected.lock() {
            *c = connected;
        }
        if !connected {
            let action = self.hud.lock().map(|mut v| v.hide_now());
            if let Ok(action) = action {
                self.apply(action);
            }
            self.set_tray(TrayState::Disconnected, None);
        } else {
            self.set_tray(TrayState::Idle, None);
        }
    }

    fn event(&self, event: &Value) {
        let _ = self.app.emit(EVENT_DAEMON, event);
        if event.get("type").and_then(Value::as_str) != Some("state_changed") {
            return;
        }
        let Some(to) = event
            .get("to")
            .and_then(Value::as_str)
            .map(|s| State::from(s.to_string()))
        else {
            return;
        };
        if let Ok(mut last) = self.last_session.lock() {
            *last = Some(event.clone());
        }
        let action = self.hud.lock().map(|mut v| v.on_state(&to, Instant::now()));
        if let Ok(action) = action {
            self.apply(action);
        }
        self.set_tray(TrayState::from_session(&to), Some(&to));
    }
}

/// Where the bar should sit, shared with the X11 map handler.
#[derive(Default)]
struct HudPin(crate::hud::Pin);

/// Position the bar bottom-center on the monitor under the pointer, and map it.
fn show_hud(app: &AppHandle) {
    let Some(window) = app.get_webview_window("hud") else {
        return;
    };
    let pointer = app.cursor_position().ok().map(|p| (p.x, p.y));
    let monitors: Vec<placement::Monitor> = app
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| placement::Monitor {
            x: m.position().x,
            y: m.position().y,
            width: m.size().width,
            height: m.size().height,
            scale: m.scale_factor(),
        })
        .collect();
    if let Some(monitor) = placement::monitor_under(pointer, &monitors) {
        let (x, y) = placement::bottom_center(
            &monitor,
            (f64::from(HUD_SIZE.0), f64::from(HUD_SIZE.1)),
            HUD_BOTTOM_MARGIN,
        );
        if let Some(pin) = app.try_state::<HudPin>() {
            if let Ok(mut slot) = pin.0.lock() {
                // GDK coordinates are logical pixels.
                *slot = Some((
                    (f64::from(x) / monitor.scale).round() as i32,
                    (f64::from(y) / monitor.scale).round() as i32,
                ));
            }
        }
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let _ = window.show();
}

fn create_hud(app: &tauri::App) -> tauri::Result<()> {
    let window = WebviewWindowBuilder::new(app, "hud", WebviewUrl::App("hud.html".into()))
        .title("Flow bar")
        .inner_size(f64::from(HUD_SIZE.0), f64::from(HUD_SIZE.1))
        .resizable(false)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        // Both, deliberately: `focused(false)` alone is undone by tao after the
        // first draw. See x11.rs.
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()?;
    let pin = HudPin::default();
    #[cfg(target_os = "linux")]
    if let Err(e) = crate::x11::harden_overlay(&window, HUD_SIZE, pin.0.clone()) {
        eprintln!("dictate-ui: could not apply X11 overlay hints: {e}");
    }
    app.manage(pin);
    Ok(())
}

/// Bring up the hub window (creating it on first use), optionally on `page`.
///
/// # Errors
///
/// If the window cannot be created.
pub fn open_hub(app: &AppHandle, page: Option<&str>) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("hub") {
        window.show()?;
        window.unminimize()?;
        window.set_focus()?;
        if let Some(page) = page {
            window.emit(EVENT_NAVIGATE, page)?;
        }
        return Ok(());
    }
    let url = format!("index.html#/{}", page.unwrap_or("home"));
    WebviewWindowBuilder::new(app, "hub", WebviewUrl::App(url.into()))
        .title("dictate")
        .inner_size(1000.0, 700.0)
        .min_inner_size(760.0, 520.0)
        .build()?;
    Ok(())
}

/// Put `text` on the CLIPBOARD selection.
///
/// # Errors
///
/// If the main thread cannot be reached.
pub fn copy_to_clipboard(app: &AppHandle, text: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        app.run_on_main_thread(move || {
            let clipboard = gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD);
            clipboard.set_text(&text);
            clipboard.store();
        })
        .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, text);
        Err("copying is only implemented on Linux".into())
    }
}

fn build_tray(app: &tauri::App, sink: &AppSink) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Start dictation", false, None::<&str>)?;
    let cancel = MenuItem::with_id(app, "cancel", "Cancel", false, None::<&str>)?;
    let hub = MenuItem::with_id(app, "hub", "Open hub", true, None::<&str>)?;
    let doctor = MenuItem::with_id(app, "doctor", "Run doctor", true, None::<&str>)?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        "Quit UI (daemon keeps running)",
        true,
        None::<&str>,
    )?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &cancel,
            &PredefinedMenuItem::separator(app)?,
            &hub,
            &doctor,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let tray = TrayIconBuilder::with_id("dictate")
        .icon(tauri::image::Image::new_owned(
            icons::rgba(TrayState::Disconnected),
            icons::SIZE,
            icons::SIZE,
        ))
        .tooltip(TrayState::Disconnected.tooltip())
        .menu(&menu)
        .on_menu_event(|app, event| {
            let bridge = app.state::<Bridge>().inner().clone();
            match event.id().as_ref() {
                "toggle" => {
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = bridge.request(Command::Toggle).await {
                            eprintln!("dictate-ui: toggle failed: {e}");
                        }
                    });
                }
                "cancel" => {
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = bridge.request(Command::Cancel).await {
                            eprintln!("dictate-ui: cancel failed: {e}");
                        }
                    });
                }
                "hub" => {
                    let _ = open_hub(app, None);
                }
                "doctor" => {
                    let _ = open_hub(app, Some("doctor"));
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .build(app)?;
    if let Ok(mut slot) = sink.tray.lock() {
        *slot = Some(TrayHandles {
            tray,
            toggle,
            cancel,
        });
    }
    Ok(())
}

/// Hub pages `--hub <page>` accepts. Mirrors `PAGES` in `ui/src/hub/App.tsx`.
pub const HUB_PAGES: &[&str] = &[
    "home",
    "history",
    "notes",
    "dictionary",
    "settings",
    "doctor",
];

/// What the command line asks for at start: `None` to start in the tray,
/// `Some(page)` to open the hub (`--hub`, optionally followed by a page).
///
/// # Errors
///
/// An unknown page or argument, with a usage line.
pub fn start_page<I: IntoIterator<Item = String>>(args: I) -> Result<Option<String>, String> {
    let mut args = args.into_iter().peekable();
    let mut page = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--hub" => {
                let next = args.next_if(|a| !a.starts_with("--"));
                let p = next.unwrap_or_else(|| "home".to_string());
                if !HUB_PAGES.contains(&p.as_str()) {
                    return Err(format!(
                        "unknown hub page '{p}'; expected one of {}",
                        HUB_PAGES.join(", ")
                    ));
                }
                page = Some(p);
            }
            other => {
                return Err(format!(
                    "unknown argument '{other}'\nusage: dictate-ui [--hub [{}]]",
                    HUB_PAGES.join("|")
                ))
            }
        }
    }
    Ok(page)
}

/// Run the desktop UI.
///
/// # Panics
///
/// If Tauri cannot start (no display).
pub fn run() {
    let start = match start_page(std::env::args().skip(1)) {
        Ok(page) => page,
        Err(message) => {
            eprintln!("dictate-ui: {message}");
            std::process::exit(2);
        }
    };
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            crate::commands::connection_state,
            crate::commands::reconnect,
            crate::commands::toggle,
            crate::commands::start_dictation,
            crate::commands::stop,
            crate::commands::cancel,
            crate::commands::get_status,
            crate::commands::get_config,
            crate::commands::set_config,
            crate::commands::list_dictionary,
            crate::commands::list_dictionary_suggestions,
            crate::commands::upsert_dictionary_entry,
            crate::commands::delete_dictionary_entry,
            crate::commands::list_notes,
            crate::commands::delete_note,
            crate::commands::query_history,
            crate::commands::get_history_analytics,
            crate::commands::purge_history,
            crate::commands::diagnose,
            crate::commands::copy_text,
            crate::commands::last_session_event,
            crate::commands::open_hub,
        ])
        .setup(move |app| {
            let sink = Arc::new(AppSink::new(app.handle().clone()));
            app.manage(sink.clone());
            create_hud(app)?;
            let tray_ok = match build_tray(app, &sink) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("dictate-ui: no system tray ({e}); opening the hub instead");
                    false
                }
            };
            let (bridge, task) = Bridge::new(BridgeConfig::default(), sink.clone());
            app.manage(bridge);
            tauri::async_runtime::spawn(task);
            tauri::async_runtime::spawn(sink.hud_timer());
            if start.is_some() || !tray_ok {
                open_hub(app.handle(), start.as_deref())?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start the dictate UI")
        .run(|_app, event| {
            // Closing the hub must not quit the app: the tray and the Flow bar
            // outlive it. Only "Quit UI" exits.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::start_page;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn no_arguments_start_in_the_tray() {
        assert_eq!(start_page(args(&[])), Ok(None));
    }

    #[test]
    fn hub_opens_home_or_the_named_page() {
        assert_eq!(start_page(args(&["--hub"])), Ok(Some("home".into())));
        assert_eq!(
            start_page(args(&["--hub", "settings"])),
            Ok(Some("settings".into()))
        );
        assert_eq!(
            start_page(args(&["--hub", "notes"])),
            Ok(Some("notes".into()))
        );
    }

    #[test]
    fn unknown_pages_and_arguments_are_refused() {
        assert!(start_page(args(&["--hub", "snippets"])).is_err());
        assert!(start_page(args(&["--tray"])).is_err());
    }
}
