//! Refs « par référence » : un dossier temporaire de liens symboliques vers des
//! fichiers du disque, ouvert pour l'utilisateur dans son gestionnaire de
//! fichiers. Aucune copie de données. Nettoyage via /helper/refs/unmount.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use regex::Regex;
use serde_json::Value;

use super::iso::{self, home, list_mount, new_id};
use super::proc::{self, argv};
use super::py::{self, jobj, J};
use super::Reply;

pub struct Refs {
    pub dir_path: String,
    pub label: String,
    pub sources: Vec<String>,
    #[allow(dead_code)] // gardé comme dans helper.py (diagnostic)
    pub created_at: SystemTime,
}

pub static REFS: LazyLock<Mutex<HashMap<String, Refs>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn refs() -> std::sync::MutexGuard<'static, HashMap<String, Refs>> {
    REFS.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn refs_cache_dir() -> PathBuf {
    home().join(".cache").join("redstars-helper").join("refs")
}

/// Au démarrage, repeuple REFS depuis les sous-dossiers existants
/// `~/.cache/redstars-helper/refs/<LABEL>-<12hex>`. Sans ça, après un
/// redémarrage du helper, les chemins d'un montage de la session précédente
/// seraient refusés (403) alors que les liens existent toujours.
pub fn recover_from_disk() {
    let dir = refs_cache_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    static PAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?P<label>.+)-(?P<id>[0-9a-f]{12})$").expect("pat"));
    let mut g = refs();
    for child in rd.flatten() {
        let p = child.path();
        if !p.is_dir() {
            continue;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        let Some(m) = PAT.captures(&name) else { continue };
        let id = m["id"].to_string();
        if g.contains_key(&id) {
            continue;
        }
        let mut sources = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&p) {
            for e in entries.flatten() {
                if let Ok(tgt) = std::fs::read_link(e.path()) {
                    if tgt.is_absolute() {
                        sources.push(tgt.to_string_lossy().into_owned());
                    } else {
                        sources.push(py::resolve(&p.join(tgt).to_string_lossy()));
                    }
                }
            }
        }
        let created_at = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or_else(|_| SystemTime::now());
        g.insert(
            id,
            Refs { dir_path: p.to_string_lossy().into_owned(), label: m["label"].to_string(), sources, created_at },
        );
    }
}

/// `paths` attendu comme liste de chaînes (`body.get('paths') or []`).
fn str_list(body: &Value, key: &str) -> Result<Vec<String>, String> {
    if !py::truthy(body.get(key)) {
        return Ok(Vec::new());
    }
    match &body[key] {
        Value::Array(a) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                other => Err(format!(
                    "TypeError: argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                    py::py_type(other)
                )),
            })
            .collect(),
        other => Err(format!("TypeError: '{}' paths are not supported", py::py_type(other))),
    }
}

/// `Path(p).exists()` (`Path('')` vaut `.`).
fn exists(p: &str) -> bool {
    Path::new(if p.is_empty() { "." } else { p }).exists()
}

fn str_field(body: &Value, key: &str) -> String {
    body.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}
#[cfg(windows)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::os::windows::fs::symlink_dir(src, dst)
    } else {
        std::os::windows::fs::symlink_file(src, dst)
    }
}

/// Crée `dir/<nom de src>` (suffixé `-1`, `-2`… si pris) → src ; rend l'entrée.
fn link_into(dir: &Path, src_res: &str) -> Result<J, Reply> {
    let base = py::path_name(src_res);
    let mut target = dir.join(&base);
    let mut i = 1;
    while target.exists() {
        let (stem, ext) = py::stem_suffix(&base);
        target = dir.join(format!("{stem}-{i}{ext}"));
        i += 1;
    }
    let t = target.to_string_lossy().into_owned();
    let fail = |msg: String| (500, jobj! {"error" => format!("symlink failed: {msg}"), "path" => src_res});
    symlink(Path::new(src_res), &target).map_err(|e| fail(py::oserror_str(&e, &[src_res, &t])))?;
    let st = std::fs::metadata(src_res).map_err(|e| fail(py::oserror_str(&e, &[src_res])))?;
    Ok(jobj! {
        "name" => py::path_name(&t),
        "path" => t,
        "target" => src_res,
        "size" => st.len(),
        "is_dir" => st.is_dir(),
    })
}

