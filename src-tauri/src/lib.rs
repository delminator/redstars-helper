// Library entry — main.rs delegates to run() so cargo can build both a
// binary (Linux/Mac) and a static lib (mobile platforms).

mod cleanup;
mod script_updater;
mod shell_updater;

use std::process::{Child, Command};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;

const HELPER_URL: &str = "http://localhost:49080/";

struct HelperProc(Mutex<Option<Child>>);

/// Spawn helper.py from whichever copy is currently authoritative —
/// dev-override > cached fetched from GitHub > bundled fallback. See
/// `script_updater::current_script_path` for the resolution order.
///
/// Python lookup tries multiple candidates in order: pyenv shims first
/// (some users override system python that way, and desktop-app spawn
/// inherits a stripped PATH without ~/.pyenv/shims), then a bare name
/// for PATH resolution, then common system locations. First successful
/// spawn wins.
fn spawn_helper(app: &AppHandle) -> Option<Child> {
    let script = script_updater::current_script_path(app);
    for py in python_candidates() {
        // Absolute path: cheap existence check before fork.
        if py.starts_with('/') && !std::path::Path::new(&py).is_file() {
            continue;
        }
        eprintln!("[helper] trying {} {}", py, script.display());
        if let Ok(child) = Command::new(&py).arg("-u").arg(&script).spawn() {
            eprintln!("[helper] spawned via {}", py);
            return Some(child);
        }
    }
    eprintln!("[helper] no usable python found; user must install python3");
    None
}

/// Make this process the ONLY helper on the machine: terminate any
/// other running helper shell and any orphan `helper.py` a previous
/// shell may have left holding port 49080.
///
/// Newest-wins by construction: we enumerate the process table once,
/// at startup, so we only ever target processes that already existed
/// before us. A helper launched *after* us isn't in our snapshot — and
/// when it boots it runs this same sweep and retires us in turn. That
/// avoids two concurrently-starting shells mutually SIGKILLing each
/// other (the realistic launches — login autostart, a tray click, the
/// dashboard's `redstars-helper://` button — are never simultaneous to
/// the millisecond anyway).
///
/// We must run this BEFORE spawning our own python child, so the port
/// is free when our helper.py binds it. We match two shapes, mirroring
/// `scripts/preinst.sh`: the shell binary (by exe file name, robust to
/// the 15-char `comm` truncation on Linux) and any python whose command
/// line references `helper.py`.
fn terminate_other_helpers() {
    use sysinfo::{ProcessesToUpdate, System};

    let self_pid = match sysinfo::get_current_pid() {
        Ok(p) => p,
        Err(_) => return,
    };
    let our_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    for (pid, proc_) in sys.processes() {
        if *pid == self_pid {
            continue;
        }
        let name = proc_.name().to_string_lossy().to_string();
        let exe_name = proc_
            .exe()
            .and_then(|e| e.file_name())
            .map(|n| n.to_string_lossy().to_string());
        let is_shell = our_name.is_some()
            && (our_name.as_deref() == Some(name.as_str()) || our_name == exe_name);
        let is_helper_py = proc_
            .cmd()
            .iter()
            .any(|a| a.to_string_lossy().ends_with("helper.py"));
        if is_shell || is_helper_py {
            eprintln!("[single] retiring prior helper pid={} ({})", pid, name);
            proc_.kill();
        }
    }
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

/// Ordered python-interpreter candidates. Pyenv shims first so users
/// who manage python via pyenv don't get a stale system python when
/// the desktop app's spawn environment lacks ~/.pyenv/shims in PATH.
#[cfg(unix)]
fn python_candidates() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        v.push(format!("{}/.pyenv/shims/python3", home));
        v.push(format!("{}/.pyenv/shims/python", home));
    }
    v.push("python3".to_string());            // PATH lookup
    v.push("/usr/bin/python3".to_string());
    v.push("/usr/local/bin/python3".to_string());
    v.push("/opt/homebrew/bin/python3".to_string());  // mac arm64
    v.push("/opt/local/bin/python3".to_string());     // macports
    v
}
#[cfg(windows)]
fn python_candidates() -> Vec<String> {
    vec![
        "python".to_string(),
        "python3".to_string(),
        "py".to_string(),
    ]
}

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
        // depending on OS). The helper is a windowless tray agent — there's
        // nothing to show on launch, it just appears in the system tray.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(HelperProc(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![open_demo, restart_helper])
        .setup(|app| {
            let handle = app.handle().clone();

            // Windowless tray agent: on macOS hide the Dock icon so we are a
            // pure background item (no-op / not present on Linux + Windows).
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // Single-instance sweep: retire any other helper shell and
            // free port 49080 from an orphan helper.py before we spawn
            // ours. Newest launch wins; see terminate_other_helpers.
            terminate_other_helpers();

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

            // Boot-time cleanup: data-dir cruft + stale old-version artefacts
            // (old AppImage siblings, old downloaded packages, dead autostart
            // entries). Runs AFTER the autostart enable above so the current,
            // live entry is never mistaken for a leftover. Conservative and
            // best-effort — see cleanup.rs.
            cleanup::run(&handle);

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

            // Shell self-update (the Tauri binary itself): check now + every
            // 6 h. Moved out of the now-removed webview into Rust so this
            // windowless tray agent keeps itself current. See shell_updater.rs.
            shell_updater::check_now(&handle);
            shell_updater::start_poller(handle.clone());

            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "open", "Open demo", true, None::<&str>)?,
                    &MenuItem::with_id(app, "restart", "Restart helper", true, None::<&str>)?,
                    &MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?,
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
                    "update" => { shell_updater::check_now(app); }
                    "quit" => { app.exit(0); }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri app")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { code, api, .. } = event {
                // Tray-only agent: there are no windows, so a code-less exit
                // request is spurious (e.g. a stray "last window closed") —
                // keep running. An explicit quit from the tray uses
                // app.exit(0) (code = Some) and is allowed through, tearing
                // down the helper subprocess on the way out.
                if code.is_none() {
                    api.prevent_exit();
                    return;
                }
                let s = app.state::<HelperProc>();
                let mut g = s.0.lock().unwrap();
                if let Some(mut c) = g.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        });
}
