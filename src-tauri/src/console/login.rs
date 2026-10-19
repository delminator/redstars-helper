//! L'écran de marque RedStars (`_con_login_screen`) — la réplique Vidéotex du
//! login web, montrée à chaque lancement, y compris quand la session est déjà
//! ouverte (bouton « Continuer »). F1 y bascule le mode accessible.

use super::term::{self, LoginKey};
use regex::Regex;
use std::sync::OnceLock;

pub enum LoginOut {
    /// (utilisateur, mot de passe) saisis.
    Creds(String, String),
    Continue,
    Quit,
    /// Pas d'écran (pas un tty, mode accessible, Échap) : l'appelant retombe sur
    /// la saisie ligne à ligne.
    Fallback,
}

struct L10n {
    sub: &'static str,
    user: &'static str,
    pw: &'static str,
    ph: &'static str,
    envoi: &'static str,
    submit: &'static str,
    noacc: &'static str,
    hint: &'static str,
    f1: &'static str,
    cont: &'static str,
    contsub: &'static str,
    hint2: &'static str,
}

const FR: L10n = L10n {
    sub: "Plateforme de gestion multi-services",
    user: "Nom d'utilisateur",
    pw: "Mot de passe",
    ph: "votre_pseudo",
    envoi: "ENVOI",
    submit: "se connecter",
    noacc: "Pas de compte ? Inscription sur le web.",
    hint: "↵ valider  ·  ↑↓ champ  ·  Echap annuler",
    f1: "F1 · accessibilité (voix, liste numérotée)",
    cont: "CONTINUER",
    contsub: "session déjà ouverte",
    hint2: "↵ continuer  ·  Echap quitter",
};

const EN: L10n = L10n {
    sub: "Multi-service management platform",
    user: "Username",
    pw: "Password",
    ph: "your_username",
    envoi: "ENTER",
    submit: "sign in",
    noacc: "No account? Register on the web.",
    hint: "↵ submit  ·  ↑↓ field  ·  Esc cancel",
    f1: "F1 · accessibility (speech, numbered list)",
    cont: "CONTINUE",
    contsub: "session already open",
    hint2: "↵ continue  ·  Esc quit",
};

fn ansi_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap())
}

/// Largeur visible (en points de code, comme `len()` Python) sans les SGR.
fn visible_len(s: &str) -> usize {
    ansi_re().replace_all(s, "").chars().count()
}

/// `str.ljust(n)` en points de code.
pub fn ljust(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len >= n {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(n - len))
    }
}

/// `s[:n]` en points de code.
pub fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const C_BRAND: &str = "\x1b[36;1m";
const C_STAR: &str = "\x1b[33m";
const C_DIM: &str = "\x1b[2m";
const C_FRAME: &str = "\x1b[34m";
const C_ACT: &str = "\x1b[36m";
const C_OK: &str = "\x1b[32;1m";
const R: &str = "\x1b[0m";
const IW: usize = 32; // largeur intérieure du cadre
const FW: usize = IW - 4; // largeur visible d'un champ

/// Compose l'écran (pur, testable) : une ligne par élément.
pub fn screen_lines(cw: usize, lang: &str, logged_in: bool, fields: &[String; 2], cur: usize) -> Vec<String> {
    let tr = if lang == "en" { &EN } else { &FR };
    let center = |s: String| -> String {
        let pad = cw.saturating_sub(visible_len(&s)) / 2;
        format!("{}{}", " ".repeat(pad), s)
    };
    let frame_row = |plain: &str, color: &str| -> String {
        let body = ljust(&format!(" {plain}"), IW);
        center(format!("{C_FRAME}│{R}{color}{body}{R}{C_FRAME}│{R}"))
    };
    let frame_field = |idx: usize, masked: bool, placeholder: &str| -> String {
        let raw = &fields[idx];
        if raw.is_empty() && idx != cur && !placeholder.is_empty() {
            return frame_row(&format!("[{}]", take(&ljust(placeholder, FW), FW)), C_DIM);
        }
        let mut shown: String = if masked { "•".repeat(raw.chars().count()) } else { raw.clone() };
        if idx == cur {
            shown.push('█');
        }
        let n = shown.chars().count();
        if n > FW {
            shown = shown.chars().skip(n - FW).collect();
        }
        frame_row(&format!("[{}]", ljust(&shown, FW)), if idx == cur { C_ACT } else { C_DIM })
    };

    let mut lines = vec![
        String::new(),
        center(format!("{C_STAR}★{R}  {C_BRAND}R E D S T A R S{R}  {C_STAR}★{R}")),
        center(format!("{C_DIM}{}{R}", tr.sub)),
        String::new(),
    ];
    if logged_in {
        lines.extend([
            String::new(),
            center(format!("{C_OK}\x1b[7m  ▶  {}  {R}", tr.cont)),
            String::new(),
            center(format!("{C_DIM}{}{R}", tr.contsub)),
            String::new(),
            center(format!("{C_OK}{}{R} {C_DIM}{}{R}", tr.envoi, tr.cont.to_lowercase())),
        ]);
    } else {
        lines.extend([
            center(format!("{C_FRAME}┌{}┐{R}", "─".repeat(IW))),
            frame_row(tr.user, if cur == 0 { C_ACT } else { C_DIM }),
            frame_field(0, false, tr.ph),
            frame_row("", ""),
            frame_row(tr.pw, if cur == 1 { C_ACT } else { C_DIM }),
            frame_field(1, true, ""),
            center(format!("{C_FRAME}└{}┘{R}", "─".repeat(IW))),
            String::new(),
            center(format!("{C_OK}{}{R} {C_DIM}{}{R}", tr.envoi, tr.submit)),
            center(format!("{C_DIM}{}{R}", tr.noacc)),
        ]);
    }
    lines.extend([
        String::new(),
        center(format!("{C_STAR}{}{R}", tr.f1)),
        center(format!("{C_DIM}{}{R}", if logged_in { tr.hint2 } else { tr.hint })),
    ]);
    lines
}

