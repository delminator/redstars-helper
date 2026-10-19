//! RedStars en console — portage natif du client terminal de helper.py
//! (`run_console`, `_con_*`, `_img_*`).
//!
//!     redhelper console --user X --app eau
//!     redhelper console --app eau --minitel      (40×24)
//!     redhelper console --app eau --accessible   (liste numérotée)
//!
//! Le client est DÉLIBÉRÉMENT BÊTE : il ne sait ni ce qu'est une app, ni un slot,
//! ni une colonne. Il envoie « je fais N colonnes, je suis ici, on a appuyé sur
//! ↓ » et imprime ce qu'on lui renvoie. Tout le savoir vit dans le core, à côté
//! des renderers React qui lisent les MÊMES descripteurs. Le jour où ce module
//! contient un `if app == "eau"`, l'architecture a échoué.
//!
//! Les requêtes (URL, query, corps, en-têtes) sont celles du client Python à
//! l'octet près ; `REDSTARS_CONSOLE_TRACE` les journalise (voir http.rs) et
//! `tests/parity_console.py` compare les deux clients.

pub mod http;
mod login;
pub mod session;
mod term;

use http::{pytruthy, pyval, ReqErr};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_APP_URL: &str = "https://dev.redstars.redlinks.fr";
pub const DEFAULT_API_URL: &str = "https://api.dev.redstars.redlinks.fr";

// ── Sortie en erreur (`sys.exit(msg)`) ───────────────────────────────────────

/// Une sortie anticipée : message sur stderr, code 1 — `sys.exit("…")` en Python,
/// ou la trace d'une exception non rattrapée (erreur réseau).
#[derive(Debug)]
struct Fail(String);

impl From<ReqErr> for Fail {
    fn from(e: ReqErr) -> Self {
        match e {
            ReqErr::Http { code, .. } => Fail(format!("Erreur HTTP {code}")),
            ReqErr::Other(m) => Fail(format!("Erreur réseau : {m}")),
        }
    }
}

type Res<T> = Result<T, Fail>;

// ── Arguments (l'argparse de run_console) ────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Args {
    pub app_url: String,
    pub api_url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub app: Option<String>,
    pub role: String,
    pub lang: String,
    pub cols: i64,
    pub rows: i64,
    pub minitel: bool,
    pub token_file: Option<String>,
    pub accessible: bool,
}

#[derive(Debug, PartialEq)]
pub enum ArgsOutcome {
    Help,
    Error(String),
}

const PROG: &str = "redhelper console";

/// (nom long, prend une valeur ?)
const OPTS: &[(&str, bool)] = &[
    ("--help", false),
    ("--app-url", true),
    ("--api-url", true),
    ("--user", true),
    ("--password", true),
    ("--app", true),
    ("--role", true),
    ("--lang", true),
    ("--cols", true),
    ("--rows", true),
    ("--minitel", false),
    ("--token-file", true),
    ("--accessible", false),
];

fn usage() -> String {
    format!(
        "usage: {PROG} [-h] [--app-url APP_URL] [--api-url API_URL] [--user USER]\n\
         {pad}[--password PASSWORD] [--app APP] [--role ROLE] [--lang LANG]\n\
         {pad}[--cols COLS] [--rows ROWS] [--minitel] [--accessible]\n",
        pad = " ".repeat(PROG.len() + 8)
    )
}

fn help_text() -> String {
    format!(
        "{}\nRedStars en console\n\n\
options:\n  \
-h, --help           show this help message and exit\n  \
--app-url APP_URL\n  \
--api-url API_URL\n  \
--user USER\n  \
--password PASSWORD\n  \
--app APP            id de l'app (eau, ludo…). Omis → on demande.\n  \
--role ROLE\n  \
--lang LANG\n  \
--cols COLS          0 = taille réelle du terminal\n  \
--rows ROWS\n  \
--minitel            force 40×24, comme un Minitel\n  \
--accessible         liste numérotée, texte pur, sans curseur — pour lecteur d'écran\n",
        usage()
    )
}

fn looks_negative_number(s: &str) -> bool {
    s.len() > 1 && s.starts_with('-') && s[1..].parse::<f64>().is_ok()
}

/// Analyse comme argparse : `--opt valeur`, `--opt=valeur`, préfixes non ambigus
/// (`--min` → `--minitel`), `-h`, la dernière occurrence gagne.
pub fn parse_args(argv: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<Args, ArgsOutcome> {
    let mut a = Args {
        app_url: env("REDSTARS_APP_URL").unwrap_or_else(|| DEFAULT_APP_URL.into()),
        api_url: env("REDSTARS_API_URL").unwrap_or_else(|| DEFAULT_API_URL.into()),
        user: None,
        password: None,
        app: None,
        role: "collaborator".into(),
        lang: "fr".into(),
        cols: 0,
        rows: 0,
        minitel: false,
        token_file: None,
        accessible: false,
    };
    let mut unrecognized: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        i += 1;
        if arg == "-h" {
            return Err(ArgsOutcome::Help);
        }
        if !arg.starts_with("--") || arg == "--" {
            unrecognized.push(arg.clone());
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let opt = match OPTS.iter().find(|(o, _)| *o == name) {
            Some(o) => *o,
            None => {
                let hits: Vec<_> = OPTS.iter().filter(|(o, _)| o.starts_with(name)).collect();
                match hits.len() {
                    1 => *hits[0],
                    0 => {
                        unrecognized.push(arg.clone());
                        continue;
                    }
                    _ => {
                        let names: Vec<_> = hits.iter().map(|(o, _)| *o).collect();
                        return Err(ArgsOutcome::Error(format!(
                            "ambiguous option: {name} could match {}",
                            names.join(", ")
                        )));
                    }
                }
            }
        };
        let (oname, takes) = opt;
        let value = if takes {
            match inline {
                Some(v) => v,
                None => match argv.get(i) {
                    Some(v) if !v.starts_with('-') || looks_negative_number(v) => {
                        i += 1;
                        v.clone()
                    }
                    _ => return Err(ArgsOutcome::Error(format!("argument {oname}: expected one argument"))),
                },
            }
        } else {
            if inline.is_some() {
                return Err(ArgsOutcome::Error(format!("argument {oname}: ignored explicit argument")));
            }
            String::new()
        };
        let int = |v: &str| -> Result<i64, ArgsOutcome> {
            v.trim()
                .replace('_', "")
                .parse::<i64>()
                .map_err(|_| ArgsOutcome::Error(format!("argument {oname}: invalid int value: '{v}'")))
        };
        match oname {
            "--help" => return Err(ArgsOutcome::Help),
            "--app-url" => a.app_url = value,
            "--api-url" => a.api_url = value,
            "--user" => a.user = Some(value),
            "--password" => a.password = Some(value),
            "--app" => a.app = Some(value),
            "--role" => a.role = value,
            "--lang" => a.lang = value,
            "--cols" => a.cols = int(&value)?,
            "--rows" => a.rows = int(&value)?,
            "--minitel" => a.minitel = true,
            "--token-file" => a.token_file = Some(value),
            "--accessible" => a.accessible = true,
            _ => unreachable!(),
        }
    }
    if !unrecognized.is_empty() {
        return Err(ArgsOutcome::Error(format!("unrecognized arguments: {}", unrecognized.join(" "))));
    }
    Ok(a)
}

/// `Some("")` → None : la vérité Python d'une option chaîne.
fn nonempty(o: &Option<String>) -> Option<String> {
    o.clone().filter(|s| !s.is_empty())
}

// ── Panique, nettoyage ───────────────────────────────────────────────────────

thread_local! {
    static QUIET_PANIC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Crochet de panique : remet le terminal d'aplomb avant le message. Muet pendant
/// les appels au décodeur d'images (voir `quiet_catch`).
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if QUIET_PANIC.with(|q| q.get()) {
                return;
            }
            term::force_restore();
            prev(info);
        }));
    });
}

