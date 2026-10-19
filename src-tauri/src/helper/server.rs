//! Serveur /helper/* (portage de `Handler` + `main()` de helper.py 0.5.64).
//!
//! HTTP sur 0.0.0.0:HELPER_PORT (49080) et HTTPS sur 127.0.0.1:HELPER_HTTPS_PORT
//! (49443, certificat local.redlinks.fr — lié à 127.0.0.1 seulement, comme
//! helper.py : la clé permettrait à toute machine du LAN de se faire passer pour
//! nous). Tout ce qui n'est pas sous /helper/ répond 404.
//!
//! Le serveur tourne sur son propre runtime tokio, dans un fil à lui : il ne
//! dépend pas du runtime de Tauri. Les routes sont bloquantes (sous-processus,
//! port série, sélecteur de fichiers) : chacune s'exécute dans `spawn_blocking`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use serde_json::Value;

use super::py::{self, jobj, J};
use super::{iso, refs, scale, tls, webgpu, Reply, VERSION};

/// Origines admises à appeler /helper/* en cross-origin.
const ALLOWED_ORIGINS: &[&str] = &[
    "https://dev.redstars.redlinks.fr",
    "https://redstars.redlinks.fr",
    "http://localhost:49080",
    "https://local.redlinks.fr:49443",
    // Hérité — gardé pour les vieux clients pendant la migration hors de 9999/8443.
    "http://localhost:9999",
    "https://local.redlinks.fr:8443",
];

const DEFAULT_API_URL: &str = "https://api.dev.redstars.redlinks.fr";
/// Les ISO arrivent en base64 dans du JSON : on accepte gros.
const BODY_LIMIT: usize = 512 << 20;
const TLS_RELOAD_EVERY: Duration = Duration::from_secs(6 * 3600);

// ─── Arrêt quand le site est fermé ───────────────────────────────────────────
// Un site ouvert envoie GET /helper/ping toutes les 4 min et toute autre requête
// compte aussi. Sans nouvelles pendant IDLE_EXIT_S, on appelle `on_idle_exit`.
// Horloge monotone : un portable qui se réveille ne trouve pas son helper
// « muet depuis 8 h ».

static BASE: LazyLock<Instant> = LazyLock::new(Instant::now);
static LAST_SEEN_NS: AtomicU64 = AtomicU64::new(0);

fn mark_seen() {
    LAST_SEEN_NS.store(BASE.elapsed().as_nanos() as u64, Ordering::Relaxed);
}

fn since_seen() -> Duration {
    BASE.elapsed().saturating_sub(Duration::from_nanos(LAST_SEEN_NS.load(Ordering::Relaxed)))
}

fn start_idle_watchdog(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) {
    if std::env::var("REDSTARS_HELPER_PLATFORM").as_deref() == Ok("android") {
        return; // le helper vit dans l'appli, qui est le site : il meurt avec elle
    }
    let idle: i64 = std::env::var("REDSTARS_HELPER_IDLE_EXIT_S")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(720);
    if idle <= 0 {
        println!("  arrêt sur inactivité : désactivé (REDSTARS_HELPER_IDLE_EXIT_S=0)");
        return;
    }
    let span = if idle >= 120 { format!("{} min", idle / 60) } else { format!("{idle} s") };
    println!("  arrêt sur inactivité : après {span} sans nouvelles du site");
    let limit = Duration::from_secs(idle as u64);
    let spawned = std::thread::Builder::new().name("idle-watchdog".into()).spawn(move || {
        loop {
            let seen = since_seen();
            if seen >= limit {
                break;
            }
            // Dormir jusqu'à l'échéance, pas plus souvent.
            std::thread::sleep((limit - seen).max(Duration::from_secs(5)));
        }
        println!("redstars-helper: aucun signe du site depuis {span} — arrêt.");
        on_idle_exit();
    });
    if let Err(e) = spawned {
        eprintln!("  arrêt sur inactivité : fil impossible ({e})");
    }
}

