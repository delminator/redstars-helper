// Auto-updating helper script loader.
//
// The Tauri shell embeds Python orchestration but the actual helper logic
// (HTTP endpoints, USB device probing, Firefox prefs writing) lives in a
// standalone helper.py that gets pulled fresh from a public GitHub release
// on every startup + every 6 hours in the background. The shell itself
// changes rarely; the script ships independently and reaches users without
// a reinstall.
//
// Resolution order for the script path each time `current_script_path` is
// called:
//
//   1. `REDSTARS_HELPER_LOCAL=/path/to/helper.py` env var
//      → dev override, bypasses everything. The path is used as-is.
//   2. Cached copy at `<data-dir>/helper.py`
//      → set by `fetch_remote()` once the GitHub copy is verified.
//   3. Bundled fallback `<resources>/helper.py`
//      → ships in the .deb / .rpm / .AppImage / .dmg / .msi as a last
//        resort when GitHub is unreachable on first launch.
//
// Verification: every download passes through `verify_minisign` before
// being written to the cache. The pubkey is hardcoded in this module
// (same key as the Tauri updater so we only manage one signing identity).
// A failed signature is a hard reject — we keep the previous cached copy
// rather than fall back to bundled (a bad signature is a corruption /
// tampering signal, falling back silently would mask it).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Manager};

const DEFAULT_API_BASE: &str = "https://dev.redstars.redlinks.fr";
const SCRIPT_NAME: &str = "helper.py";
const SHELL_VERSION: &str = env!("CARGO_PKG_VERSION");
const FETCH_TIMEOUT_SECS: u64 = 8;

/// Minisign pubkey for verifying helper.py signatures. Same key as the
/// Tauri updater — when the user wires a new release we sign the script
/// alongside the .deb/.rpm/.dmg/.msi with the same private key.
const HELPER_SCRIPT_PUBKEY: &str = include_str!("script_updater_pubkey.txt");

#[derive(Debug, Deserialize, Clone)]
pub struct ScriptManifest {
    pub name: String,
    pub version: String,
    pub script_url: String,
    pub signature_url: String,
    pub sha256: String,
    pub size: i64,
    pub min_shell: String,
    pub updated_at: String,
}

/// Compare two semver strings — returns Ordering::Greater if `a > b`.
fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let pa: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    for i in 0..3 {
        let na = pa.get(i).copied().unwrap_or(0);
        let nb = pb.get(i).copied().unwrap_or(0);
        if na != nb {
            return na.cmp(&nb);
        }
    }
    std::cmp::Ordering::Equal
}

/// Returns the path the helper script should be loaded from RIGHT NOW.
/// Never returns None — we always fall back to the bundled copy. If the
/// bundle is missing too (broken install), returns the cache path even
/// though it doesn't exist yet so the caller logs a useful error.
pub fn current_script_path(app: &AppHandle) -> PathBuf {
    // 1. Dev override
    if let Ok(local) = env::var("REDSTARS_HELPER_LOCAL") {
        let p = PathBuf::from(local);
        if p.is_file() {
            eprintln!("[script-updater] using REDSTARS_HELPER_LOCAL={}", p.display());
            return p;
        }
        eprintln!("[script-updater] REDSTARS_HELPER_LOCAL is set but file is missing: {}", p.display());
    }
    // 2. Cached copy from a previous successful fetch
    if let Some(p) = cache_path(app) {
        if p.is_file() {
            return p;
        }
    }
    // 3. Bundled fallback
    if let Ok(p) = app.path().resolve(SCRIPT_NAME, tauri::path::BaseDirectory::Resource) {
        if p.is_file() {
            return p;
        }
    }
    // 4. Last-resort path (caller logs)
    cache_path(app).unwrap_or_else(|| PathBuf::from(SCRIPT_NAME))
}

/// Path to the cached script in the OS-appropriate per-user data dir.
fn cache_path(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_local_data_dir().ok()?;
    fs::create_dir_all(&dir).ok()?;
    Some(dir.join(SCRIPT_NAME))
}

/// Fetch the latest manifest from the backend. Short timeout so a slow
/// API call doesn't delay app startup.
fn fetch_manifest() -> Result<ScriptManifest, String> {
    let api = env::var("REDSTARS_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
    let url = format!("{}/api/v1/agents/script-latest?name={}", api, SCRIPT_NAME);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("http client: {}", e))?;
    let resp = client.get(&url).send().map_err(|e| format!("fetch manifest: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("manifest http {}", resp.status()));
    }
    resp.json::<ScriptManifest>().map_err(|e| format!("manifest parse: {}", e))
}

/// Download `url` to a Vec<u8>, with the same short timeout as the manifest.
fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS * 2))
        .build()
        .map_err(|e| format!("http client: {}", e))?;
    let resp = client.get(url).send().map_err(|e| format!("fetch {}: {}", url, e))?;
    if !resp.status().is_success() {
        return Err(format!("http {} on {}", resp.status(), url));
    }
    resp.bytes().map(|b| b.to_vec()).map_err(|e| format!("read body: {}", e))
}