/// Appelle `f` en traitant une panique comme un échec (Err) — le décodeur
/// d'images peut ne pas être là (stub, modèle absent), et le repli texte du
/// serveur doit alors rester, exactement comme quand onnxruntime manquait au
/// Python. NB : sans effet si le binaire est compilé `panic = "abort"`.
fn quiet_catch<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    QUIET_PANIC.with(|q| q.set(true));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    QUIET_PANIC.with(|q| q.set(false));
    r.map_err(|_| ())
}

/// Ce que Python confiait à `atexit` en mode Minitel : police d'origine, puis
/// taille de fenêtre d'origine (ordre LIFO de atexit).
#[derive(Default)]
struct Cleanup {
    resize: Option<(i64, i64)>,
    font: Option<PathBuf>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(old) = self.font.take() {
            bigfont_restore(&old);
        }
        if let Some((c, r)) = self.resize.take() {
            term::out(&format!("\x1b[8;{r};{c}t"));
            term::flush();
        }
    }
}

// ── Point d'entrée ───────────────────────────────────────────────────────────

/// `redhelper console [args…]` / `minitel` : lance le client dans le terminal
/// courant et rend le code de sortie. `args` = ce qui suit `console`.
pub fn run(args: &[String]) -> i32 {
    install_panic_hook();
    let mut a = match parse_args(args, &|k| std::env::var(k).ok()) {
        Ok(a) => a,
        Err(ArgsOutcome::Help) => {
            print!("{}", help_text());
            return 0;
        }
        Err(ArgsOutcome::Error(m)) => {
            eprint!("{}", usage());
            eprintln!("{PROG}: error: {m}");
            return 2;
        }
    };
    let mut cleanup = Cleanup::default();
    let code = match run_console(&mut a, &mut cleanup) {
        Ok(()) => 0,
        Err(Fail(m)) => {
            term::flush();
            if !m.is_empty() {
                eprintln!("{m}");
            }
            1
        }
    };
    drop(cleanup);
    term::flush();
    code
}

/// Contexte d'affichage commun aux boucles de trame.
struct View<'a> {
    app_url: &'a str,
    token: &'a str,
    role: &'a str,
    lang: &'a str,
    cols: i64,
    rows: i64,
    center: bool,
}

