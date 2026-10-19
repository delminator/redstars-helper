//! Images ISO montées pour le tableau de bord (portage de make_iso / mount_iso /
//! unmount_iso / list_mount et des routes /helper/iso/*).
//!
//! Montage en espace utilisateur, sans sudo :
//!   Linux   → udisksctl loop-setup + udisksctl mount   (Polkit, session)
//!   macOS   → hdiutil attach
//!   Windows → powershell Mount-DiskImage

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use super::proc::{self, argv};
use super::py::{self, jobj, J};
use super::Reply;

pub struct Mounted {
    pub iso_path: String,
    pub mount_path: String,
    pub dev: String,
    pub label: String,
}

/// iso_id → montage actif.
pub static MOUNTED: LazyLock<Mutex<HashMap<String, Mounted>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn mounted() -> std::sync::MutexGuard<'static, HashMap<String, Mounted>> {
    MOUNTED.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn iso_cache_dir() -> PathBuf {
    home().join(".cache").join("redstars-helper").join("iso")
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Listing léger d'un dossier pour le cadre du tableau de bord.
pub fn list_mount(mount_path: &str) -> J {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(mount_path) {
        let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for name in names {
            let full = Path::new(mount_path).join(&name);
            let Ok(st) = std::fs::metadata(&full) else { continue };
            out.push(jobj! {
                "name" => name,
                "path" => full.to_string_lossy().into_owned(),
                "size" => st.len(),
                "is_dir" => st.is_dir(),
            });
        }
    }
    J::Arr(out)
}

pub enum Payload {
    None,
    Bytes(Vec<u8>),
    Files(Vec<(String, Vec<u8>)>),
}

/// Construit une image ISO 9660 (+Rock Ridge/Joliet) ; rend son chemin.
fn make_iso(label: &str, payload: Payload) -> Result<PathBuf, String> {
    let dir = iso_cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| py::oserror_full(&e, &[&dir.to_string_lossy()]))?;
    let iso_id = new_id();
    let iso_path = dir.join(format!("{iso_id}.iso"));
    let src_dir = dir.join(format!("src-{iso_id}"));
    std::fs::create_dir(&src_dir).map_err(|e| py::oserror_full(&e, &[&src_dir.to_string_lossy()]))?;
    let res = (|| {
        let write = |p: PathBuf, d: &[u8]| std::fs::write(&p, d).map_err(|e| py::oserror_full(&e, &[&p.to_string_lossy()]));
        match payload {
            Payload::None => {}
            Payload::Bytes(b) => write(src_dir.join("payload.bin"), &b)?,
            Payload::Files(files) => {
                for (name, data) in files {
                    let safe = py::path_name(&name); // retire les composants de chemin
                    if safe.is_empty() {
                        continue;
                    }
                    write(src_dir.join(safe), &data)?;
                }
            }
        }
        let tool = ["xorrisofs", "genisoimage", "mkisofs"]
            .into_iter()
            .find(|t| proc::which(t))
            .ok_or("RuntimeError: No ISO tool (install xorriso / genisoimage / mkisofs).")?;
        let cut: String = label.chars().take(32).collect();
        let safe_label = match py::strip(&cut) {
            "" => "REDSTARS",
            s => s,
        };
        let cmd = argv(&[
            tool,
            "-iso-level",
            "3",
            "-R",
            "-J",
            "-no-pad",
            "-V",
            safe_label,
            "-o",
            &iso_path.to_string_lossy(),
            &src_dir.to_string_lossy(),
        ]);
        let r = proc::run(&cmd, None).map_err(|e| e.msg())?;
        if r.code != 0 {
            let err: String = py::strip(&r.stderr).chars().take(500).collect();
            return Err(format!("RuntimeError: {tool} failed: {err}"));
        }
        Ok(iso_path.clone())
    })();
    let _ = std::fs::remove_dir_all(&src_dir);
    res
}

const INDEX_ERR: &str = "IndexError: list index out of range";

/// Monte l'image ; rend (mount_path, dev).
fn mount_iso(iso_path: &Path) -> Result<(String, String), String> {
    let iso = iso_path.to_string_lossy().into_owned();
    if cfg!(target_os = "macos") {
        let out = proc::run_check(&argv(&["hdiutil", "attach", "-nobrowse", &iso]))?.stdout;
        // Dernière ligne non vide : "/dev/disk2   Apple_HFS    /Volumes/REDSTARS"
        let last = py::splitlines(py::strip(&out)).into_iter().rfind(|l| !py::strip(l).is_empty()).ok_or(INDEX_ERR)?;
        static SPLIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}|\t").expect("split"));
        let parts: Vec<&str> = SPLIT.split(py::strip(last)).collect();
        let dev = py::strip(parts[0]).to_string();
        let mount_path = py::strip(parts[parts.len() - 1]).to_string();
        return Ok((mount_path, dev));
    }
    if cfg!(windows) {
        let ps = format!(
            "$img = Mount-DiskImage -ImagePath '{iso}' -PassThru; Start-Sleep -Milliseconds 250; ($img | Get-Volume).DriveLetter"
        );
        let out = proc::run_check(&argv(&["powershell", "-NoProfile", "-Command", &ps]))?.stdout;
        let d = py::splitlines(py::strip(&out)).last().map(|s| py::strip(s).to_string()).ok_or(INDEX_ERR)?;
        return Ok((format!("{d}:\\"), iso)); // dev = chemin de l'iso (pour Dismount-DiskImage)
    }
    if cfg!(any(target_os = "linux", target_os = "android")) {
        let out = proc::run_check(&argv(&["udisksctl", "loop-setup", "-f", &iso, "--no-user-interaction"]))?.stdout;
        // "Mapped file file.iso as /dev/loop0."
        let dev = py::strip(&out)
            .split(py::is_py_space)
            .filter(|s| !s.is_empty())
            .last()
            .ok_or(INDEX_ERR)?
            .trim_end_matches('.')
            .to_string();
        let m = proc::run_check(&argv(&["udisksctl", "mount", "-b", &dev, "--no-user-interaction"]))?.stdout;
        // "Mounted /dev/loop0 at /run/media/user/LABEL"
        let m = py::strip(&m);
        let tail = m.split_once(" at ").map(|(_, t)| t).unwrap_or(m);
        let mount_path = py::strip(tail.trim_end_matches('.')).to_string();
        return Ok((mount_path, dev));
    }
    Err(format!("RuntimeError: Unsupported OS: {}", std::env::consts::OS))
}