// ─── Mise à jour : déléguée au shell ─────────────────────────────────────────

type Hook = Arc<dyn Fn() + Send + Sync + 'static>;
static UPDATE_HOOK: Mutex<Option<Hook>> = Mutex::new(None);

pub fn set_update_hook(f: Box<dyn Fn() + Send + Sync + 'static>) {
    *UPDATE_HOOK.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::from(f));
}

fn fire_update_hook() {
    let hook = UPDATE_HOOK.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(h) = hook {
        // Laisser partir la réponse d'abord (le shell peut redémarrer).
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h())).is_err() {
                eprintln!("[helper] le crochet de mise à jour a paniqué");
            }
        });
    }
}

// ─── Réponses ────────────────────────────────────────────────────────────────

struct Resp {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Resp {
    fn json(code: u16, payload: &J) -> Resp {
        let body = payload.dumps().into_bytes();
        Resp {
            status: code,
            headers: vec![("Content-Type", "application/json".into()), ("Content-Length", body.len().to_string())],
            body,
        }
    }

    fn empty(code: u16) -> Resp {
        Resp { status: code, headers: Vec::new(), body: Vec::new() }
    }

    /// `send_error(501, "Unsupported method ('X')")` de BaseHTTPRequestHandler
    /// (gabarit de Python 3.14).
    fn unsupported(method: &str) -> Resp {
        let msg = format!("Unsupported method ({})", py::repr_str(method));
        let esc = msg.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let body = format!(
            "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        \
             <style type=\"text/css\">\n            :root {{\n                color-scheme: light dark;\n            }}\n        \
             </style>\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        \
             <p>Error code: 501</p>\n        <p>Message: {esc}.</p>\n        \
             <p>Error code explanation: 501 - Server does not support this operation.</p>\n    </body>\n</html>\n"
        )
        .into_bytes();
        Resp {
            status: 501,
            headers: vec![
                ("Connection", "close".into()),
                ("Content-Type", "text/html;charset=utf-8".into()),
                ("Content-Length", body.len().to_string()),
            ],
            body,
        }
    }

    /// `Handler.end_headers` : CORS + PNA, puis COOP/COEP/CORP.
    fn end_headers(&mut self, origin: &str, path: &str) {
        let known = ALLOWED_ORIGINS.contains(&origin);
        if known || path.starts_with("/helper/") {
            let allow = if known { origin } else { "*" };
            self.headers.push(("Access-Control-Allow-Origin", allow.to_string()));
            self.headers.push(("Access-Control-Allow-Methods", "GET, POST, OPTIONS".into()));
            self.headers.push(("Access-Control-Allow-Headers", "Content-Type".into()));
            // Private Network Access : Firefox 142+/Chrome 130+ l'exigent quand une
            // page publique cible 127.0.0.1 via local.redlinks.fr.
            self.headers.push(("Access-Control-Allow-Private-Network", "true".into()));
        }
        self.headers.push(("Cross-Origin-Opener-Policy", "same-origin".into()));
        self.headers.push(("Cross-Origin-Embedder-Policy", "require-corp".into()));
        self.headers.push(("Cross-Origin-Resource-Policy", "cross-origin".into()));
    }

    fn into_response(self) -> Response<Body> {
        let mut r = Response::new(Body::from(self.body));
        *r.status_mut() = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let h = r.headers_mut();
        for (k, v) in self.headers {
            if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
                h.append(k, v);
            }
        }
        r
    }
}

// ─── Requêtes ────────────────────────────────────────────────────────────────

struct Req<'a> {
    method: &'a str,
    /// Chemin brut, requête comprise (`self.path` de Python).
    path: &'a str,
    headers: &'a HeaderMap,
    body: &'a [u8],
}