fn run_console(a: &mut Args, cleanup: &mut Cleanup) -> Res<()> {
    // La taille du terminal n'est JAMAIS une condition d'existence : repli 80×24.
    let (tc, tr) = term::size().unwrap_or((80, 24));
    let (cols, rows) = if a.minitel {
        (40, 24)
    } else {
        (if a.cols != 0 { a.cols } else { tc }, if a.rows != 0 { a.rows } else { tr })
    };

    // Profondeur de couleur et capacité image, négociées une fois.
    let colorterm = std::env::var("COLORTERM").ok();
    let truecolor = http::color_depth(a.minitel, colorterm.as_deref()) == "truecolor";
    http::set_truecolor(truecolor);
    http::set_img(!a.minitel && truecolor && img_capable());

    // Mode Minitel : fenêtre 40×24 (émulateurs) + grosse police (console Linux),
    // restaurées à la sortie.
    let mut center = false;
    if a.minitel && term::stdout_is_tty() {
        term::out("\x1b[8;24;40t");
        term::flush();
        cleanup.resize = Some((tc, tr));
        cleanup.font = bigfont_setup();
        center = true;
    }

    // Le jeton passé par le navigateur : fichier 0600 (lu PUIS supprimé avant
    // toute autre chose), sinon l'environnement. Jamais argv.
    let mut token = String::new();
    if let Some(tf) = &a.token_file {
        token = std::fs::read_to_string(tf).map(|s| s.trim().to_string()).unwrap_or_default();
        let _ = std::fs::remove_file(tf);
    }
    let env_tok = std::env::var("REDSTARS_TOKEN").unwrap_or_default();
    std::env::remove_var("REDSTARS_TOKEN");
    if token.is_empty() {
        token = env_tok;
    }

    // Puis la session gardée ; le mot de passe en tout dernier recours.
    if token.is_empty() {
        token = session::load(&a.api_url).unwrap_or_default();
    }

    if token.is_empty() {
        let user0 = nonempty(&a.user);
        let pw0 = nonempty(&a.password);
        let creds = if user0.is_none() && pw0.is_none() {
            login::screen(&mut a.accessible, &a.lang, a.minitel, false)
        } else {
            login::LoginOut::Fallback
        };
        let (user, pw) = match creds {
            login::LoginOut::Creds(u, p) => (u, p),
            _ => {
                let user = match user0 {
                    Some(u) => u,
                    None => term::input("Utilisateur : ").ok_or_else(|| Fail(String::new()))?,
                };
                let pw = match pw0 {
                    Some(p) => p,
                    None => rpassword::prompt_password("Mot de passe : ").map_err(|_| Fail(String::new()))?,
                };
                (user, pw)
            }
        };
        let body = http::py_json_obj(&[("username", &user), ("password", &pw)]);
        let d = match http::req_json(&format!("{}/api/v1/auth/login", a.api_url), Some(&body), &[], None, 30) {
            Ok(d) => d,
            Err(ReqErr::Http { code, .. }) => return Err(Fail(format!("Connexion refusée (HTTP {code})."))),
            Err(e) => return Err(e.into()),
        };
        token = d
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Fail("Connexion : réponse sans access_token.".into()))?
            .to_string();
        let refresh = d.get("refresh_token").and_then(|v| v.as_str()).unwrap_or("");
        if let Err(e) = session::save(&token, refresh, &a.api_url) {
            eprintln!("[console] session non enregistrée : {e}");
        }
    } else if matches!(login::screen(&mut a.accessible, &a.lang, a.minitel, true), login::LoginOut::Quit) {
        return Ok(());
    }

    let v = View {
        app_url: &a.app_url,
        token: &token,
        role: &a.role,
        lang: &a.lang,
        cols,
        rows,
        center,
    };

    // Le DASHBOARD (toutes les apps de toutes les orgs), puis l'app, son menu,
    // la rubrique. « Retour » remonte d'UN cran.
    let pinned = nonempty(&a.app).is_some();
    loop {
        let mut app = nonempty(&a.app);
        let mut org_oid: Option<String> = None;
        if app.is_none() {
            if a.accessible {
                let (ap, o) = home_accessible(&v)?;
                app = ap;
                org_oid = o;
            } else {
                match con_run(&v, None, None, None, None, None, None)? {
                    RunOut::Action(act) => {
                        app = pytruthy(act.get("to"));
                        org_oid = pytruthy(act.get("org"));
                    }
                    _ => return Ok(()),
                }
            }
            if app.is_none() {
                return Ok(());
            }
        }
        let app = app.unwrap();
        if org_oid.is_none() {
            org_oid = choose_org(&v, a.accessible, &app)?;
            if org_oid.is_none() {
                if pinned {
                    return Ok(());
                }
                continue;
            }
        }
        let oid = org_oid.unwrap();
        let org_name = "";

        // Le sommaire.
        let s = http::frame(
            v.app_url,
            v.token,
            &[("app", Some(app.clone())), ("org", Some(oid.clone())), ("role", Some(a.role.clone()))],
        )?;
        if let Some(e) = s.get("error") {
            return Err(Fail(pyval(Some(e)).unwrap_or_default()));
        }

        let menu = build_menu(&s);
        let menu_text = |m: &Value| menu_label(m, &a.lang);

        if a.accessible {
            println!("\n{app} — {org_name}.");
            println!("{} rubriques.\n", menu.len());
            for (i, m) in menu.iter().enumerate() {
                println!("{}. {}", i + 1, menu_text(m));
            }
            println!("\nTapez un numéro de 1 à {} puis Entrée. 0 pour quitter.", menu.len());
            let Some(n) = term::input("> ") else { return Ok(()) };
            let Some(k) = parse_choice(&n, menu.len()) else { return Ok(()) };
            let slot = pyval(menu[k - 1].get("id"));
            run_accessible(&v, &app, &oid, slot)?;
            return Ok(());
        }

        let menu_title = pytruthy(s.get("title")).unwrap_or_else(|| app.clone());
        let hdr = menu_title; // pas de nom d'org : le suffixe « — org » ne s'ajoute pas

        loop {
            match pick(&menu, &hdr, &menu_text) {
                Pick::Quit => return Ok(()),
                Pick::Back => break,
                Pick::Item(k) => {
                    let slot = pyval(menu[k].get("id"));
                    let r = con_run(&v, Some(app.clone()), Some(oid.clone()), slot, None, None, None)?;
                    if matches!(r, RunOut::Quit) {
                        return Ok(());
                    }
                }
            }
        }
        if pinned {
            return Ok(());
        }
    }
}

/// Le menu de l'app (`menu`, ou les `slots` d'une vieille trame), entrées
/// `align: bottom` en pied.
fn build_menu(s: &Value) -> Vec<Value> {
    let menu: Vec<Value> = match s.get("menu").and_then(|m| m.as_array()) {
        Some(m) if !m.is_empty() => m.clone(),
        _ => s
            .get("slots")
            .and_then(|x| x.as_array())
            .map(|slots| {
                slots
                    .iter()
                    .map(|x| {
                        let id = x.get("id").cloned().unwrap_or(Value::Null);
                        serde_json::json!({"id": id, "label": {"fr": id, "en": id}, "kind": x.get("kind")})
                    })
                    .collect()
            })
            .unwrap_or_default(),
    };
    let bottom = |m: &Value| m.get("align").and_then(|x| x.as_str()) == Some("bottom");
    let (top, bot): (Vec<Value>, Vec<Value>) = menu.into_iter().partition(|m| !bottom(m));
    top.into_iter().chain(bot).collect()
}

/// `_menu_text` : libellé localisé, icône devant.
fn menu_label(m: &Value, lang: &str) -> String {
    let lab = m.get("label");
    let txt = match lab {
        Some(Value::Object(o)) => pytruthy(o.get(lang))
            .or_else(|| pytruthy(o.get("fr")))
            .or_else(|| pyval(m.get("id")))
            .unwrap_or_else(|| "?".into()),
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => pyval(m.get("id")).unwrap_or_else(|| "?".into()),
    };
    let ico = m.get("icon").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    if ico.is_empty() {
        txt
    } else {
        format!("{ico} {txt}").trim().to_string()
    }
}

/// `n.isdigit() and 1 <= int(n) <= max` (après strip).
fn parse_choice(n: &str, max: usize) -> Option<usize> {
    let n = n.trim();
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let k: usize = n.parse().ok()?;
    (1..=max).contains(&k).then_some(k)
}

// ── Le menu minimal (`_con_pick`) ────────────────────────────────────────────

enum Pick {
    Item(usize),
    Back,
    Quit,
}

