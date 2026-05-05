// Library entry — main.rs delegates to run() so cargo can build both a
// binary (Linux/Mac) and a static lib (mobile platforms).

use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_opener::OpenerExt;

const HELPER_URL: &str = "http://localhost:8080/";

struct HelperProc(Mutex<Option<Child>>);

fn spawn_helper(app: &AppHandle) -> Option<Child> {
    let resource = app
        .path()
        .resolve("helper.py", tauri::path::BaseDirectory::Resource)
        .ok()?;
    let py = which_python();
    eprintln!("[helper] launching {} {}", py, resource.display());
    Command::new(py).arg("-u").arg(resource).spawn().ok()
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
fn restart_helper(app: AppHandle, proc: State<HelperProc>) -> Result<(), String> {
    let mut g = proc.0.lock().unwrap();
    if let Some(mut c) = g.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    *g = spawn_helper(&app);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(HelperProc(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![open_demo, restart_helper])
        .setup(|app| {
            let handle = app.handle().clone();
            let child = spawn_helper(&handle);
            *app.state::<HelperProc>().0.lock().unwrap() = child;

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
                .tooltip("Redstars Helper — http://localhost:8080")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => { let _ = app.opener().open_url(HELPER_URL, None::<&str>); }
                    "restart" => {
                        let s = app.state::<HelperProc>();
                        let mut g = s.0.lock().unwrap();
                        if let Some(mut c) = g.take() { let _ = c.kill(); let _ = c.wait(); }
                        *g = spawn_helper(app);
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