fn header_str(h: &HeaderMap, name: &str) -> String {
    // http.server décode les en-têtes en latin-1.
    h.get(name).map(|v| v.as_bytes().iter().map(|&b| b as char).collect()).unwrap_or_default()
}

impl Req<'_> {
    fn origin(&self) -> String {
        header_str(self.headers, "origin")
    }

    /// `_read_json` : Content-Length octets de JSON ; illisible ⇒ `{}`.
    fn json(&self) -> Value {
        let n: i64 = header_str(self.headers, "content-length").trim().parse().unwrap_or(0);
        if n <= 0 {
            return Value::Object(Default::default());
        }
        let data = &self.body[..self.body.len().min(n as usize)];
        std::str::from_utf8(data)
            .ok()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| Value::Object(Default::default()))
    }

    /// `(path après /helper, query)`.
    fn split(&self) -> (&str, &str) {
        let p = self.path.split('#').next().unwrap_or("");
        let (path, query) = p.split_once('?').unwrap_or((p, ""));
        (&path["/helper".len()..], query)
    }
}

fn dispatch(req: &Req) -> Resp {
    let origin = req.origin();
    let mut resp = match req.method {
        "OPTIONS" => {
            mark_seen(); // préflight des appels cross-origin du tableau de bord
            Resp::empty(204)
        }
        "GET" => {
            mark_seen(); // toute requête prouve qu'un client est là
            if req.path.starts_with("/helper/") {
                let (code, j) = do_get(req);
                Resp::json(code, &j)
            } else {
                Resp::json(404, &jobj! {"error" => "not found"})
            }
        }
        "HEAD" => Resp::empty(if req.path.starts_with("/helper/") { 200 } else { 404 }),
        "POST" => {
            mark_seen();
            if req.path.starts_with("/helper/") {
                let (code, j) = do_post(req, &origin);
                Resp::json(code, &j)
            } else {
                Resp::json(404, &jobj! {"error" => "POST only allowed under /helper/*"})
            }
        }
        m => Resp::unsupported(m),
    };
    resp.end_headers(&origin, req.path);
    resp
}

fn do_get(req: &Req) -> Reply {
    let (ep, query) = req.split();
    let q = |k: &str| py::query_first(query, k).unwrap_or_default();
    match ep {
        // Battement du site ouvert : mark_seen() a déjà fait le travail.
        "/ping" => (200, jobj! {"ok" => true}),
        "/status" => (200, jobj! {"ok" => true, "version" => VERSION}),
        "/disk" => disk(&q("path")),
        "/scale" => (200, scale::read_json()),
        "/lsusb" => lsusb(),
        "/iso/list" => iso::list(&q("id")),
        "/files/pick" => files_pick(&q("mode")),
        "/refs/list" => refs::list(&q("id")),
        _ => (404, jobj! {"error" => "unknown helper endpoint"}),
    }
}

fn do_post(req: &Req, origin: &str) -> Reply {
    let (ep, _) = req.split();
    match ep {
        "/session" | "/console" if !ALLOWED_ORIGINS.contains(&origin) => {
            // Ces routes écrivent un secret / ouvrent un terminal : le CORS
            // permissif du reste de /helper/* ne suffit pas.
            let o = if origin.is_empty() { "(absente)" } else { origin };
            (403, jobj! {"error" => format!("origine refusée: {o}")})
        }
        "/session" => session(&req.json()),
        "/console" => console(&req.json()),
        "/enable-webgpu" => webgpu::enable(),
        "/reset-webgpu" => webgpu::reset(),
        "/update" => {
            // Plus de script à mettre à jour : le helper natif suit l'application.
            fire_update_hook();
            (
                200,
                jobj! {
                    "ok" => true,
                    "updated" => false,
                    "version" => VERSION,
                    "message" => "helper natif : mis à jour avec l'application",
                },
            )
        }
        "/iso/mount" => iso::mount(&req.json()),
        "/iso/unmount" => iso::unmount(&req.json()),
        "/iso/open" => iso::open(&req.json()),
        "/refs/mount" => refs::mount(&req.json()),
        "/refs/add" => refs::add(&req.json()),
        "/refs/open" => refs::open(&req.json()),
        "/refs/unmount" => refs::unmount(&req.json()),
        _ => (404, jobj! {"error" => "unknown helper endpoint"}),
    }
}