fn pick(items: &[Value], label: &str, render: &dyn Fn(&Value) -> String) -> Pick {
    let mut i = 0usize;
    let _raw = term::Raw::enter();
    loop {
        let w = term::size().map(|(c, _)| c).unwrap_or(80).max(0) as usize;
        let inner = w.saturating_sub(3).max(4);
        let mut s = format!("\x1b[2J\x1b[H\x1b[1;36m{}\x1b[0m\r\n\r\n", login::take(label, w));
        for (k, it) in items.iter().enumerate() {
            let sel = k == i;
            let mut line = login::take(&render(it), inner);
            if sel {
                line = login::ljust(&line, inner);
            }
            let mark = if sel { "\x1b[7m" } else { "" };
            s.push_str(&format!("  {mark}{line}\x1b[0m\r\n"));
        }
        s.push_str("\r\n\x1b[2m↑ ↓ choisir · ↵ valider · Échap retour · q quitter\x1b[0m\r\n");
        term::out(&s);
        term::flush();
        let k = match term::next_key() {
            Some(k) => term::nav_key(&k),
            None => Some("quit"),
        };
        let n = items.len();
        match k {
            Some("up") if n > 0 => i = (i + n - 1) % n,
            Some("down") if n > 0 => i = (i + 1) % n,
            Some("enter") if n > 0 => return Pick::Item(i),
            Some("quit") => return Pick::Quit,
            Some("back") => return Pick::Back,
            _ => {}
        }
    }
}

// ── La boucle de trame (`_con_run`) ──────────────────────────────────────────

#[derive(Debug)]
enum RunOut {
    Back,
    Quit,
    Action(Value),
}

/// Égalité « Python » d'un `n` de focusable : 1 == 1.0.
fn same_n(a: Option<&Value>, b: &Value) -> bool {
    match (a, b) {
        (Some(Value::Number(x)), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Some(x), y) => x == y,
        (None, _) => false,
    }
}

/// Le déplacement du curseur : rend le nouveau `sel` (le NUMÉRO du focusable,
/// pas sa ligne). `here` = index du focusable courant (0 si introuvable).
fn move_sel(foc: &[Value], sel: &Value, key: &str, rows: i64) -> Option<Value> {
    if foc.is_empty() {
        return None;
    }
    let here = foc.iter().position(|x| same_n(x.get("n"), sel)).unwrap_or(0);
    let step = (rows - 5).max(1) as usize;
    let last = foc.len() - 1;
    let idx = match key {
        "down" => (here + 1).min(last),
        "up" => here.saturating_sub(1),
        "next" => (here + step).min(last),
        "prev" => here.saturating_sub(step),
        _ => return None,
    };
    Some(foc[idx].get("n").cloned().unwrap_or(Value::Null))
}

fn con_run(
    v: &View,
    app: Option<String>,
    org: Option<String>,
    slot: Option<String>,
    modal: Option<String>,
    item: Option<String>,
    src: Option<String>,
) -> Res<RunOut> {
    let mut sel = Value::from(1);
    let _raw = term::Raw::enter();
    // Comme le `finally` Python : écran effacé à la sortie.
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            term::out("\x1b[2J\x1b[H");
            term::flush();
        }
    }
    let _clear = Clear;
    loop {
        let f = http::frame(
            v.app_url,
            v.token,
            &[
                ("app", app.clone()),
                ("org", org.clone()),
                ("role", Some(v.role.to_string())),
                ("slot", slot.clone()),
                ("modal", modal.clone()),
                ("item", item.clone()),
                ("src", src.clone()),
                ("sel", pyval(Some(&sel))),
                ("cols", Some(v.cols.to_string())),
                ("rows", Some(v.rows.to_string())),
                ("lang", Some(v.lang.to_string())),
            ],
        )?;
        if let Some(e) = f.get("error") {
            let msg = pyval(Some(e)).unwrap_or_default();
            term::out(&format!("\x1b[2J\x1b[H\x1b[31m{}\x1b[0m\r\n\r\n", msg.replace('\n', "\r\n")));
            term::out("\x1b[2mUne touche pour revenir…\x1b[0m");
            term::flush();
            let _ = term::next_key();
            return Ok(RunOut::Back);
        }

        let ansi = f.get("ansi").and_then(|x| x.as_str()).unwrap_or("");
        if v.center {
            term::out(&center_frame(ansi, v.cols, v.rows, term::size()));
        } else {
            term::out(&ansi.replace('\n', "\r\n"));
        }
        if let Some(imgs) = f.get("images").and_then(|x| x.as_array()).filter(|x| !x.is_empty()) {
            let (t, l) = offset(v.cols, v.rows, v.center, term::size());
            blit_images(imgs, t, l);
        }
        term::flush();

        let k = match term::next_key() {
            Some(k) => term::nav_key(&k),
            None => Some("quit"),
        };
        let empty = Vec::new();
        let foc = f.get("focusables").and_then(|x| x.as_array()).unwrap_or(&empty);
        match k {
            Some(dir @ ("down" | "up" | "next" | "prev")) => {
                if let Some(n) = move_sel(foc, &sel, dir, v.rows) {
                    sel = n;
                }
            }
            Some("enter") => {
                let here = foc.iter().position(|x| same_n(x.get("n"), &sel)).unwrap_or(0);
                if here < foc.len() {
                    let act = foc[here].get("action").cloned().unwrap_or(Value::Null);
                    let act = if act.is_object() { act } else { serde_json::json!({}) };
                    match act.get("kind").and_then(|x| x.as_str()) {
                        Some("route") => return Ok(RunOut::Action(act)),
                        Some("open-modal") => {
                            let sub = con_run(
                                v,
                                app.clone(),
                                org.clone(),
                                None,
                                pyval(act.get("modal")),
                                pyval(act.get("itemId")),
                                slot.clone(),
                            )?;
                            if matches!(sub, RunOut::Quit) {
                                return Ok(RunOut::Quit);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("back") => return Ok(RunOut::Back),
            Some("quit") => return Ok(RunOut::Quit),
            _ => {}
        }
    }
}

// ── Centrage (mode Minitel) ──────────────────────────────────────────────────

/// `_con_center` : re-pose chaque ligne de la trame à une position absolue pour
/// centrer le cadre `cols`×`rows` dans le terminal réel `real` (relu à chaque
/// trame : setfont a pu changer la géométrie).
pub fn center_frame(ansi: &str, cols: i64, rows: i64, real: Option<(i64, i64)>) -> String {
    let (rc, rr) = real.unwrap_or((cols, rows));
    let left = ((rc - cols) / 2).max(0);
    let top = ((rr - rows) / 2).max(0);
    let body = ansi.replace("\x1b[2J", "").replace("\x1b[H", "");
    let mut out = String::from("\x1b[2J");
    for (i, ln) in body.split('\n').enumerate() {
        out.push_str(&format!("\x1b[{};{}H{}", top + i as i64 + 1, left + 1, ln.trim_end_matches('\r')));
    }
    out
}

/// `_con_offset` : coin haut-gauche (ligne, colonne) du cadre.
pub fn offset(cols: i64, rows: i64, center: bool, real: Option<(i64, i64)>) -> (i64, i64) {
    if !center {
        return (0, 0);
    }
    let (rc, rr) = real.unwrap_or((cols, rows));
    (((rr - rows) / 2).max(0), ((rc - cols) / 2).max(0))
}

// ── Images nn: (décodeur local, crate::img) ──────────────────────────────────

static IMG_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn img_capable() -> bool {
    static CAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| match quiet_catch(crate::img::capable) {
        Ok(c) => c,
        Err(()) => {
            IMG_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
            false
        }
    })
}

fn img_render(h: &str, w: usize, hc: usize) -> Option<Vec<String>> {
    if IMG_BROKEN.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    match quiet_catch(|| crate::img::render_tile(h, w, hc)) {
        Ok(r) => r,
        Err(()) => {
            IMG_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
            None
        }
    }
}

/// `int(x)` Python sur un champ JSON (nombre tronqué, ou chaîne d'entier).
fn py_int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0).trunc() as i64),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        Some(Value::Bool(b)) => *b as i64,
        _ => 0,
    }
}