/// Restaure curseur + écran à la sortie, quel que soit le chemin.
struct ScreenGuard;
impl Drop for ScreenGuard {
    fn drop(&mut self) {
        term::out("\x1b[?25h\x1b[2J\x1b[H");
        term::flush();
    }
}

/// `_con_login_screen(a, logged_in)`. `accessible` peut passer à true (F1).
pub fn screen(accessible: &mut bool, lang: &str, minitel: bool, logged_in: bool) -> LoginOut {
    let nontty = if logged_in { LoginOut::Continue } else { LoginOut::Fallback };
    if *accessible || !term::stdin_is_tty() {
        return nontty;
    }
    let tcols = term::size().map(|(c, _)| c).unwrap_or(40);
    let cw = if minitel { 40 } else { tcols.clamp(40, 80) as usize };

    let mut fields = [String::new(), String::new()];
    let mut cur = 0usize;

    // Ordre de destruction : le mode brut tombe AVANT le nettoyage d'écran — comme
    // le `finally` Python (tcsetattr puis write).
    let _screen = ScreenGuard;
    let _raw = term::Raw::enter();
    loop {
        term::out(&format!(
            "\x1b[?25l\x1b[2J\x1b[H{}\r\n",
            screen_lines(cw, lang, logged_in, &fields, cur).join("\r\n")
        ));
        term::flush();
        let key = match term::next_key() {
            Some(k) => term::login_key(&k),
            None => LoginKey::Key("esc"),
        };
        if key == LoginKey::Key("a11y") {
            *accessible = true;
            return nontty;
        }
        if logged_in {
            match key {
                LoginKey::Key("enter") | LoginKey::Char(_) => return LoginOut::Continue,
                LoginKey::Key("esc") | LoginKey::Key("quit") => return LoginOut::Quit,
                _ => continue,
            }
        }
        match key {
            LoginKey::Char(c) => fields[cur].push(c),
            LoginKey::Key("back") => {
                fields[cur].pop();
            }
            LoginKey::Key("tab") | LoginKey::Key("down") => cur = 1 - cur,
            LoginKey::Key("up") => cur = 0,
            LoginKey::Key("enter") => {
                if cur == 0 {
                    cur = 1;
                } else if !fields[0].is_empty() && !fields[1].is_empty() {
                    return LoginOut::Creds(fields[0].clone(), fields[1].clone());
                } else {
                    cur = if fields[0].is_empty() { 0 } else { 1 };
                }
            }
            LoginKey::Key("esc") | LoginKey::Key("quit") => return LoginOut::Fallback,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centering_and_masking() {
        let f = [String::from("alice"), String::from("pw")];
        let lines = screen_lines(40, "fr", false, &f, 1);
        // Le cadre fait IW+2 = 34 de large → (40-34)/2 = 3 espaces.
        assert!(lines[4].starts_with("   \x1b[34m┌"));
        // Le mot de passe est masqué, curseur bloc sur le champ actif.
        assert!(lines[9].contains("[••█"));
        assert!(lines[6].contains("[alice"));
        let lines = screen_lines(80, "en", true, &f, 0);
        assert!(lines.iter().any(|l| l.contains("CONTINUE")));
    }

    #[test]
    fn long_field_shows_tail() {
        let f = [String::from("x".repeat(40)), String::new()];
        let lines = screen_lines(40, "fr", false, &f, 0);
        let vis = ansi_re().replace_all(&lines[6], "").to_string();
        assert!(vis.contains(&format!("[{}█]", "x".repeat(FW - 1))));
    }

    #[test]
    fn placeholder_when_inactive() {
        let f = [String::new(), String::new()];
        let lines = screen_lines(40, "fr", false, &f, 1);
        assert!(lines[6].contains("[votre_pseudo"));
    }
}