fn unmount_iso(iso_path: &str, dev: &str) {
    if cfg!(target_os = "macos") {
        let _ = proc::run(&argv(&["hdiutil", "detach", dev]), None);
    } else if cfg!(windows) {
        let ps = format!("Dismount-DiskImage -ImagePath '{iso_path}'");
        let _ = proc::run(&argv(&["powershell", "-NoProfile", "-Command", &ps]), None);
    } else {
        let _ = proc::run(&argv(&["udisksctl", "unmount", "-b", dev, "--no-user-interaction"]), None);
        let _ = proc::run(&argv(&["udisksctl", "loop-delete", "-b", dev, "--no-user-interaction"]), None);
    }
}

/// GET /helper/iso/list?id=
pub fn list(id: &str) -> Reply {
    let g = mounted();
    let Some(info) = g.get(id) else { return (404, jobj! {"error" => "unknown id"}) };
    (200, jobj! {"mount_path" => &info.mount_path, "label" => &info.label, "entries" => list_mount(&info.mount_path)})
}

/// POST /helper/iso/mount
pub fn mount(body: &serde_json::Value) -> Reply {
    let label = match body.get("label") {
        v if !py::truthy(v) => "REDSTARS".to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => return (500, jobj! {"error" => format!("TypeError: '{}' object is not subscriptable", py::py_type(other))}),
        None => "REDSTARS".to_string(),
    };
    // {"files": {"nom.ext": "<base64>", …}} gagne sur {"payload": "<base64>"}.
    let mut payload = Payload::None;
    match body.get("files") {
        Some(serde_json::Value::Object(files)) if !files.is_empty() => {
            let mut v = Vec::new();
            for (name, b64) in files {
                match py::b64decode(b64) {
                    Ok(d) => v.push((name.clone(), d)),
                    Err(e) => return (400, jobj! {"error" => format!("bad base64 in files: {e}")}),
                }
            }
            payload = Payload::Files(v);
        }
        _ => {
            if py::truthy(body.get("payload")) {
                match py::b64decode(&body["payload"]) {
                    Ok(d) => payload = Payload::Bytes(d),
                    Err(e) => return (400, jobj! {"error" => format!("bad base64 payload: {e}")}),
                }
            }
        }
    }
    let (mount_path, dev, iso_path) = match make_iso(&label, payload).and_then(|p| mount_iso(&p).map(|(m, d)| (m, d, p))) {
        Ok(x) => x,
        Err(e) => return (500, jobj! {"error" => e}),
    };
    let iso_id = new_id();
    let entries = list_mount(&mount_path);
    mounted().insert(
        iso_id.clone(),
        Mounted { iso_path: iso_path.to_string_lossy().into_owned(), mount_path: mount_path.clone(), dev, label: label.clone() },
    );
    (200, jobj! {"id" => iso_id, "mount_path" => mount_path, "label" => label, "entries" => entries})
}

/// POST /helper/iso/unmount
pub fn unmount(body: &serde_json::Value) -> Reply {
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let Some(info) = body.get("id").and_then(|v| v.as_str()).and_then(|id| mounted().remove(id)) else {
        return (404, jobj! {"error" => "unknown id"});
    };
    unmount_iso(&info.iso_path, &info.dev);
    let _ = std::fs::remove_file(&info.iso_path);
    (200, jobj! {"ok" => true, "id" => id})
}

/// POST /helper/iso/open — seulement un chemin SOUS une iso montée.
pub fn open(body: &serde_json::Value) -> Reply {
    let path = match body.get("path") {
        None => String::new(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => {
            return (500, jobj! {"error" => format!("AttributeError: '{}' object has no attribute 'startswith'", py::py_type(other))})
        }
    };
    if !mounted().values().any(|m| path.starts_with(&m.mount_path)) {
        return (403, jobj! {"error" => "path not in a mounted iso"});
    }
    let r = if cfg!(target_os = "macos") {
        proc::popen(&argv(&["open", &path]))
    } else if cfg!(windows) {
        proc::popen(&argv(&["explorer", &path]))
    } else {
        proc::popen(&argv(&["xdg-open", &path]))
    };
    match r {
        Ok(()) => (200, jobj! {"ok" => true}),
        Err(e) => (500, jobj! {"error" => e}),
    }
}