/// `_con_blit_images` : pose les tuiles décodées localement par-dessus la trame.
/// Sans décodeur, on ne touche à rien : le repli texte du serveur reste.
fn blit_images(images: &[Value], top: i64, left: i64) {
    for im in images {
        let Some(h) = im.get("hash").and_then(|x| x.as_str()).filter(|s| !s.is_empty()) else { continue };
        let (wc, hc) = (py_int(im.get("w")), py_int(im.get("h")));
        let (row, col) = (py_int(im.get("row")), py_int(im.get("col")));
        if wc <= 0 || hc <= 0 {
            continue;
        }
        let Some(lines) = img_render(h, wc as usize, hc as usize) else { continue };
        for (ri, ln) in lines.iter().enumerate() {
            term::out(&format!("\x1b[{};{}H{}", top + row + ri as i64 + 1, left + col + 1, ln));
        }
    }
}

// ── Organisation ─────────────────────────────────────────────────────────────

/// `_con_resolve_org` : l'org d'une app mono-org, lue sur sa tuile du dashboard.
fn resolve_org(v: &View, app: &str) -> Option<String> {
    let f = http::frame(v.app_url, v.token, &[("role", Some("collaborator".into()))]).ok()?;
    for foc in f.get("focusables").and_then(|x| x.as_array())? {
        let act = foc.get("action");
        let to = act.and_then(|a| a.get("to"));
        if to.and_then(|t| t.as_str()) == Some(app) {
            if let Some(o) = pytruthy(act.and_then(|a| a.get("org"))) {
                return Some(o);
            }
        }
    }
    None
}

/// `_con_choose_org` : l'org de l'app, résolue après le clic, seulement si besoin.
fn choose_org(v: &View, accessible: bool, app: &str) -> Res<Option<String>> {
    let probe = http::frame(
        v.app_url,
        v.token,
        &[("app", Some(app.into())), ("role", Some(v.role.into())), ("lang", Some(v.lang.into()))],
    )?;
    if probe.get("slots").is_some() {
        return Ok(resolve_org(v, app));
    }
    if let Some(e) = probe.get("error") {
        if probe.get("focusables").is_none() {
            println!("{}", pyval(Some(e)).unwrap_or_else(|| "None".into()));
            return Ok(None);
        }
    }
    if accessible {
        let act = pick_frame_accessible(v, Some(app))?;
        return Ok(act.and_then(|a| pytruthy(a.get("org"))));
    }
    match con_run(v, Some(app.into()), None, None, None, None, None)? {
        RunOut::Action(act) => Ok(pytruthy(act.get("org"))),
        _ => Ok(None),
    }
}

// ── Mode accessible ──────────────────────────────────────────────────────────