/// Appelle du code d'un autre module sans qu'une panique tue le serveur.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".into());
        format!("RuntimeError: {msg}")
    })
}

fn obj(b: &Value) -> Result<(), String> {
    if b.is_object() {
        Ok(())
    } else {
        Err(format!("AttributeError: '{}' object has no attribute 'get'", py::py_type(b)))
    }
}

/// POST /helper/session — conserver la session, SANS ouvrir de terminal.
fn session(b: &Value) -> Reply {
    let r = (|| -> Result<Reply, String> {
        obj(b)?;
        let token = py::strip(&py::str_or(b, "token", "", "strip")?).to_string();
        let refresh = py::strip(&py::str_or(b, "refresh", "", "strip")?).to_string();
        if token.is_empty() || refresh.is_empty() {
            // Sans refresh, rien à conserver : un jeton d'accès seul meurt en 15 min.
            return Ok((400, jobj! {"error" => "jeton et refresh requis"}));
        }
        let api = py::strip(&py::str_or(b, "api_url", DEFAULT_API_URL, "strip")?).to_string();
        guarded(|| crate::console::session::save(&token, &refresh, &api))?
            .map_err(|e| py::oserror_full(&e, &[]))?;
        Ok((200, jobj! {"ok" => true})) // jamais d'écho du jeton
    })();
    r.unwrap_or_else(|e| (500, jobj! {"error" => e}))
}

/// POST /helper/console — ouvrir un terminal sur la console, jeton fourni.
fn console(b: &Value) -> Reply {
    let r = (|| -> Result<Reply, String> {
        obj(b)?;
        let token = py::strip(&py::str_or(b, "token", "", "strip")?).to_string();
        if token.is_empty() {
            return Ok((400, jobj! {"error" => "jeton manquant"}));
        }
        // Le navigateur passe AUSSI de quoi renouveler la session.
        let refresh = py::strip(&py::str_or(b, "refresh", "", "strip")?).to_string();
        let api = py::strip(&py::str_or(b, "api_url", DEFAULT_API_URL, "strip")?).to_string();
        if !refresh.is_empty() {
            if let Err(e) = guarded(|| crate::console::session::save(&token, &refresh, &api))? {
                eprintln!("[helper] session non conservée : {e}");
            }
        }
        let term = guarded(|| crate::console::spawn_terminal(&token, b))?;
        match term {
            None => Ok((503, jobj! {"error" => "aucun émulateur de terminal trouvé — définissez $TERMINAL"})),
            // On ne renvoie JAMAIS le jeton.
            Some(t) => Ok((200, jobj! {"ok" => true, "terminal" => t})),
        }
    })();
    r.unwrap_or_else(|e| (500, jobj! {"error" => e}))
}

/// `Path.mkdir(parents=True, exist_ok=True)`.
fn mkdir_p(p: &std::path::Path) -> Result<(), String> {
    match std::fs::create_dir(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match p.parent().filter(|q| !q.as_os_str().is_empty() && *q != p) {
                Some(parent) => mkdir_p(parent)?,
                None => return Err(py::oserror_full(&e, &[&p.to_string_lossy()])),
            }
            match std::fs::create_dir(p) {
                Ok(()) => Ok(()),
                Err(_) if p.is_dir() => Ok(()),
                Err(e) => Err(py::oserror_full(&e, &[&p.to_string_lossy()])),
            }
        }
        Err(_) if p.is_dir() => Ok(()),
        Err(e) => Err(py::oserror_full(&e, &[&p.to_string_lossy()])),
    }
}