/// GET /helper/refs/list?id=
pub fn list(id: &str) -> Reply {
    let g = refs();
    let Some(info) = g.get(id) else { return (404, jobj! {"error" => "unknown id"}) };
    (200, jobj! {"id" => id, "mount_path" => &info.dir_path, "label" => &info.label, "entries" => list_mount(&info.dir_path)})
}

/// POST /helper/refs/mount — {paths, label?}
pub fn mount(body: &Value) -> Reply {
    let paths = match str_list(body, "paths") {
        Ok(p) => p,
        Err(e) => return (500, jobj! {"error" => e}),
    };
    let label = match py::str_or(body, "label", "REDSTARS", "strip") {
        Ok(l) => match py::strip(&l) {
            "" => "REDSTARS".to_string(),
            s => s.to_string(),
        },
        Err(e) => return (500, jobj! {"error" => e}),
    };
    if paths.is_empty() {
        return (400, jobj! {"error" => "paths required"});
    }
    // Valider que chaque chemin existe AVANT de créer quoi que ce soit.
    let missing: Vec<J> = paths.iter().filter(|p| !exists(p)).map(J::from).collect();
    if !missing.is_empty() {
        return (400, jobj! {"error" => "path not found", "missing" => missing});
    }
    let cache = refs_cache_dir();
    if let Err(e) = std::fs::create_dir_all(&cache) {
        return (500, jobj! {"error" => py::oserror_full(&e, &[&cache.to_string_lossy()])});
    }
    let refs_id = new_id();
    let dir_path = cache.join(format!("{label}-{refs_id}"));
    if let Err(e) = std::fs::create_dir_all(dir_path.parent().unwrap_or(&cache)).and_then(|_| std::fs::create_dir(&dir_path)) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            return (500, jobj! {"error" => "tmp dir collision"});
        }
        return (500, jobj! {"error" => py::oserror_full(&e, &[&dir_path.to_string_lossy()])});
    }
    let mut entries = Vec::new();
    for p in &paths {
        match link_into(&dir_path, &py::resolve(p)) {
            Ok(e) => entries.push(e),
            Err(r) => return r,
        }
    }
    let dir_s = dir_path.to_string_lossy().into_owned();
    refs().insert(
        refs_id.clone(),
        Refs {
            dir_path: dir_s.clone(),
            label: label.clone(),
            sources: paths.iter().map(|p| py::resolve(p)).collect(),
            created_at: SystemTime::now(),
        },
    );
    (200, jobj! {"id" => refs_id, "mount_path" => dir_s, "label" => label, "entries" => J::Arr(entries)})
}

/// POST /helper/refs/add — {id, paths} : cumule des sélections dans un montage.
pub fn add(body: &Value) -> Reply {
    let refs_id = str_field(body, "id");
    let new_paths = match str_list(body, "paths") {
        Ok(p) => p,
        Err(e) => return (500, jobj! {"error" => e}),
    };
    let dir_path = match refs().get(&refs_id) {
        Some(info) => PathBuf::from(&info.dir_path),
        None => return (404, jobj! {"error" => "unknown id"}),
    };
    if new_paths.is_empty() {
        return (400, jobj! {"error" => "paths required"});
    }
    if !dir_path.is_dir() {
        return (500, jobj! {"error" => "mount dir disappeared"});
    }
    let mut added = Vec::new();
    for p in &new_paths {
        if !exists(p) {
            let name = py::path_name(p);
            added.push(jobj! {
                "name" => name.clone(),
                "path" => dir_path.join(&name).to_string_lossy().into_owned(),
                "target" => py::path_str(p),
                "size" => 0,
                "is_dir" => false,
                "missing" => true,
            });
            continue;
        }
        match link_into(&dir_path, &py::resolve(p)) {
            Ok(e) => added.push(e),
            Err(r) => return r,
        }
    }
    if let Some(info) = refs().get_mut(&refs_id) {
        info.sources.extend(new_paths.iter().map(|p| py::resolve(p)));
    }
    (200, jobj! {"ok" => true, "added" => J::Arr(added)})
}

const FILE_MANAGERS_LINUX: &[&str] = &[
    "thunar",
    "nautilus",
    "dolphin",
    "nemo",
    "caja",
    "pcmanfm-qt",
    "pcmanfm",
    "peony",
    "konqueror",
    "krusader",
    "spacefm",
    "index.fm",
    "qtfm",
    "dde-file-manager",
    "cosmic-files",
    "nautilus-desktop",
    "gnome-files",
    "Files",
];

