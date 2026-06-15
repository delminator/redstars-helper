// Shell (Tauri app) auto-updater.
//
// Mirrors `script_updater`'s cadence — a check at startup + every 6 h — but
// for the app binary itself, via the Tauri updater plugin (latest.json on
// GitHub Releases, minisign-verified with the pubkey in tauri.conf.json).
//
// This logic used to live in the frontend (src/index.html) and ran in the
// webview. When the helper became a windowless tray agent there is no webview
// to run it, so it moved here and runs from the Rust setup hook. Bonus: no
// runtime dependency on esm.sh to even load the updater code.
//
// Linux note: only the AppImage build can self-replace. On a .deb/.rpm
// install `download_and_install` returns an error (the install is owned by
// the system package manager) — we log it and keep running on the current
// shell. macOS (.dmg/.app) and Windows (.msi) self-update normally.

use std::time::Duration;

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

const POLL_INTERVAL_SECS: u64 = 6 * 3600;

/// One check → download → install → restart cycle. Async work is spawned onto
/// the Tauri runtime so it never blocks startup or the tray event loop.
pub fn check_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let updater = match app.updater() {
            Ok(u) => u,
            Err(e) => {
                eprintln!("[shell-updater] updater unavailable: {}", e);
                return;
            }
        };
        match updater.check().await {
            Ok(Some(update)) => {
                eprintln!(
                    "[shell-updater] update available v{} (current v{}) — installing",
                    update.version,
                    env!("CARGO_PKG_VERSION")
                );
                match update.download_and_install(|_chunk, _total| {}, || {}).await {
                    Ok(()) => {
                        eprintln!("[shell-updater] installed v{} — restarting", update.version);
                        app.restart();
                    }
                    Err(e) => {
                        // Expected on .deb/.rpm (package-manager-owned install).
                        eprintln!("[shell-updater] install failed (normal on .deb/.rpm): {}", e);
                    }
                }
            }
            Ok(None) => eprintln!("[shell-updater] up-to-date"),
            Err(e) => eprintln!("[shell-updater] check failed: {}", e),
        }
    });
}

/// Background poller: re-check every 6 h. A long-running agent picks up new
/// shell releases without waiting for a manual restart.
pub fn start_poller(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
        check_now(&app);
    });
}