/// GET /helper/disk?path= — espace libre de la partition des sorties.
fn disk(target: &str) -> Reply {
    let target = if target.is_empty() {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| iso::home().join(".cache"));
        base.join("redstars-helper").to_string_lossy().into_owned()
    } else {
        target.to_string()
    };
    let p = std::path::Path::new(&target);
    let r = mkdir_p(p).and_then(|_| fs2::statvfs(p).map_err(|e| py::oserror_full(&e, &[&target])));
    match r {
        Ok(st) => (
            200,
            jobj! {
                "ok" => true,
                "path" => target,
                "total" => st.total_space(),
                "free" => st.available_space(),
                "used" => st.total_space() - st.free_space(),
            },
        ),
        Err(e) => (500, jobj! {"error" => e}),
    }
}

fn lsusb() -> Reply {
    static LINE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^Bus (\d+) Device (\d+): ID (\S+) (.*)").expect("lsusb"));
    match super::proc::run(&super::proc::argv(&["lsusb"]), Some(Duration::from_secs(2))) {
        Err(super::proc::RunErr::Spawn(m)) if m.starts_with("FileNotFoundError") => {
            (500, jobj! {"error" => "lsusb not installed"})
        }
        Err(e) => (500, jobj! {"error" => e.msg()}),
        Ok(out) if out.code != 0 => {
            (500, jobj! {"error" => format!("lsusb exit {}", out.code), "stderr" => out.stderr})
        }
        Ok(out) => {
            let devices: Vec<J> = py::splitlines(&out.stdout)
                .into_iter()
                .filter_map(|l| LINE.captures(l))
                .map(|m| {
                    jobj! {
                        "bus" => &m[1],
                        "device" => &m[2],
                        "id" => &m[3],
                        "name" => py::strip(&m[4]),
                    }
                })
                .collect();
            let n = devices.len();
            (200, jobj! {"devices" => devices, "count" => n})
        }
    }
}

/// GET /helper/files/pick?mode=files|dirs — sélecteur natif (zenity/kdialog),
/// rend les chemins choisis sans copier les données.
fn files_pick(mode: &str) -> Reply {
    let home = iso::home().to_string_lossy().into_owned();
    let mode = if mode.is_empty() { "files".to_string() } else { mode.to_lowercase() };
    let tools: Vec<Vec<String>> = if mode == "dirs" {
        vec![
            super::proc::argv(&["zenity", "--file-selection", "--directory", "--multiple", "--separator=\n"]),
            super::proc::argv(&["kdialog", "--getexistingdirectory", &home]),
        ]
    } else {
        vec![
            super::proc::argv(&["zenity", "--file-selection", "--multiple", "--separator=\n"]),
            super::proc::argv(&["kdialog", "--multiple", "--getopenfilename", &home]),
        ]
    };
    let mut paths: Option<Vec<String>> = None;
    let mut err: Option<String> = None;
    for cmd in &tools {
        if !super::proc::which(&cmd[0]) {
            continue;
        }
        match super::proc::run(cmd, Some(Duration::from_secs(300))) {
            Ok(out) if out.code != 0 => {
                paths = Some(Vec::new()); // annulé : liste vide, pas une erreur
                break;
            }
            Ok(out) => {
                let sep = if cmd[0] == "zenity" { "\n" } else { " " };
                paths = Some(py::strip(&out.stdout).split(sep).filter(|p| !p.is_empty()).map(String::from).collect());
                break;
            }
            Err(super::proc::RunErr::Timeout(_)) => err = Some(format!("{} timeout", cmd[0])),
            Err(e) => err = Some(e.msg()),
        }
    }
    let Some(paths) = paths else {
        return (500, jobj! {"error" => err.unwrap_or_else(|| "no native picker (install zenity or kdialog)".into())});
    };
    let entries: Vec<J> = paths
        .iter()
        .map(|p| {
            let name = py::path_name(p);
            let ps = py::path_str(p);
            match std::fs::metadata(p) {
                Ok(st) => jobj! {"name" => name, "path" => ps, "size" => st.len(), "is_dir" => st.is_dir()},
                Err(_) => jobj! {"name" => name, "path" => ps, "size" => 0, "is_dir" => false, "missing" => true},
            }
        })
        .collect();
    let paths: Vec<J> = paths.into_iter().map(J::from).collect();
    (200, jobj! {"paths" => paths, "entries" => entries})
}

