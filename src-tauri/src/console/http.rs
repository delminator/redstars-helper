//! Le transport du client console : `_con_req` / `_con_frame` de helper.py.
//!
//! Tout ce qui part sur le réseau passe par `req()`, et donc par la trace
//! `REDSTARS_CONSOLE_TRACE` (« 1 » → stderr, sinon un chemin de fichier où l'on
//! ajoute une ligne par requête). C'est elle qui permet de comparer, requête par
//! requête, ce client avec le client Python qu'il remplace.

use serde_json::Value;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Erreur d'une requête. `Http` = le serveur a répondu hors 2xx (l'équivalent de
/// `urllib.error.HTTPError`, que les appelants interceptent) ; `Other` = tout le
/// reste (réseau, TLS, délai, JSON illisible), qu'en Python rien n'interceptait.
#[derive(Debug)]
pub enum ReqErr {
    Http { code: u16, body: Vec<u8> },
    Other(String),
}

impl std::fmt::Display for ReqErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReqErr::Http { code, .. } => write!(f, "HTTP {code}"),
            ReqErr::Other(m) => write!(f, "{m}"),
        }
    }
}

impl ReqErr {
    /// `json.loads(e.read() or b"{}").get("error", f"HTTP {e.code}")`, avec un
    /// repli sur `HTTP <code>` là où Python plantait (corps non JSON).
    pub fn server_error(&self, fallback_prefix: &str) -> String {
        match self {
            ReqErr::Http { code, body } => {
                let body: &[u8] = if body.is_empty() { b"{}" } else { body };
                serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)))
                    .unwrap_or_else(|| format!("{fallback_prefix}{code}"))
            }
            ReqErr::Other(m) => m.clone(),
        }
    }
}

// ── Négocié au démarrage (les globales `_CON_COLOR` / `_CON_IMG`) ────────────

static TRUECOLOR: AtomicBool = AtomicBool::new(false);
static IMG: AtomicBool = AtomicBool::new(false);

pub fn set_truecolor(v: bool) {
    TRUECOLOR.store(v, Ordering::Relaxed);
}
pub fn set_img(v: bool) {
    IMG.store(v, Ordering::Relaxed);
}
fn color() -> &'static str {
    if TRUECOLOR.load(Ordering::Relaxed) {
        "truecolor"
    } else {
        "ansi256"
    }
}

/// `_con_color_depth` : truecolor seulement si le terminal l'annonce, jamais en
/// Minitel (la console Linux ne le rend pas).
pub fn color_depth(minitel: bool, colorterm: Option<&str>) -> &'static str {
    if minitel {
        return "ansi256";
    }
    match colorterm.map(|s| s.to_lowercase()) {
        Some(s) if s == "truecolor" || s == "24bit" => "truecolor",
        _ => "ansi256",
    }
}

// ── Encodage, à l'octet près comme Python ───────────────────────────────────

