// Old-version cleanup. Runs once at startup to keep the machine tidy as the
// helper updates itself over time.
//
// Every action is deliberately conservative: strict filename patterns that
// only ever match Redstars Helper's own release artefacts, version-gated so
// only STRICTLY-OLDER files are touched, never the file/entry currently in
// use, and every removal is logged. Each sweep is best-effort and isolated —
// a failure in one can never block startup or affect the others.
//
// Targets (all explicitly opted into by the user):
//   1. Stale AppImage siblings next to the running AppImage
//   2. Cruft in the per-user data dir (delegated to script_updater)
//   3. Old downloaded .deb/.rpm helper packages in the Downloads dir
//   4. Dead XDG autostart entries left by previous installs (Linux)

use std::fs;
use std::path::PathBuf;

use tauri::AppHandle;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run every sweep. Best-effort; never panics.
pub fn run(app: &AppHandle) {
    // 2. Data-dir cruft (refs/ iso/ *.tmp). Owned by script_updater.
    crate::script_updater::cleanup_runtime_state(app);

    // 1 + 3. Stale install artefacts.
    sweep_stale_appimages();
    sweep_old_packages();

    // 4. Dead autostart entries (Linux only).
    #[cfg(target_os = "linux")]
    sweep_dead_autostart();
}

/// Parse "1.2.3" → (1,2,3); missing minor/patch default to 0. Any non-numeric
/// component → None (so the caller refuses to act on an unparseable name).
fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let mut it = s.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next().map(|p| p.parse()).transpose().ok()?.unwrap_or(0);
    let c = it.next().map(|p| p.parse()).transpose().ok()?.unwrap_or(0);
    Some((a, b, c))
}

/// True only when both versions parse AND `v` is strictly older than the
/// running build. Unparseable → false (never delete what we can't reason about).
fn is_older_than_current(v: &str) -> bool {
    match (parse_version(v), parse_version(CURRENT_VERSION)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// Version embedded in a Redstars Helper AppImage filename, e.g.
/// "Redstars.Helper_0.2.9_amd64.AppImage" → "0.2.9". None if it doesn't match.
fn appimage_version(name: &str) -> Option<String> {
    let rest = name.strip_prefix("Redstars.Helper_")?.strip_suffix(".AppImage")?;
    let ver = rest.split('_').next()?; // "0.2.9_amd64" → "0.2.9"
    parse_version(ver).map(|_| ver.to_string())
}

/// Version embedded in a Redstars Helper .deb/.rpm filename:
///   "Redstars.Helper_0.2.9_amd64.deb"      → "0.2.9"
///   "Redstars.Helper-0.2.9-1.x86_64.rpm"   → "0.2.9"
fn package_version(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix("Redstars.Helper_") {
        let rest = rest.strip_suffix(".deb")?;
        let ver = rest.split('_').next()?;
        return parse_version(ver).map(|_| ver.to_string());
    }
    if let Some(rest) = name.strip_prefix("Redstars.Helper-") {
        let rest = rest.strip_suffix(".rpm")?;
        let ver = rest.split('-').next()?;
        return parse_version(ver).map(|_| ver.to_string());
    }
    None
}

fn remove_logged(path: &std::path::Path, what: &str) {
    match fs::remove_file(path) {
        Ok(()) => eprintln!("[cleanup] removed {} {}", what, path.display()),
        Err(e) => eprintln!("[cleanup] could not remove {}: {}", path.display(), e),
    }
}

/// Delete older-version AppImage files sitting in the same directory as the
/// running AppImage. No-op unless we're actually running from an AppImage:
/// the runtime exports $APPIMAGE = absolute path to the .AppImage file.
fn sweep_stale_appimages() {
    let current = match std::env::var("APPIMAGE") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => return,
    };
    let current = current.canonicalize().unwrap_or(current);
    let dir = match current.parent() {
        Some(d) => d.to_path_buf(),
        None => return,
    };
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if let Some(ver) = appimage_version(name) {
            if is_older_than_current(&ver) {
                remove_logged(&path, "stale AppImage");
            }
        }
    }
}

/// Delete OLD downloaded .deb/.rpm helper packages. Strict: only filenames
/// that exactly match the helper package naming AND carry a version older than
/// the running one are touched — every other file in Downloads is left alone.
fn sweep_old_packages() {
    for dir in download_dirs() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if let Some(ver) = package_version(name) {
                if is_older_than_current(&ver) {
                    remove_logged(&path, "old package");
                }
            }
        }
    }
}

fn download_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("XDG_DOWNLOAD_DIR") {
        if !d.is_empty() {
            v.push(PathBuf::from(d));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let d = PathBuf::from(&home).join("Downloads");
        if !v.contains(&d) {
            v.push(d);
        }
    }
    v
}

/// Remove XDG autostart .desktop entries that belong to Redstars Helper but
/// whose Exec target no longer exists — leftovers from a previous install at a
/// path that's since been removed (incl. an AppImage we just deleted above).
///
/// We only ever delete entries whose binary is GONE. A duplicate that still
/// points at a live binary is left untouched — guessing which of two live
/// entries is "canonical" is not worth the risk of disabling autostart. The
/// autostart plugin's own (live, current) entry is therefore always kept.
#[cfg(target_os = "linux")]
fn sweep_dead_autostart() {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let dir = PathBuf::from(home).join(".config/autostart");
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        // Only consider entries that are clearly ours.
        let refers_to_us = content.lines().any(|l| {
            let l = l.to_lowercase();
            (l.starts_with("exec=") || l.starts_with("name=") || l.starts_with("tryexec="))
                && (l.contains("redstars-helper")
                    || l.contains("redstars helper")
                    || l.contains("fr.redlinks.redstars-helper"))
        });
        if !refers_to_us {
            continue;
        }
        // Resolve the Exec/TryExec target binary.
        let target = content
            .lines()
            .find_map(|l| l.strip_prefix("Exec=").or_else(|| l.strip_prefix("TryExec=")))
            .and_then(|cmd| cmd.split_whitespace().next())
            .map(PathBuf::from);
        let dead = matches!(target, Some(ref t) if !t.exists());
        if dead {
            remove_logged(&path, "dead autostart entry");
        }
    }
}