// ─── axum ────────────────────────────────────────────────────────────────────

async fn handle(req: Request<Body>) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            let mut r = Resp::json(413, &jobj! {"error" => "request body too large"});
            r.end_headers("", "");
            return r.into_response();
        }
    };
    // Chemin brut comme `self.path` ; http.server ramène « //x » à « /x ».
    let mut path = if parts.uri.scheme().is_some() {
        parts.uri.to_string()
    } else {
        parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| parts.uri.to_string())
    };
    if path.starts_with("//") {
        path = format!("/{}", path.trim_start_matches('/'));
    }
    let method = parts.method.as_str().to_string();
    let headers = parts.headers;
    let res = tokio::task::spawn_blocking(move || {
        dispatch(&Req { method: &method, path: &path, headers: &headers, body: &bytes })
    })
    .await;
    match res {
        Ok(r) => r.into_response(),
        Err(e) => {
            let mut r = Resp::json(500, &jobj! {"error" => format!("RuntimeError: {e}")});
            r.end_headers("", "/helper/");
            r.into_response()
        }
    }
}

fn app() -> Router {
    Router::new().fallback(handle)
}

/// Écoute TCP avec SO_REUSEADDR (Unix), retentée comme helper.py : le port peut
/// rester tenu un instant par le helper qu'on vient de remplacer.
async fn listen(addr: SocketAddr) -> std::io::Result<std::net::TcpListener> {
    let mut last = None;
    for attempt in 0..5u64 {
        let r = (|| {
            let s = tokio::net::TcpSocket::new_v4()?;
            #[cfg(unix)]
            s.set_reuseaddr(true)?;
            s.bind(addr)?;
            s.listen(128)?.into_std()
        })();
        match r {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("bind")))
}

