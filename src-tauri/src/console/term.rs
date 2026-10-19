//! Terminal : mode brut, touches, taille, sortie — via crossterm, pour que le
//! client marche aussi hors unix (Windows).
//!
//! Le Python entrait/sortait du mode brut fonction par fonction (`tty.setraw` +
//! `tcsetattr` dans un finally), y compris en imbriqué (la modale dans la liste).
//! `Raw` reproduit ça avec un compteur : le terminal ne redevient « cuit » qu'à la
//! sortie du garde le plus externe. Le crochet de panique (voir mod.rs) appelle
//! `force_restore` pour qu'aucune sortie — panique comprise — ne laisse le
//! terminal en vrac.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

static RAW_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Garde de mode brut (imbricable).
pub struct Raw(());

impl Raw {
    pub fn enter() -> Raw {
        if RAW_DEPTH.fetch_add(1, Ordering::SeqCst) == 0 {
            let _ = crossterm::terminal::enable_raw_mode();
        }
        Raw(())
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if RAW_DEPTH.fetch_sub(1, Ordering::SeqCst) == 1 {
            flush();
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

/// Remet le terminal d'aplomb quoi qu'il arrive (panique) : mode cuit, curseur
/// visible.
pub fn force_restore() {
    RAW_DEPTH.store(0, Ordering::SeqCst);
    let _ = crossterm::terminal::disable_raw_mode();
    let mut o = io::stdout();
    let _ = o.write_all(b"\x1b[0m\x1b[?25h\r\n");
    let _ = o.flush();
}

/// `os.get_terminal_size()` : la taille de la sortie standard, None si ce n'est
/// pas un terminal (pipe, cron) — l'appelant choisit son repli.
pub fn size() -> Option<(i64, i64)> {
    if !io::stdout().is_terminal() {
        return None;
    }
    match crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => Some((c as i64, r as i64)),
        _ => None,
    }
}

pub fn stdin_is_tty() -> bool {
    io::stdin().is_terminal()
}
pub fn stdout_is_tty() -> bool {
    io::stdout().is_terminal()
}

pub fn out(s: &str) {
    let _ = io::stdout().write_all(s.as_bytes());
}
pub fn flush() {
    let _ = io::stdout().flush();
}

/// `input(prompt)` : None sur EOF / erreur (EOFError en Python).
pub fn input(prompt: &str) -> Option<String> {
    out(prompt);
    flush();
    let mut line = String::new();
    match io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
            }
            Some(line)
        }
    }
}

/// La prochaine frappe (les autres événements — redimensionnement, souris,
/// relâchement de touche sous Windows — sont ignorés). None = entrée fermée.
pub fn next_key() -> Option<KeyEvent> {
    loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => return Some(k),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

/// `_con_read_key` : une touche → l'intention que le serveur attend.
///
/// Mêmes noms que Python (`up`, `down`, `prev`, `next`, `enter`, `quit`, `new`,
/// `back`, `a11y`). Toute séquence d'échappement non reconnue — Échap seul,
/// ←, →, Début, Fin, Suppr, F2… — vaut « back », comme en Python. Les autres
/// caractères → None (la boucle redemande la trame, comme en Python).
///
/// Écart voulu : Ctrl-C vaut « quit » (en Python, le mode brut l'avalait et il
/// ne faisait rien).
pub fn nav_key(k: &KeyEvent) -> Option<&'static str> {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::Up => Some("up"),
        KeyCode::Down => Some("down"),
        KeyCode::PageUp => Some("prev"),
        KeyCode::PageDown => Some("next"),
        KeyCode::F(1) => Some("a11y"),
        KeyCode::Enter => Some("enter"),
        KeyCode::Char(c) if ctrl => match c {
            'j' | 'm' => Some("enter"), // \n en mode brut = Ctrl-J
            'c' => Some("quit"),
            _ => None,
        },
        KeyCode::Char(_) if alt => Some("back"), // ESC + octet : séquence inconnue
        KeyCode::Char('q') => Some("quit"),
        KeyCode::Char('n') => Some("new"),
        KeyCode::Char(_) | KeyCode::Tab | KeyCode::Backspace | KeyCode::Null => None,
        _ => Some("back"),
    }
}