/// SHA-256 hex digest of `bytes`. Lightweight implementation via the
/// `sha2` crate (added to Cargo.toml dependencies).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    let r = h.finalize();
    let mut s = String::with_capacity(64);
    for b in r.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Verify `script_bytes` against `signature_bytes` using the bundled
/// minisign pubkey. Returns Ok(()) only when the signature checks out.
fn verify_minisign(script_bytes: &[u8], signature_bytes: &[u8]) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};
    let pk = PublicKey::decode(HELPER_SCRIPT_PUBKEY.trim())
        .map_err(|e| format!("invalid bundled pubkey: {:?}", e))?;
    let sig_str = std::str::from_utf8(signature_bytes)
        .map_err(|_| "signature is not valid UTF-8".to_string())?;
    let sig = Signature::decode(sig_str)
        .map_err(|e| format!("parse signature: {:?}", e))?;
    pk.verify(script_bytes, &sig, false)
        .map_err(|e| format!("signature mismatch: {:?}", e))
}

/// Attempt one fetch + verify + cache write. Idempotent. Returns true
/// only when a NEW version was actually written (caller can decide to
/// restart the helper subprocess).
pub fn fetch_and_cache(app: &AppHandle) -> bool {
    let manifest = match fetch_manifest() {
        Ok(m) => m,
        Err(e) => { eprintln!("[script-updater] manifest fetch failed: {}", e); return false; }
    };

    // Min-shell version gate — refuse to load a script that needs a
    // newer shell capability than we have.
    if semver_cmp(&manifest.min_shell, SHELL_VERSION) == std::cmp::Ordering::Greater {
        eprintln!(
            "[script-updater] skip: script needs shell >= {} but we are {}",
            manifest.min_shell, SHELL_VERSION
        );
        return false;
    }

    // Skip download if the cached version is already current. The cache
    // path's "version" is recorded as a sidecar JSON next to it.
    let cache = match cache_path(app) { Some(p) => p, None => return false };
    let version_marker = cache.with_extension("version");
    if let Ok(prev) = fs::read_to_string(&version_marker) {
        if prev.trim() == manifest.version {
            return false; // already up-to-date
        }
    }

    eprintln!("[script-updater] fetching {} v{}", manifest.name, manifest.version);
    let script_bytes = match fetch_bytes(&manifest.script_url) {
        Ok(b) => b,
        Err(e) => { eprintln!("[script-updater] script fetch: {}", e); return false; }
    };
    let sig_bytes = match fetch_bytes(&manifest.signature_url) {
        Ok(b) => b,
        Err(e) => { eprintln!("[script-updater] signature fetch: {}", e); return false; }
    };

    // SHA-256 sanity (manifest could be a typo). The signature is the
    // authoritative check; this is just an extra layer.
    if !manifest.sha256.is_empty() {
        let got = sha256_hex(&script_bytes);
        if got != manifest.sha256 {
            eprintln!(
                "[script-updater] sha256 mismatch: expected {}, got {}",
                manifest.sha256, got
            );
            return false;
        }
    }

    // Signature is the real gate. Bad signature → REJECT, keep previous.
    if let Err(e) = verify_minisign(&script_bytes, &sig_bytes) {
        eprintln!("[script-updater] signature verify failed: {} — KEEPING previous cache", e);
        return false;
    }

    // Atomic-ish write: tmp file → rename.
    let tmp = cache.with_extension("tmp");
    if let Err(e) = fs::write(&tmp, &script_bytes) {
        eprintln!("[script-updater] write tmp: {}", e);
        return false;
    }
    if let Err(e) = fs::rename(&tmp, &cache) {
        eprintln!("[script-updater] rename: {}", e);
        return false;
    }
    if let Err(e) = fs::write(&version_marker, &manifest.version) {
        eprintln!("[script-updater] write version marker: {}", e);
    }

    eprintln!("[script-updater] cached v{} at {}", manifest.version, cache.display());
    true
}

/// Spawn a daemon thread that re-runs `fetch_and_cache` every 6 hours.
/// On a new version, calls `on_update` so the caller can restart the
/// helper subprocess.
pub fn start_background_poller<F>(app: AppHandle, on_update: F)
where
    F: Fn(&AppHandle) + Send + 'static,
{
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(6 * 3600));
            let updated = fetch_and_cache(&app);
            if updated {
                eprintln!("[script-updater] background update applied — restarting helper");
                on_update(&app);
            }
        }
    });
}