fn port(var: &str, default: u16) -> u16 {
    std::env::var(var).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

// ─── TLS rechargeable ────────────────────────────────────────────────────────

static TLS: OnceLock<(tokio::runtime::Handle, RustlsConfig)> = OnceLock::new();

/// ALPN http/1.1 seulement, comme le helper Python (pas de h2).
fn http1_only(cfg: &RustlsConfig) -> Arc<rustls::ServerConfig> {
    let mut sc = (*cfg.get_inner()).clone();
    sc.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(sc)
}

/// Première source de certificat qui donne une config rustls valable.
async fn build_tls() -> Option<(String, Arc<rustls::ServerConfig>)> {
    for (src, (cert, key)) in tls::sources() {
        match RustlsConfig::from_pem(cert, key).await {
            Ok(cfg) => return Some((src, http1_only(&cfg))),
            Err(e) => eprintln!("  HTTPS: certificat de {src} refusé : {e}"),
        }
    }
    None
}

/// Relit le certificat (voir `tls::load_pem`) et l'applique aux connexions
/// suivantes, sans redémarrer le serveur. Rend la main tout de suite.
pub fn reload_tls() {
    let Some((rt, cfg)) = TLS.get() else { return };
    let cfg = cfg.clone();
    rt.spawn(async move {
        if let Some((src, sc)) = build_tls().await {
            cfg.reload_from_config(sc);
            println!("  HTTPS: certificat rechargé ({src})");
        }
    });
}

// ─── Démarrage ───────────────────────────────────────────────────────────────

async fn serve_all(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) -> i32 {
    let _ = rustls::crypto::ring::default_provider().install_default();
    mark_seen();
    refs::recover_from_disk();
    println!("redstars-helper {VERSION} (natif)");

    let mut tasks = Vec::new();

    let http_port = port("HELPER_PORT", 49080);
    match listen(SocketAddr::from(([0, 0, 0, 0], http_port))).await {
        Ok(l) => {
            println!("  HTTP  http://0.0.0.0:{http_port}/  +  /helper/*");
            let mut srv = axum_server::from_tcp(l);
            srv.http_builder().http1().title_case_headers(true);
            tasks.push(tokio::spawn(async move {
                if let Err(e) = srv.serve(app().into_make_service()).await {
                    eprintln!("  HTTP crashed: {e}");
                }
            }));
        }
        Err(e) => eprintln!(
            "redstars-helper {VERSION}: port {http_port} indisponible ({e}) — un helper tourne déjà ?"
        ),
    }

    let https_port = port("HELPER_HTTPS_PORT", 49443);
    match build_tls().await {
        None => println!("  HTTPS: skipped — aucun certificat utilisable"),
        Some((src, sc)) => match listen(SocketAddr::from(([127, 0, 0, 1], https_port))).await {
            Err(e) => eprintln!("  HTTPS: port {https_port} indisponible ({e})"),
            Ok(l) => {
                let cfg = RustlsConfig::from_config(sc);
                let _ = TLS.set((tokio::runtime::Handle::current(), cfg.clone()));
                println!("  HTTPS https://local.redlinks.fr:{https_port}/  +  /helper/*  (certificat : {src})");
                let mut srv = axum_server::from_tcp_rustls(l, cfg);
                srv.http_builder().http1().title_case_headers(true);
                tasks.push(tokio::spawn(async move {
                    if let Err(e) = srv.serve(app().into_make_service()).await {
                        eprintln!("  HTTPS crashed: {e}");
                    }
                }));
                // Le parc reçoit les certificats renouvelés via le helper.py signé
                // en cache : on relit régulièrement.
                tokio::spawn(async {
                    loop {
                        tokio::time::sleep(TLS_RELOAD_EVERY).await;
                        reload_tls();
                    }
                });
            }
        },
    }

    if tasks.is_empty() {
        eprintln!("redstars-helper: aucun port n'a pu être ouvert — rien à servir.");
        return 1;
    }
    start_idle_watchdog(on_idle_exit);
    for t in tasks {
        let _ = t.await;
    }
    0
}

/// Fait tourner les serveurs sur le fil courant (runtime tokio dédié) jusqu'à
/// leur fin. Rend 1 si aucun port n'a pu être ouvert.
pub fn run(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) -> i32 {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(64)
        .thread_name("helper-http")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("redstars-helper: runtime tokio impossible : {e}");
            return 1;
        }
    };
    rt.block_on(serve_all(on_idle_exit))
}

/// Démarre en arrière-plan ; rend la main immédiatement.
pub fn start(on_idle_exit: Box<dyn Fn() + Send + Sync + 'static>) {
    let r = std::thread::Builder::new().name("helper-server".into()).spawn(move || {
        run(on_idle_exit);
    });
    if let Err(e) = r {
        eprintln!("redstars-helper: fil serveur impossible : {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_rechargeable() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rt = tokio::runtime::Runtime::new().expect("rt");
        let (_, sc) = rt.block_on(build_tls()).expect("un certificat utilisable");
        assert_eq!(sc.alpn_protocols, vec![b"http/1.1".to_vec()]);
        let cfg = RustlsConfig::from_config(Arc::new((*sc).clone()));
        let before = Arc::as_ptr(&cfg.get_inner());
        let _ = TLS.set((rt.handle().clone(), cfg.clone()));
        reload_tls();
        rt.block_on(async {
            for _ in 0..50 {
                if Arc::as_ptr(&cfg.get_inner()) != before {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            panic!("config TLS non rechargée");
        });
    }
}
