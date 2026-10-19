//! Session console conservée sur disque (~/.local/state/redstars/session.json).
//!
//! Le jeton d'accès vit quinze minutes, le refresh_token sept jours : on garde
//! les deux pour que la console marche une semaine sans retaper le mot de passe.
//! Même fichier, mêmes clés que `_con_session_*` de helper.py — une session
//! écrite par l'un est lue par l'autre. Répertoire 0700, fichier 0600 (unix).

use super::http;
use base64::Engine;
use serde_json::Value;
use std::io::{self, Write};
use std::path::PathBuf;

/// Le répertoire d'état : `$XDG_STATE_HOME` s'il est non vide, sinon
/// `~/.local/state`.
fn state_base() -> PathBuf {
    match std::env::var_os("XDG_STATE_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x),
        _ => dirs::home_dir().unwrap_or_else(|| PathBuf::from("~")).join(".local/state"),
    }
}

/// `_con_session_path` : crée `<state>/redstars` (0700) et rend le chemin du fichier.
pub fn path() -> io::Result<PathBuf> {
    let d = state_base().join("redstars");
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(&d)?;
    Ok(d.join("session.json"))
}

/// Le contenu exact qu'écrit Python : `json.dump({...}, f)`.
fn serialize(access: &str, refresh: &str, api: &str) -> String {
    http::py_json_obj(&[("access_token", access), ("refresh_token", refresh), ("api", api)])
}

/// Écrit la session (droits 0600), atomiquement (fichier .tmp puis rename).
/// Ne garde RIEN sans refresh_token : un jeton d'accès seul serait mort dans un
/// quart d'heure.
pub fn save(access: &str, refresh: &str, api: &str) -> io::Result<()> {
    if refresh.is_empty() {
        return Ok(());
    }
    let path = path()?;
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut oo = std::fs::OpenOptions::new();
    oo.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        oo.mode(0o600);
    }
    let mut f = oo.open(&tmp)?;
    f.write_all(serialize(access, refresh, api).as_bytes())?;
    f.flush()?;
    drop(f);
    std::fs::rename(&tmp, &path)
}

/// Le JWT est-il périmé (ou à moins de `margin` secondes de l'être) à l'instant
/// `now` ? Illisible = périmé.
pub fn jwt_expired_at(tok: &str, margin: f64, now: f64) -> bool {
    let Some(payload) = tok.split('.').nth(1) else { return true };
    // urlsafe_b64decode de Python ignore le bourrage manquant une fois complété ;
    // on accepte les deux formes.
    let trimmed = payload.trim_end_matches('=');
    let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(trimmed) else { return true };
    let Ok(v) = serde_json::from_slice::<Value>(&raw) else { return true };
    let Some(obj) = v.as_object() else { return true };
    let exp = match obj.get("exp") {
        None => 0.0,
        Some(e) => match e.as_f64() {
            Some(x) => x,
            None => return true, // exp non numérique : TypeError en Python → périmé
        },
    };
    now + margin >= exp
}

pub fn jwt_expired(tok: &str, margin: f64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    jwt_expired_at(tok, margin, now)
}

/// `_con_session_load` : rend un jeton d'accès VALIDE, ou None. Renouvelle en
/// silence via `POST {api}/api/v1/auth/refresh` (refresh dans l'en-tête
/// `X-Refresh-Token`, pas dans le corps) et réécrit la session.
pub fn load(api: &str) -> Option<String> {
    let p = path().ok()?;
    let txt = std::fs::read(&p).ok()?;
    let d: Value = serde_json::from_slice(&txt).ok()?;
    let s = |k: &str| d.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
    let access = s("access_token");
    let refresh = s("refresh_token");
    if let Some(a) = &access {
        if !jwt_expired(a, 60.0) {
            return access;
        }
    }
    let refresh = refresh?;
    let new = http::req_json(
        &format!("{api}/api/v1/auth/refresh"),
        None,
        &[("X-Refresh-Token", &refresh)],
        Some("POST"),
        15,
    )
    .ok()?;
    let access = new.get("access_token").and_then(|v| v.as_str()).filter(|s| !s.is_empty())?.to_string();
    // Rotation : le serveur PEUT renvoyer un nouveau refresh.
    let rot = new
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(refresh);
    let _ = save(&access, &rot, api);
    Some(access)
}

/// Les variables d'environnement sont globales au processus : les tests qui y
/// touchent passent par ce verrou.
#[cfg(test)]
pub(crate) fn tests_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(exp: Option<serde_json::Value>) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let mut claims = serde_json::json!({"sub": "6"});
        if let Some(x) = exp {
            claims["exp"] = x;
        }
        format!("{}.{}.sig", e.encode(b"{\"alg\":\"HS256\"}"), e.encode(claims.to_string()))
    }

    #[test]
    fn jwt_expiry() {
        assert!(!jwt_expired_at(&jwt(Some(1000.into())), 60.0, 900.0));
        assert!(jwt_expired_at(&jwt(Some(1000.into())), 60.0, 940.0)); // dans la marge
        assert!(jwt_expired_at(&jwt(None), 60.0, 0.0)); // exp absent → 0
        assert!(jwt_expired_at(&jwt(Some("x".into())), 60.0, 0.0));
        assert!(jwt_expired_at("pas-un-jwt", 60.0, 0.0));
        assert!(jwt_expired_at("a.!!!.b", 60.0, 0.0));
        assert!(!jwt_expired_at(&jwt(Some(1000.5.into())), 0.0, 1000.0));
    }

    #[test]
    fn session_round_trip_and_python_compat() {
        let _g = tests_env_lock();
        let dir = std::env::temp_dir().join(format!("rs-sess-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("XDG_STATE_HOME", &dir);

        // Pas de refresh → rien d'écrit.
        save("a", "", "https://api").unwrap();
        assert!(!dir.join("redstars/session.json").exists());

        // Un jeton valide (exp lointain) est rendu tel quel, sans réseau.
        let tok = jwt(Some(4_000_000_000u64.into()));
        save(&tok, "r1", "https://api.example").unwrap();
        let p = dir.join("redstars/session.json");
        let txt = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            txt,
            format!(r#"{{"access_token": "{tok}", "refresh_token": "r1", "api": "https://api.example"}}"#)
        );
        assert_eq!(load("https://api.example"), Some(tok.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
            let dm = std::fs::metadata(dir.join("redstars")).unwrap().permissions().mode() & 0o777;
            assert_eq!(dm, 0o700);
        }

        // Fichier écrit par helper.py (json.dump, même format), lu par Rust.
        std::fs::write(
            &p,
            format!(r#"{{"access_token": "{tok}", "refresh_token": "zz", "api": "https://x"}}"#),
        )
        .unwrap();
        assert_eq!(load("https://x"), Some(tok));

        // Jeton périmé sans refresh → None (aucune requête).
        std::fs::write(&p, format!(r#"{{"access_token": "{}"}}"#, jwt(Some(1.into())))).unwrap();
        assert_eq!(load("https://x"), None);
        // Fichier illisible → None.
        std::fs::write(&p, "{pas du json").unwrap();
        assert_eq!(load("https://x"), None);

        std::env::remove_var("XDG_STATE_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
