//! POST /helper/enable-webgpu et /helper/reset-webgpu : préférences WebGPU dans
//! le profil Firefox par défaut (user.js / prefs.js).

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use super::iso::home;
use super::py::{self, jobj};
use super::Reply;

const PREFS: [&str; 2] = ["dom.webgpu.enabled", "dom.webgpu.unsafe.enabled"];
const NO_PROFILE: &str = "No Firefox profile found at ~/.mozilla/firefox";

/// Profil Firefox actif : celui du `Default=` de la section `[Install…]`
/// (celui que Firefox lance vraiment), sinon un `[Profile]` avec `Default=1`.
fn find_firefox_default_profile() -> Result<Option<PathBuf>, String> {
    let ff_dir = home().join(".mozilla").join("firefox");
    let ini = ff_dir.join("profiles.ini");
    if !ini.exists() {
        return Ok(None);
    }
    let text = py::read_text(&ini)?;
    static INSTALL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\[Install[^\]]+\]\s*\nDefault=(\S+)").expect("install"));
    static PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Path=(\S+)").expect("path"));
    if let Some(m) = INSTALL.captures(&text) {
        let c = ff_dir.join(&m[1]);
        if c.is_dir() {
            return Ok(Some(c));
        }
    }
    // re.split(r'\n(?=\[)', text)
    let mut blocks = Vec::new();
    let mut start = 0;
    for (i, _) in text.match_indices('\n') {
        if text[i + 1..].starts_with('[') {
            blocks.push(&text[start..i]);
            start = i + 1;
        }
    }
    blocks.push(&text[start..]);
    for block in blocks {
        if block.contains("Default=1") {
            if let Some(m) = PATH.captures(block) {
                let c = ff_dir.join(&m[1]);
                if c.is_dir() {
                    return Ok(Some(c));
                }
            }
        }
    }
    Ok(None)
}

pub fn enable() -> Reply {
    match enable_inner() {
        Ok(r) => r,
        Err(e) => (500, jobj! {"error" => e}),
    }
}

fn enable_inner() -> Result<Reply, String> {
    let Some(profile) = find_firefox_default_profile()? else {
        return Ok((500, jobj! {"error" => NO_PROFILE}));
    };
    let user_js = profile.join("user.js");
    let content = if user_js.exists() { py::read_text(&user_js)? } else { String::new() };
    let additions: Vec<String> =
        PREFS.iter().filter(|p| !content.contains(*p)).map(|p| format!("user_pref(\"{p}\", true);")).collect();
    if !additions.is_empty() {
        let header = if content.ends_with('\n') || content.is_empty() { "" } else { "\n" };
        py::write_text(&user_js, &format!("{content}{header}{}\n", additions.join("\n")))?;
    }
    Ok((
        200,
        jobj! {
            "ok" => true,
            "profile" => profile.to_string_lossy().into_owned(),
            "user_js" => user_js.to_string_lossy().into_owned(),
            "wrote" => additions.len(),
            "restart_required" => true,
            "message" => if additions.is_empty() {
                "WebGPU prefs already present in user.js."
            } else {
                "Restart Firefox for changes to take effect."
            },
        },
    ))
}

pub fn reset() -> Reply {
    match reset_inner() {
        Ok(r) => r,
        Err(e) => (500, jobj! {"error" => e}),
    }
}

/// Retire nos préférences de user.js ET les remet au défaut dans prefs.js, pour
/// que le prochain lancement de Firefox ne retrouve pas un `true` en cache.
fn reset_inner() -> Result<Reply, String> {
    let Some(profile) = find_firefox_default_profile()? else {
        return Ok((500, jobj! {"error" => NO_PROFILE}));
    };
    let mut actions: Vec<&str> = Vec::new();
    let user_js = profile.join("user.js");
    if user_js.exists() {
        let text = py::read_text(&user_js)?;
        let kept: Vec<&str> = py::splitlines(&text)
            .into_iter()
            .filter(|l| !PREFS.iter().any(|p| l.contains(p)))
            .filter(|l| !(l.starts_with("//") && l.contains("WebGPU")))
            .collect();
        let joined = kept.join("\n");
        let new_content = py::strip(&joined);
        if new_content.is_empty() {
            std::fs::remove_file(&user_js).map_err(|e| py::oserror_full(&e, &[&user_js.to_string_lossy()]))?;
        } else {
            py::write_text(&user_js, &format!("{new_content}\n"))?;
        }
        actions.push("cleaned user.js");
    }
    // prefs.js est réécrit par Firefox à sa fermeture — correctif au mieux.
    let prefs_js = profile.join("prefs.js");
    if prefs_js.exists() {
        let text = py::read_text(&prefs_js)?;
        let lines = py::splitlines(&text);
        let kept: Vec<&str> = lines.iter().copied().filter(|l| !PREFS.iter().any(|p| l.contains(p))).collect();
        if kept.len() != lines.len() {
            py::write_text(&prefs_js, &format!("{}\n", kept.join("\n")))?;
            actions.push("patched prefs.js (will only stick if Firefox is closed)");
        }
    }
    let action = if actions.is_empty() { "nothing to do".to_string() } else { actions.join(", ") };
    Ok((
        200,
        jobj! {
            "ok" => true,
            "profile" => profile.to_string_lossy().into_owned(),
            "user_js" => user_js.to_string_lossy().into_owned(),
            "action" => action,
            "message" => "Close Firefox first, then reopen, for the reset to apply cleanly.",
        },
    ))
}