/// `urllib.parse.quote_plus` : alphanumériques ASCII et `_.-~` tels quels,
/// espace → `+`, tout le reste en `%XX` majuscule (octets UTF-8).
pub fn quote_plus(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.urlencode` sur une liste ordonnée (l'ordre d'un dict Python).
pub fn urlencode(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", quote_plus(k), quote_plus(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Une chaîne JSON comme `json.dumps` (ensure_ascii=True) : non-ASCII en `\uXXXX`.
pub fn py_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Un objet plat de chaînes comme `json.dumps({...})` : `{"a": "x", "b": "y"}`.
pub fn py_json_obj(pairs: &[(&str, &str)]) -> String {
    let body = pairs
        .iter()
        .map(|(k, v)| format!("{}: {}", py_json_str(k), py_json_str(v)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

/// La représentation `str(v)` d'une valeur JSON telle que Python l'enverrait
/// dans une query (None = absent).
pub fn pyval(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Bool(true) => Some("True".into()),
        Value::Bool(false) => Some("False".into()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

/// Comme `pyval`, mais « faux » au sens Python (`""`, `0`, `False`) → None : pour
/// les `if not app:` / `if not org_oid:`.
pub fn pytruthy(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if s.is_empty() => None,
        Value::Bool(false) => None,
        Value::Number(n) if n.as_f64() == Some(0.0) => None,
        Value::Array(a) if a.is_empty() => None,
        Value::Object(o) if o.is_empty() => None,
        other => pyval(Some(other)),
    }
}

// ── Trace ────────────────────────────────────────────────────────────────────

fn trace(method: &str, url: &str, auth: bool, refresh: bool, body: Option<&str>) {
    let Ok(dest) = std::env::var("REDSTARS_CONSOLE_TRACE") else { return };
    if dest.is_empty() || dest == "0" {
        return;
    }
    let line = format!(
        "{method} {url} auth={} refresh={} body={}\n",
        if auth { "Bearer" } else { "-" },
        if refresh { "y" } else { "-" },
        body.unwrap_or("-")
    );
    if dest == "1" {
        eprint!("{line}");
    } else if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&dest) {
        let _ = f.write_all(line.as_bytes());
    }
}

// ── Requêtes ─────────────────────────────────────────────────────────────────

fn client() -> &'static reqwest::blocking::Client {
    static C: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent(concat!("redstars-helper/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("client HTTP")
    })
}

/// `_con_req(raw=True)` : rend les octets de la réponse. `body` est le JSON déjà
/// sérialisé ; la méthode par défaut est POST s'il y a un corps, GET sinon.
pub fn req(
    url: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
    method: Option<&str>,
    timeout_s: u64,
) -> Result<Vec<u8>, ReqErr> {
    let method = method.unwrap_or(if body.is_some() { "POST" } else { "GET" });
    let auth = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
    let refresh = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-refresh-token"));
    trace(method, url, auth, refresh, body);

    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| ReqErr::Other(e.to_string()))?;
    let mut rb = client()
        .request(m, url)
        .timeout(Duration::from_secs(timeout_s))
        .header("Content-Type", "application/json");
    for (k, v) in headers {
        rb = rb.header(*k, *v);
    }
    if let Some(b) = body {
        rb = rb.body(b.to_string());
    }
    let resp = rb.send().map_err(|e| ReqErr::Other(e.to_string()))?;
    let code = resp.status().as_u16();
    let bytes = resp.bytes().map_err(|e| ReqErr::Other(e.to_string()))?.to_vec();
    if !(200..300).contains(&code) {
        return Err(ReqErr::Http { code, body: bytes });
    }
    Ok(bytes)
}

/// `_con_req` : la réponse décodée en JSON (`payload or b"{}"`).
pub fn req_json(
    url: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
    method: Option<&str>,
    timeout_s: u64,
) -> Result<Value, ReqErr> {
    let bytes = req(url, body, headers, method, timeout_s)?;
    let bytes: &[u8] = if bytes.is_empty() { b"{}" } else { &bytes };
    serde_json::from_slice(bytes).map_err(|e| ReqErr::Other(format!("réponse illisible : {e}")))
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// GET authentifié, texte brut (les trames `a11y=1`).
pub fn get_text(url: &str, token: &str) -> Result<String, ReqErr> {
    let b = bearer(token);
    let bytes = req(url, None, &[("Authorization", &b)], None, 30)?;
    String::from_utf8(bytes).map_err(|e| ReqErr::Other(e.to_string()))
}

/// L'URL d'une trame : `{app_url}/api/console/frame/?{query}`.
pub fn frame_url(app_url: &str, pairs: &[(&str, String)]) -> String {
    format!("{app_url}/api/console/frame/?{}", urlencode(pairs))
}

/// Le décalage local en minutes à l'est de UTC (`tm_gmtoff // 60`).
pub fn tz_minutes() -> i64 {
    let secs = chrono::Local::now().offset().local_minus_utc() as i64;
    secs.div_euclid(60)
}

/// Complète une query de trame comme `_con_frame` : retire les None, ajoute
/// `tz`, `color` et (si négocié) `img`, dans cet ordre.
pub fn frame_query(q: &[(&str, Option<String>)], tz: i64, color: &str, img: bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        q.iter().filter_map(|(k, v)| v.clone().map(|v| (k.to_string(), v))).collect();
    let has = |out: &Vec<(String, String)>, k: &str| out.iter().any(|(kk, _)| kk == k);
    if !has(&out, "tz") {
        out.push(("tz".into(), tz.to_string()));
    }
    if !has(&out, "color") {
        out.push(("color".into(), color.into()));
    }
    if !has(&out, "img") && img {
        out.push(("img".into(), "1".into()));
    }
    out
}

/// `_con_frame` : demande une trame. Une erreur HTTP devient `{"error": …}` comme
/// en Python ; une erreur réseau remonte (`Err`).
pub fn frame(app_url: &str, token: &str, q: &[(&str, Option<String>)]) -> Result<Value, ReqErr> {
    let full = frame_query(q, tz_minutes(), color(), IMG.load(Ordering::Relaxed));
    let pairs: Vec<(&str, String)> = full.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let url = frame_url(app_url, &pairs);
    let b = bearer(token);
    match req_json(&url, None, &[("Authorization", &b)], None, 30) {
        Ok(v) => Ok(v),
        Err(e @ ReqErr::Http { .. }) => Ok(serde_json::json!({ "error": e.server_error("HTTP ") })),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_plus_matches_python() {
        // python3 -c "import urllib.parse as u; print(u.quote_plus('a b/é&=~_.-'))"
        assert_eq!(quote_plus("a b/é&=~_.-"), "a+b%2F%C3%A9%26%3D~_.-");
        assert_eq!(
            urlencode(&[("app", "eau".into()), ("org", "org:4:x".into()), ("sel", "3".into())]),
            "app=eau&org=org%3A4%3Ax&sel=3"
        );
    }

    #[test]
    fn json_like_python() {
        // json.dumps({"username": "é\"x", "password": "p\n"})
        assert_eq!(
            py_json_obj(&[("username", "é\"x"), ("password", "p\n")]),
            "{\"username\": \"\\u00e9\\\"x\", \"password\": \"p\\n\"}"
        );
        assert_eq!(py_json_str("\u{1F600}"), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn frame_query_order_and_filter() {
        let q = [
            ("app", Some("eau".to_string())),
            ("org", None),
            ("role", Some("collaborator".to_string())),
            ("sel", Some("1".to_string())),
        ];
        let full = frame_query(&q, 120, "ansi256", false);
        let keys: Vec<_> = full.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["app", "role", "sel", "tz", "color"]);
        let full = frame_query(&q, -90, "truecolor", true);
        assert_eq!(full.last().unwrap(), &("img".to_string(), "1".to_string()));
        assert_eq!(full[3].1, "-90");
    }

    #[test]
    fn color_negotiation() {
        assert_eq!(color_depth(true, Some("truecolor")), "ansi256");
        assert_eq!(color_depth(false, Some("24BIT")), "truecolor");
        assert_eq!(color_depth(false, None), "ansi256");
    }

    #[test]
    fn pyval_semantics() {
        use serde_json::json;
        assert_eq!(pyval(Some(&json!(3))), Some("3".into()));
        assert_eq!(pyval(Some(&json!(null))), None);
        assert_eq!(pytruthy(Some(&json!(""))), None);
        assert_eq!(pytruthy(Some(&json!("x"))), Some("x".into()));
    }

    #[test]
    fn server_error_extraction() {
        let e = ReqErr::Http { code: 403, body: br#"{"error":"interdit"}"#.to_vec() };
        assert_eq!(e.server_error("HTTP "), "interdit");
        let e = ReqErr::Http { code: 502, body: b"<html>".to_vec() };
        assert_eq!(e.server_error("Erreur HTTP "), "Erreur HTTP 502");
    }
}