/// Une frappe du formulaire de login (`_con_login_read`).
#[derive(Debug, PartialEq, Eq)]
pub enum LoginKey {
    Char(char),
    Key(&'static str),
}

pub fn login_key(k: &KeyEvent) -> LoginKey {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    match k.code {
        KeyCode::F(1) => LoginKey::Key("a11y"),
        KeyCode::Up => LoginKey::Key("up"),
        KeyCode::Down => LoginKey::Key("down"),
        KeyCode::Enter => LoginKey::Key("enter"),
        KeyCode::Tab => LoginKey::Key("tab"),
        KeyCode::Backspace => LoginKey::Key("back"),
        KeyCode::Char(c) if ctrl => LoginKey::Key(match c {
            'c' | 'd' => "quit",
            'j' | 'm' => "enter",
            'h' => "back",
            'i' => "tab",
            _ => "skip",
        }),
        KeyCode::Char(_) if alt => LoginKey::Key("esc"),
        KeyCode::Char(c) => LoginKey::Char(c),
        KeyCode::Null => LoginKey::Key("skip"),
        _ => LoginKey::Key("esc"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn kc(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn nav_keys_like_python() {
        assert_eq!(nav_key(&k(KeyCode::Up)), Some("up"));
        assert_eq!(nav_key(&k(KeyCode::Down)), Some("down"));
        assert_eq!(nav_key(&k(KeyCode::PageUp)), Some("prev"));
        assert_eq!(nav_key(&k(KeyCode::PageDown)), Some("next"));
        assert_eq!(nav_key(&k(KeyCode::F(1))), Some("a11y"));
        assert_eq!(nav_key(&k(KeyCode::Enter)), Some("enter"));
        assert_eq!(nav_key(&kc('j')), Some("enter"));
        assert_eq!(nav_key(&k(KeyCode::Char('q'))), Some("quit"));
        assert_eq!(nav_key(&k(KeyCode::Char('n'))), Some("new"));
        assert_eq!(nav_key(&k(KeyCode::Char('Q'))), None);
        assert_eq!(nav_key(&k(KeyCode::Char('x'))), None);
        assert_eq!(nav_key(&k(KeyCode::Esc)), Some("back"));
        assert_eq!(nav_key(&k(KeyCode::Left)), Some("back"));
        assert_eq!(nav_key(&k(KeyCode::Right)), Some("back"));
        assert_eq!(nav_key(&k(KeyCode::F(5))), Some("back"));
        assert_eq!(nav_key(&kc('c')), Some("quit"));
        assert_eq!(nav_key(&k(KeyCode::Tab)), None);
    }

    #[test]
    fn login_keys_like_python() {
        assert_eq!(login_key(&k(KeyCode::Char('é'))), LoginKey::Char('é'));
        assert_eq!(
            login_key(&KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            LoginKey::Char('A')
        );
        assert_eq!(login_key(&k(KeyCode::Backspace)), LoginKey::Key("back"));
        assert_eq!(login_key(&kc('h')), LoginKey::Key("back"));
        assert_eq!(login_key(&kc('d')), LoginKey::Key("quit"));
        assert_eq!(login_key(&kc('a')), LoginKey::Key("skip"));
        assert_eq!(login_key(&k(KeyCode::Tab)), LoginKey::Key("tab"));
        assert_eq!(login_key(&k(KeyCode::Esc)), LoginKey::Key("esc"));
        assert_eq!(login_key(&k(KeyCode::Left)), LoginKey::Key("esc"));
        assert_eq!(login_key(&k(KeyCode::F(1))), LoginKey::Key("a11y"));
    }
}