/// `_con_pick_frame_accessible` : un écran de choix en liste numérotée (texte
/// `a11y=1`), l'action du numéro lue sur la trame JSON.
fn pick_frame_accessible(v: &View, app: Option<&str>) -> Res<Option<Value>> {
    let mut q: Vec<(&str, String)> = vec![("role", v.role.into()), ("lang", v.lang.into()), ("a11y", "1".into())];
    if let Some(ap) = app {
        q.push(("app", ap.into()));
    }
    let url = http::frame_url(v.app_url, &q);
    let txt = match http::get_text(&url, v.token) {
        Ok(t) => t,
        Err(e @ ReqErr::Http { .. }) => {
            println!("{}", e.server_error("Erreur HTTP "));
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    println!();
    println!("{txt}");

    let mut fq: Vec<(&str, Option<String>)> = vec![("role", Some(v.role.into())), ("lang", Some(v.lang.into()))];
    if let Some(ap) = app {
        fq.push(("app", Some(ap.into())));
    }
    let f = http::frame(v.app_url, v.token, &fq)?;
    let empty = Vec::new();
    let foc = f.get("focusables").and_then(|x| x.as_array()).unwrap_or(&empty);
    let Some(n) = term::input("> ") else { return Ok(None) };
    let Some(k) = parse_choice(&n, foc.len()) else { return Ok(None) };
    let act = foc[k - 1].get("action").cloned().unwrap_or(Value::Null);
    Ok(Some(if act.is_object() { act } else { serde_json::json!({}) }))
}

/// `_con_home_accessible` : le dashboard numéroté → (app, org).
fn home_accessible(v: &View) -> Res<(Option<String>, Option<String>)> {
    let act = pick_frame_accessible(v, None)?.unwrap_or_else(|| serde_json::json!({}));
    Ok((pytruthy(act.get("to")), pytruthy(act.get("org"))))
}

/// `_con_modal_accessible` : le détail en texte pur.
fn modal_accessible(v: &View, app: &str, org: &str, modal: Option<String>, item: Option<String>, src: Option<String>) -> Res<()> {
    let q: Vec<(&str, String)> = [
        ("app", Some(app.to_string())),
        ("org", Some(org.to_string())),
        ("role", Some(v.role.to_string())),
        ("modal", modal),
        ("item", item),
        ("src", src),
        ("lang", Some(v.lang.to_string())),
        ("a11y", Some("1".to_string())),
    ]
    .into_iter()
    .filter_map(|(k, x)| x.map(|x| (k, x)))
    .collect();
    let url = http::frame_url(v.app_url, &q);
    let txt = match http::get_text(&url, v.token) {
        Ok(t) => t,
        Err(e @ ReqErr::Http { .. }) => {
            println!("{}", e.server_error("Erreur HTTP "));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    println!();
    println!("{txt}");
    let _ = term::input("\n(Entrée pour revenir) ");
    Ok(())
}

/// `_con_run_accessible` : la rubrique en liste numérotée ; on tape un numéro.
fn run_accessible(v: &View, app: &str, org: &str, slot: Option<String>) -> Res<()> {
    let mut intro = "1";
    loop {
        // Pas de filtre des None ici, comme en Python (urlencode d'un dict brut).
        let q: Vec<(&str, String)> = vec![
            ("app", app.into()),
            ("org", org.into()),
            ("role", v.role.into()),
            ("slot", slot.clone().unwrap_or_else(|| "None".into())),
            ("lang", v.lang.into()),
            ("a11y", "1".into()),
            ("intro", intro.into()),
        ];
        let url = http::frame_url(v.app_url, &q);
        let txt = match http::get_text(&url, v.token) {
            Ok(t) => t,
            Err(e @ ReqErr::Http { .. }) => {
                println!("{}", e.server_error("Erreur HTTP "));
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        println!();
        println!("{txt}");
        intro = "0";

        let Some(answer) = term::input("> ") else { return Ok(()) };
        let answer = answer.trim().to_lowercase();
        if answer == "0" || answer == "q" || answer.is_empty() {
            return Ok(());
        }
        if answer == "n" {
            println!("La création n'est pas encore écrite.");
            continue;
        }
        if answer.chars().all(|c| c.is_ascii_digit()) {
            let f = http::frame(
                v.app_url,
                v.token,
                &[
                    ("app", Some(app.into())),
                    ("org", Some(org.into())),
                    ("role", Some(v.role.into())),
                    ("slot", slot.clone()),
                    ("lang", Some(v.lang.into())),
                ],
            )?;
            let want: Value = answer.parse::<u64>().map(Value::from).unwrap_or(Value::Null);
            let hit = f
                .get("focusables")
                .and_then(|x| x.as_array())
                .and_then(|foc| foc.iter().find(|x| same_n(x.get("n"), &want)).cloned());
            let Some(hit) = hit else {
                println!("Il n'y a pas d'élément numéro {answer}.");
                continue;
            };
            let act = hit.get("action").cloned().unwrap_or(Value::Null);
            if act.get("kind").and_then(|x| x.as_str()) == Some("open-modal") {
                modal_accessible(v, app, org, pyval(act.get("modal")), pyval(act.get("itemId")), slot.clone())?;
                continue;
            }
            println!("\n{}", hit.get("speech").and_then(|x| x.as_str()).unwrap_or(""));
            println!("(pas d'action disponible ici)");
            continue;
        }
        println!("Tapez un numéro, ou 0 pour revenir.");
    }
}

// ── Grosse police (console Linux, mode Minitel) ──────────────────────────────

/// `_con_bigfont_setup` : sur une VRAIE console Linux (TERM=linux), charge une
/// grosse police avec `setfont` en sauvant l'ancienne. Rend le chemin de la
/// sauvegarde (à restaurer en sortie), ou None. No-op ailleurs.
fn bigfont_setup() -> Option<PathBuf> {
    if std::env::var("TERM").ok().as_deref() != Some("linux") || !term::stdout_is_tty() {
        return None;
    }
    which::which("setfont").ok()?;
    let old = std::env::temp_dir().join(format!("redstars-oldfont-{}.psf", std::process::id()));
    for font in [
        "ter-v32b",
        "latarcyrheb-sun32",
        "Uni3-TerminusBold32x16",
        "Lat15-Terminus32x16",
        "Lat2-Terminus32x16",
        "Uni2-Terminus32x16",
        "sun12x22",
        "ter-v24b",
        "ter-v22b",
    ] {
        let st = Command::new("setfont")
            .arg("-o")
            .arg(&old)
            .arg(font)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if st.success() {
            return Some(old);
        }
    }
    None
}

fn bigfont_restore(old: &Path) {
    let _ = Command::new("setfont")
        .arg(old)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = std::fs::remove_file(old);
}

// ── Handoff console : le navigateur a le jeton, nous avons le terminal ───────
//
// Le jeton ne passe JAMAIS par argv (lisible par tous via `ps`) : il est déposé
// dans un fichier 0600 sous $XDG_RUNTIME_DIR, dont seul le CHEMIN voyage dans la
// ligne de commande ; le client le lit et le supprime aussitôt. Il est aussi mis
// dans l'environnement du terminal (REDSTARS_TOKEN) pour les terminaux qui ne
// sont pas activés par D-Bus.

/// (nom, préfixe d'args, activé par D-Bus ?)
const TERMINALS: &[(&str, &[&str], bool)] = &[
    ("foot", &["-e"], false),
    ("alacritty", &["-e"], false),
    ("ghostty", &["-e"], false),
    ("kitty", &[], false),
    ("wezterm", &["start", "--"], false),
    ("ptyxis", &["--"], true),
    ("kgx", &["-e"], true),
    ("gnome-terminal", &["--"], true),
    ("konsole", &["-e"], false),
    ("xfce4-terminal", &["-x"], false),
    ("tilix", &["-e"], false),
    ("terminator", &["-x"], false),
    ("mate-terminal", &["--"], true),
    ("lxterminal", &["-e"], false),
    ("deepin-terminal", &["-e"], false),
    ("urxvt", &["-e"], false),
    ("st", &["-e"], false),
    ("xterm", &["-e"], false),
    ("x-terminal-emulator", &["-e"], false),
];

fn bindirs() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    vec![
        "/usr/bin".into(),
        "/usr/local/bin".into(),
        "/bin".into(),
        "/opt/bin".into(),
        home.join(".local/bin"),
        "/var/lib/flatpak/exports/bin".into(),
        home.join(".local/share/flatpak/exports/bin"),
        "/snap/bin".into(),
    ]
}

/// `_con_find` : trouve un binaire même quand PATH est amputé (lancement par le
/// bureau).
pub fn find(term: &str) -> Option<PathBuf> {
    if let Ok(p) = which::which(term) {
        return Some(p);
    }
    bindirs().into_iter().map(|d| d.join(term)).find(|c| is_executable(c))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.is_file() && std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// `shlex.quote`.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// Le binaire à relancer pour la console : l'AppImage lui-même s'il y en a un
/// (le binaire monté dans /tmp/.mount_* n'a pas ses bibliothèques hors de
/// l'AppRun), sinon l'exécutable courant.
pub fn self_exe() -> PathBuf {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let p = PathBuf::from(appimage);
        if p.is_file() {
            return p;
        }
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("redstars-helper"))
}

/// Retire de l'environnement d'un terminal lancé par nous les variables que
/// l'AppRun d'une AppImage injecte (elles casseraient un émulateur système).
pub fn clean_env(c: &mut Command) {
    if std::env::var_os("APPIMAGE").is_some() {
        for var in ["LD_LIBRARY_PATH", "LD_PRELOAD", "PYTHONHOME", "PYTHONPATH"] {
            c.env_remove(var);
        }
    }
}

/// `_con_token_file` : dépose le jeton dans un fichier 0600 (création exclusive)
/// sous $XDG_RUNTIME_DIR, ou le répertoire temporaire.
fn token_file(token: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let d = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|x| !x.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let hex: String = uuid::Uuid::new_v4().simple().to_string().chars().take(16).collect();
    let path = d.join(format!("redstars-console-{hex}.tok"));
    let mut oo = std::fs::OpenOptions::new();
    oo.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        oo.mode(0o600);
    }
    let mut f = oo.open(&path)?;
    f.write_all(token.as_bytes())?;
    Ok(path)
}

/// La valeur d'une option JSON si elle est « vraie » au sens Python, en `str()`.
fn opt_str(opts: &Value, k: &str) -> Option<String> {
    pytruthy(opts.get(k))
}

/// La ligne de commande lancée dans le terminal (sans le jeton, jamais).
fn console_argv(exe: &Path, tokfile: &Path, opts: &Value) -> Vec<String> {
    let mut argv = vec![
        exe.to_string_lossy().to_string(),
        "console".into(),
        "--token-file".into(),
        tokfile.to_string_lossy().to_string(),
    ];
    for k in ["app", "role", "lang", "app-url", "api-url"] {
        if let Some(v) = opt_str(opts, &k.replace('-', "_")) {
            argv.push(format!("--{k}"));
            argv.push(v);
        }
    }
    if opt_str(opts, "minitel").is_some() {
        argv.push("--minitel".into());
    }
    if opt_str(opts, "accessible").is_some() {
        argv.push("--accessible".into());
    }
    argv
}

/// Ouvre un émulateur de terminal qui lance la console avec `token` (et les
/// options JSON de POST /helper/console). Renvoie le nom du terminal utilisé,
/// ou None si aucun n'a été trouvé. Ne renvoie JAMAIS le jeton.
pub fn spawn_terminal(token: &str, opts: &serde_json::Value) -> Option<String> {
    let tokfile = token_file(token).ok()?;
    let argv = console_argv(&self_exe(), &tokfile, opts);
    // shlex.quote sur CHAQUE argument : ces valeurs viennent d'une requête HTTP
    // et finissent dans un `sh -lc`.
    let cmd = argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ");
    let inner = format!("{cmd}; echo; read -p 'Entrée pour fermer…' _");

    let mut terms: Vec<(String, Vec<String>)> = Vec::new();
    if let Ok(t) = std::env::var("TERMINAL") {
        if !t.is_empty() {
            terms.push((t, vec!["-e".into()]));
        }
    }
    terms.extend(
        TERMINALS
            .iter()
            .map(|(n, pre, _dbus)| (n.to_string(), pre.iter().map(|s| s.to_string()).collect())),
    );

    for (term, pre) in terms {
        let Some(path) = find(&term) else { continue };
        let mut c = Command::new(&path);
        c.args(&pre).arg("sh").arg("-lc").arg(&inner).env("REDSTARS_TOKEN", token);
        clean_env(&mut c);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0); // détaché, comme start_new_session
        }
        if let Ok(mut child) = c.spawn() {
            // Ramasser le fils quand il se termine (pas de zombie).
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return path.file_name().map(|n| n.to_string_lossy().to_string());
        }
    }
    // Personne n'ouvrira ce fichier : on ne laisse pas traîner un jeton.
    let _ = std::fs::remove_file(&tokfile);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn noenv(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn args_defaults_and_env() {
        let a = parse_args(&[], &noenv).unwrap();
        assert_eq!(a.app_url, DEFAULT_APP_URL);
        assert_eq!(a.api_url, DEFAULT_API_URL);
        assert_eq!((a.role.as_str(), a.lang.as_str(), a.cols, a.rows), ("collaborator", "fr", 0, 0));
        assert!(!a.minitel && !a.accessible);
        let env = |k: &str| (k == "REDSTARS_API_URL").then(|| "http://x".to_string());
        assert_eq!(parse_args(&[], &env).unwrap().api_url, "http://x");
    }

    #[test]
    fn args_forms() {
        let a = parse_args(
            &sv(&["--app", "eau", "--cols=100", "--min", "--access", "--user", "bob", "--token-file", "/t"]),
            &noenv,
        )
        .unwrap();
        assert_eq!(a.app.as_deref(), Some("eau"));
        assert_eq!(a.cols, 100);
        assert!(a.minitel && a.accessible);
        assert_eq!(a.user.as_deref(), Some("bob"));
        assert_eq!(a.token_file.as_deref(), Some("/t"));
        // --app exact l'emporte sur les préfixes --app-url / --api-url.
        assert_eq!(parse_args(&sv(&["--app", "x"]), &noenv).unwrap().app.as_deref(), Some("x"));
        assert!(matches!(parse_args(&sv(&["--ap", "x"]), &noenv), Err(ArgsOutcome::Error(m)) if m.contains("ambiguous")));
        assert!(matches!(parse_args(&sv(&["--cols", "abc"]), &noenv), Err(ArgsOutcome::Error(_))));
        assert!(matches!(parse_args(&sv(&["--user"]), &noenv), Err(ArgsOutcome::Error(_))));
        assert!(matches!(parse_args(&sv(&["foo"]), &noenv), Err(ArgsOutcome::Error(m)) if m.contains("unrecognized")));
        assert_eq!(parse_args(&sv(&["-h"]), &noenv), Err(ArgsOutcome::Help));
        assert_eq!(parse_args(&sv(&["--he"]), &noenv), Err(ArgsOutcome::Help));
        assert_eq!(parse_args(&sv(&["--rows", "-3"]), &noenv).unwrap().rows, -3);
    }

    #[test]
    fn centering_math() {
        // Terminal 100×30, cadre 40×24 → gauche 30, haut 3.
        let s = center_frame("\x1b[2J\x1b[Hab\r\ncd", 40, 24, Some((100, 30)));
        assert_eq!(s, "\x1b[2J\x1b[4;31Hab\x1b[5;31Hcd");
        assert_eq!(offset(40, 24, true, Some((100, 30))), (3, 30));
        assert_eq!(offset(40, 24, false, Some((100, 30))), (0, 0));
        // Terminal plus petit que le cadre → collé en haut à gauche.
        assert_eq!(offset(40, 24, true, Some((30, 20))), (0, 0));
        assert_eq!(offset(40, 24, true, None), (0, 0));
        // Python : (rc - cols) // 2 avec rc-cols impair → plancher.
        assert_eq!(offset(40, 24, true, Some((81, 25))), (0, 20));
    }

    #[test]
    fn cursor_moves_by_focusable_number() {
        let foc: Vec<Value> = [2, 5, 7, 9].iter().map(|n| json!({ "n": n })).collect();
        assert_eq!(move_sel(&foc, &json!(5), "down", 24), Some(json!(7)));
        assert_eq!(move_sel(&foc, &json!(9), "down", 24), Some(json!(9)));
        assert_eq!(move_sel(&foc, &json!(2), "up", 24), Some(json!(2)));
        // sel introuvable → here = 0.
        assert_eq!(move_sel(&foc, &json!(1), "down", 24), Some(json!(5)));
        // Page : rows-5 crans, borné.
        assert_eq!(move_sel(&foc, &json!(2), "next", 7), Some(json!(7)));
        assert_eq!(move_sel(&foc, &json!(9), "prev", 24), Some(json!(2)));
        assert_eq!(move_sel(&[], &json!(1), "down", 24), None);
    }

    #[test]
    fn menu_ordering_and_labels() {
        let s = json!({"menu": [
            {"id": "reglages", "label": {"fr": "Réglages"}, "align": "bottom", "icon": "⚙"},
            {"id": "abonnes", "label": {"fr": "Abonnés", "en": "Subscribers"}},
        ]});
        let m = build_menu(&s);
        assert_eq!(m[0]["id"], "abonnes");
        assert_eq!(menu_label(&m[0], "en"), "Subscribers");
        assert_eq!(menu_label(&m[1], "en"), "⚙ Réglages");
        let s = json!({"slots": [{"id": "a", "kind": null}]});
        assert_eq!(menu_label(&build_menu(&s)[0], "fr"), "a");
    }

    #[test]
    fn choice_parsing() {
        assert_eq!(parse_choice(" 2 ", 3), Some(2));
        assert_eq!(parse_choice("0", 3), None);
        assert_eq!(parse_choice("4", 3), None);
        assert_eq!(parse_choice("x", 3), None);
        assert_eq!(parse_choice("", 3), None);
    }

    #[test]
    fn shlex_quote_like_python() {
        assert_eq!(shell_quote("eau"), "eau");
        assert_eq!(shell_quote("/usr/bin/x-y_z.1"), "/usr/bin/x-y_z.1");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("x; rm -rf ~"), "'x; rm -rf ~'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn spawn_argv_never_carries_token() {
        let opts = json!({"app": "eau", "role": "", "app_url": "https://a", "minitel": true, "accessible": false});
        let argv = console_argv(Path::new("/bin/rh"), Path::new("/run/t.tok"), &opts);
        assert_eq!(
            argv,
            sv(&["/bin/rh", "console", "--token-file", "/run/t.tok", "--app", "eau", "--app-url", "https://a", "--minitel"])
        );
    }

    #[test]
    fn token_file_is_private() {
        let _g = session::tests_env_lock();
        let dir = std::env::temp_dir().join(format!("rs-tok-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let p = token_file("secret").unwrap();
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert!(p.starts_with(&dir));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_terminal_token_by_file_not_argv() {
        use std::os::unix::fs::PermissionsExt;
        let _g = session::tests_env_lock();
        let dir = std::env::temp_dir().join(format!("rs-spawn-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("argv.log");
        let fake = dir.join("fake-term");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {l}.tmp\necho \"TOK=$REDSTARS_TOKEN\" >> {l}.tmp\nmv {l}.tmp {l}\n",
                l = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("TERMINAL", &fake);
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let r = spawn_terminal("SECRET-TOKEN", &json!({"app": "eau; rm -rf ~", "minitel": true}));
        std::env::remove_var("TERMINAL");
        std::env::remove_var("XDG_RUNTIME_DIR");
        assert_eq!(r.as_deref(), Some("fake-term"));
        let t0 = std::time::Instant::now();
        while !log.exists() && t0.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let txt = std::fs::read_to_string(&log).unwrap();
        let (argv, env) = txt.rsplit_once("TOK=").unwrap();
        assert!(!argv.contains("SECRET-TOKEN"), "le jeton ne doit jamais être dans argv");
        assert_eq!(env.trim(), "SECRET-TOKEN");
        let lines: Vec<&str> = argv.lines().collect();
        assert_eq!(&lines[..3], &["-e", "sh", "-lc"]);
        let inner = lines[3..].join("\n");
        assert!(inner.contains(" console --token-file "));
        assert!(inner.contains("--app 'eau; rm -rf ~' --minitel; echo; read -p"));
        // Le fichier jeton : 0600, sous XDG_RUNTIME_DIR, contient le jeton.
        let tok = inner.split_whitespace().skip_while(|w| *w != "--token-file").nth(1).unwrap();
        assert!(tok.starts_with(&dir.to_string_lossy().to_string()));
        assert_eq!(std::fs::read_to_string(tok).unwrap(), "SECRET-TOKEN");
        assert_eq!(std::fs::metadata(tok).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
