// Library entry — main.rs delegates to run() so cargo can build both a
// binary (Linux/Mac) and a static lib (mobile platforms).

mod script_updater;

use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_opener::OpenerExt;

const HELPER_URL: &str = "http://localhost:49080/";

struct HelperProc(Mutex<Option<Child>>);

/// Spawn helper.py from whichever copy is currently authoritative —
/// dev-override > cached fetched from GitHub > bundled fallback. See
/// `script_updater::current_script_path` for the resolution order.
fn spawn_helper(app: &AppHandle) -> Option<Child> {
    let script = script_updater::current_script_path(app);
    let py = which_python();
    eprintln!("[helper] launching {} {}", py, script.display());
    Command::new(py).arg("-u").arg(&script).spawn().ok()
}

/// Restart helper subprocess in place (kill + respawn). Used by the
/// background updater after a new script version is cached, and by the
/// tray "Restart helper" menu item.
fn restart_helper_proc(app: &AppHandle) {
    let s = app.state::<HelperProc>();
    let mut g = s.0.lock().unwrap();
    if let Some(mut c) = g.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    *g = spawn_helper(app);
}

#[cfg(unix)]
fn which_python() -> &'static str { "python3" }
#[cfg(windows)]
fn which_python() -> &'static str { "python" }

#[tauri::command]
fn open_demo(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(HELPER_URL, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
fn restart_helper(app: AppHandle, _proc: State<HelperProc>) -> Result<(), String> {
    restart_helper_proc(&app);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Autostart at session login (XDG autostart / login items / registry
        // depending on OS). We pass `--minimized` so the window stays hidden
        // and the user only sees the tray icon — same UX as on a manual
        // launch via the tray.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(HelperProc(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![open_demo, restart_helper])
        .setup(|app| {
            let handle = app.handle().clone();

            // First-run autostart enable. The plugin's `is_enabled` /
            // `enable` are idempotent; calling enable() on every run
            // is harmless if it's already set. We never DISABLE it
            // automatically — that requires explicit user action in
            // the helper's settings / tray menu.
            use tauri_plugin_autostart::ManagerExt;
            if let Ok(am) = handle.autolaunch().is_enabled() {
                if !am {
                    let _ = handle.autolaunch().enable();
                    eprintln!("[autostart] enabled — helper will auto-launch at next session login");
                }
            }

            // Pull the latest helper.py from GitHub before we spawn it.
            // Sync call with a short timeout — if GitHub is slow we fall
            // back to the cached or bundled copy and the background
            // poller will catch up later.
            let _ = script_updater::fetch_and_cache(&handle);

            let child = spawn_helper(&handle);
            *app.state::<HelperProc>().0.lock().unwrap() = child;

            // Background poller: every 6 h, fetch + verify + cache. On
            // a successful update, kill the running python3 subprocess
            // and respawn it pointing at the new script.
            script_updater::start_background_poller(handle.clone(), |app| {
                restart_helper_proc(app);
            });

            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "open", "Open demo", true, None::<&str>)?,
                    &MenuItem::with_id(app, "restart", "Restart helper", true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?,
                ],
            )?;
            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("Redstars Helper — http://localhost:49080")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => { let _ = app.opener().open_url(HELPER_URL, None::<&str>); }
                    "restart" => {
                        // Pull a fresh copy first, then restart with whichever
                        // is now current (cached vs bundled fallback).
                        let _ = script_updater::fetch_and_cache(app);
                        restart_helper_proc(app);
                    }
                    "quit" => { app.exit(0); }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                window.hide().ok();
                api.prevent_close();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let s = app.state::<HelperProc>();
                let mut g = s.0.lock().unwrap();
                if let Some(mut c) = g.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        });
}