/// POST /helper/refs/open — {id, path?}
pub fn open(body: &Value) -> Reply {
    let refs_id = str_field(body, "id");
    let dir = match refs().get(&refs_id) {
        Some(info) => Some(info.dir_path.clone()),
        None => None,
    };
    // Repli : une vraie iso (make_iso) vit dans MOUNTED, pas dans REFS.
    let dir = match dir.or_else(|| iso::mounted().get(&refs_id).map(|m| m.mount_path.clone())) {
        Some(d) => d,
        None => return (404, jobj! {"error" => "unknown id"}),
    };
    let target = match body.get("path") {
        v if !py::truthy(v) => dir.clone(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return (500, jobj! {"error" => format!("TypeError: expected str, bytes or os.PathLike object, not {}", py::py_type(other))})
        }
        None => dir.clone(),
    };
    // abspath + normpath, PAS resolve() : les liens pointent HORS du dossier
    // (c'est leur raison d'être). On se garde de `.../../../etc/passwd` littéral.
    let mount_norm = py::abspath(&dir);
    let target_norm = py::abspath(&target);
    let sep = std::path::MAIN_SEPARATOR;
    if target_norm != mount_norm && !target_norm.starts_with(&format!("{mount_norm}{sep}")) {
        return (403, jobj! {"error" => "path not in refs mount"});
    }
    let is_dir = Path::new(&target).is_dir(); // suit les liens
    let mut opener = "xdg".to_string();
    let r = if cfg!(target_os = "macos") {
        // `open <dossier>` ouvre le Finder.
        if is_dir {
            opener = "finder".into();
        }
        proc::popen(&argv(&["open", &target]))
    } else if cfg!(windows) {
        if is_dir {
            opener = "explorer".into();
        }
        proc::popen(&argv(&["explorer", &target]))
    } else {
        // Les DOSSIERS contournent xdg-open : les éditeurs (VSCodium, …) se
        // déclarent gestionnaires de inode/directory et détourneraient le
        // montage. Les FICHIERS gardent xdg-open (choix MIME de l'utilisateur).
        let mut cmd = None;
        if is_dir {
            let dt = format!(
                "{}:{}",
                std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
                std::env::var("XDG_SESSION_DESKTOP").unwrap_or_default()
            )
            .to_lowercase();
            let de_map = [
                ("xfce", "thunar"),
                ("gnome", "nautilus"),
                ("kde", "dolphin"),
                ("plasma", "dolphin"),
                ("mate", "caja"),
                ("cinnamon", "nemo"),
                ("lxqt", "pcmanfm-qt"),
                ("lxde", "pcmanfm"),
                ("ukui", "peony"),
                ("deepin", "dde-file-manager"),
                ("cosmic", "cosmic-files"),
            ];
            let preferred = de_map.iter().find(|(tok, _)| dt.contains(tok)).map(|(_, fm)| *fm);
            let order = preferred.into_iter().chain(FILE_MANAGERS_LINUX.iter().copied().filter(|f| Some(*f) != preferred));
            for fm in order {
                if proc::which(fm) {
                    cmd = Some(argv(&[fm, &target]));
                    opener = format!("fm:{fm}");
                    break;
                }
            }
        }
        proc::popen(&cmd.unwrap_or_else(|| argv(&["xdg-open", &target])))
    };
    match r {
        Ok(()) => (200, jobj! {"ok" => true, "path" => target, "opener" => opener}),
        Err(e) => (500, jobj! {"error" => e}),
    }
}

/// POST /helper/refs/unmount — {id} : supprime les liens (cibles intactes).
pub fn unmount(body: &Value) -> Reply {
    let refs_id = str_field(body, "id");
    let Some(info) = refs().remove(&refs_id) else { return (404, jobj! {"error" => "unknown id"}) };
    let d = Path::new(&info.dir_path);
    if d.exists() {
        if let Ok(rd) = std::fs::read_dir(d) {
            for child in rd.flatten() {
                let _ = std::fs::remove_file(child.path()).or_else(|_| std::fs::remove_dir(child.path()));
            }
        }
        if let Err(e) = std::fs::remove_dir(d) {
            return (
                500,
                jobj! {"error" => format!("unmount failed: {}", py::oserror_str(&e, &[&info.dir_path])), "id" => refs_id, "dir" => &info.dir_path},
            );
        }
    }
    (200, jobj! {"ok" => true, "id" => refs_id})
}
