//! Petites imitations de Python, pour que les réponses soient octet pour octet
//! celles de helper.py : `json.dumps` (ordre des clés, séparateurs `", "`/`": "`,
//! `ensure_ascii`, `repr` des flottants), les messages `OSError`, `parse_qs`,
//! `os.path.normpath`, `str.splitlines`, `base64.b64decode` non strict…

use std::fmt::Write as _;
use std::io;

// ─── JSON ordonné, rendu comme json.dumps ────────────────────────────────────

/// Valeur JSON qui garde l'ordre d'insertion des clés (comme un dict Python).
#[derive(Clone, Debug)]
pub enum J {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

macro_rules! jobj {
    ($($k:expr => $v:expr),* $(,)?) => {
        $crate::helper::py::J::Obj(vec![$(($k.to_string(), $crate::helper::py::J::from($v))),*])
    };
}
pub(crate) use jobj;

impl From<bool> for J {
    fn from(v: bool) -> Self {
        J::Bool(v)
    }
}
impl From<&str> for J {
    fn from(v: &str) -> Self {
        J::Str(v.to_string())
    }
}
impl From<String> for J {
    fn from(v: String) -> Self {
        J::Str(v)
    }
}
impl From<&String> for J {
    fn from(v: &String) -> Self {
        J::Str(v.clone())
    }
}
macro_rules! from_int {
    ($($t:ty),*) => { $(impl From<$t> for J { fn from(v: $t) -> Self { J::Int(v as i128) } })* };
}
from_int!(i32, i64, u32, u64, usize);
impl From<f64> for J {
    fn from(v: f64) -> Self {
        J::Float(v)
    }
}
impl From<Vec<J>> for J {
    fn from(v: Vec<J>) -> Self {
        J::Arr(v)
    }
}
impl<T: Into<J>> From<Option<T>> for J {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(x) => x.into(),
            None => J::Null,
        }
    }
}

impl J {
    /// `json.dumps(self)` avec les réglages par défaut de Python.
    pub fn dumps(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(i) => {
                let _ = write!(out, "{i}");
            }
            J::Float(f) => out.push_str(&float_repr(*f)),
            J::Str(s) => write_str(out, s),
            J::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out);
                }
                out.push(']');
            }
            J::Obj(o) => {
                out.push('{');
                for (i, (k, v)) in o.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_str(out, k);
                    out.push_str(": ");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            _ => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{:04x}", u);
                }
            }
        }
    }
    out.push('"');
}

/// `repr(float)` de Python (et donc `json.dumps`).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let s = format!("{:e}", f); // chiffres les plus courts qui relisent la même valeur
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if !(-4..16).contains(&exp) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    } else if exp < 0 {
        out.push_str("0.");
        for _ in 0..(-exp - 1) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let e = exp as usize;
        if digits.len() > e + 1 {
            out.push_str(&digits[..e + 1]);
            out.push('.');
            out.push_str(&digits[e + 1..]);
        } else {
            out.push_str(&digits);
            for _ in 0..(e + 1 - digits.len()) {
                out.push('0');
            }
            out.push_str(".0");
        }
    }
    out
}

// ─── Corps JSON lu avec la souplesse de Python ───────────────────────────────

/// Nom du type Python d'une valeur JSON décodée (pour les messages d'erreur).
pub fn py_type(v: &serde_json::Value) -> &'static str {
    use serde_json::Value as V;
    match v {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Number(n) if n.is_f64() => "float",
        V::Number(_) => "int",
        V::String(_) => "str",
        V::Array(_) => "list",
        V::Object(_) => "dict",
    }
}

/// Vérité au sens Python (`if x:` / `x or y`).
pub fn truthy(v: Option<&serde_json::Value>) -> bool {
    use serde_json::Value as V;
    match v {
        None | Some(V::Null) => false,
        Some(V::Bool(b)) => *b,
        Some(V::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(V::String(s)) => !s.is_empty(),
        Some(V::Array(a)) => !a.is_empty(),
        Some(V::Object(o)) => !o.is_empty(),
    }
}

/// `(b.get(key) or default)` attendu comme chaîne. Erreur = message Python
/// `"TypeName: msg"` de l'exception qu'aurait levée la méthode `method`
/// appelée dessus (`strip`, …).
pub fn str_or(
    b: &serde_json::Value,
    key: &str,
    default: &str,
    method: &str,
) -> Result<String, String> {
    let v = b.get(key);
    if !truthy(v) {
        return Ok(default.to_string());
    }
    match v {
        Some(serde_json::Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!(
            "AttributeError: '{}' object has no attribute '{}'",
            py_type(other),
            method
        )),
        None => Ok(default.to_string()),
    }
}

// ─── Exceptions OSError ──────────────────────────────────────────────────────

/// Nom de la sous-classe d'OSError que Python lèverait.
pub fn oserror_name(e: &io::Error) -> &'static str {
    use io::ErrorKind as K;
    match e.kind() {
        K::NotFound => "FileNotFoundError",
        K::AlreadyExists => "FileExistsError",
        K::PermissionDenied => "PermissionError",
        K::NotADirectory => "NotADirectoryError",
        K::IsADirectory => "IsADirectoryError",
        K::TimedOut => "TimeoutError",
        K::Interrupted => "InterruptedError",
        K::BrokenPipe => "BrokenPipeError",
        K::ConnectionRefused => "ConnectionRefusedError",
        K::ConnectionReset => "ConnectionResetError",
        _ => "OSError",
    }
}

/// `str(e)` d'une OSError Python : `[Errno 2] No such file or directory: 'x'`.
pub fn oserror_str(e: &io::Error, files: &[&str]) -> String {
    let Some(code) = e.raw_os_error() else {
        return e.to_string();
    };
    let full = e.to_string();
    let msg = match full.rfind(" (os error ") {
        Some(i) => full[..i].to_string(),
        None => full,
    };
    let tag = if cfg!(windows) { "WinError" } else { "Errno" };
    let mut s = format!("[{tag} {code}] {msg}");
    match files {
        [] => {}
        [a] => {
            let _ = write!(s, ": {}", repr_str(a));
        }
        [a, b, ..] => {
            let _ = write!(s, ": {} -> {}", repr_str(a), repr_str(b));
        }
    }
    s
}

/// `f'{type(e).__name__}: {e}'` pour une OSError.
pub fn oserror_full(e: &io::Error, files: &[&str]) -> String {
    format!("{}: {}", oserror_name(e), oserror_str(e, files))
}

/// `repr(str)` de Python (cas courants).
pub fn repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// `repr(list_of_str)`.
pub fn repr_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| repr_str(s)).collect();
    format!("[{}]", parts.join(", "))
}

// ─── Chaînes ─────────────────────────────────────────────────────────────────

/// `str.isspace()` pour un caractère.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `str.splitlines()`.
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `Path.read_text()` : UTF-8 strict, sauts de ligne universels.
pub fn read_text(p: &std::path::Path) -> Result<String, String> {
    let bytes = std::fs::read(p).map_err(|e| oserror_full(&e, &[&p.to_string_lossy()]))?;
    let s = String::from_utf8(bytes)
        .map_err(|e| format!("UnicodeDecodeError: 'utf-8' codec can't decode bytes: {e}"))?;
    Ok(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `Path.write_text()` : `\n` devient `os.linesep`.
pub fn write_text(p: &std::path::Path, s: &str) -> Result<(), String> {
    let data = if cfg!(windows) { s.replace('\n', "\r\n") } else { s.to_string() };
    std::fs::write(p, data).map_err(|e| oserror_full(&e, &[&p.to_string_lossy()]))
}

// ─── URL ─────────────────────────────────────────────────────────────────────

fn unquote_plus(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let h = if b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
                    std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
                } else {
                    None
                };
                match h {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `parse_qs(qs)[key][0]` ou `""` (valeurs vides ignorées, comme Python).
pub fn query_first(qs: &str, key: &str) -> Option<String> {
    for part in qs.split('&') {
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else { continue };
        if v.is_empty() {
            continue;
        }
        if unquote_plus(k) == key {
            return Some(unquote_plus(v));
        }
    }
    None
}

// ─── Chemins ─────────────────────────────────────────────────────────────────

/// `os.path.normpath(os.path.abspath(p))`.
pub fn abspath(p: &str) -> String {
    #[cfg(windows)]
    {
        // ntpath.abspath = GetFullPathNameW, ce que fait std::path::absolute.
        std::path::absolute(p)
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_else(|_| p.to_string())
    }
    #[cfg(not(windows))]
    {
        if p.starts_with('/') {
            normpath(p)
        } else {
            let cwd = std::env::current_dir()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".into());
            normpath(&format!("{cwd}/{p}"))
        }
    }
}

/// `posixpath.normpath`.
#[cfg_attr(windows, allow(dead_code))]
pub fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let initial = if p.starts_with("//") && !p.starts_with("///") {
        2
    } else if p.starts_with('/') {
        1
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for c in p.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(c);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let s = format!("{}{}", "/".repeat(initial), comps.join("/"));
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

/// `str(Path(p))` : barres doublées, `.` et barre finale retirés.
pub fn path_str(p: &str) -> String {
    if cfg!(windows) {
        let pb = std::path::PathBuf::from(p);
        let s: std::path::PathBuf = pb.components().collect();
        let s = s.to_string_lossy().into_owned();
        return if s.is_empty() { ".".into() } else { s };
    }
    let initial = if p.starts_with("//") && !p.starts_with("///") {
        "//"
    } else if p.starts_with('/') {
        "/"
    } else {
        ""
    };
    let comps: Vec<&str> = p.split('/').filter(|c| !c.is_empty() && *c != ".").collect();
    let s = format!("{initial}{}", comps.join("/"));
    if s.is_empty() {
        ".".into()
    } else {
        s
    }
}

/// `Path(p).name`.
pub fn path_name(p: &str) -> String {
    let s = path_str(p);
    let cut = if cfg!(windows) {
        s.rfind(['/', '\\'])
    } else {
        s.rfind('/')
    };
    let name = match cut {
        Some(i) => &s[i + 1..],
        None => &s[..],
    };
    if name == "." || (cfg!(windows) && name.ends_with(':')) {
        String::new()
    } else {
        name.to_string()
    }
}

/// `(Path.stem, Path.suffix)` selon Python 3.14 (un point final est un suffixe).
pub fn stem_suffix(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 && name != ".." => (name[..i].to_string(), name[i..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

/// `str(Path(p).resolve())` (non strict).
pub fn resolve(p: &str) -> String {
    match std::fs::canonicalize(p) {
        Ok(c) => strip_verbatim(c.to_string_lossy().into_owned()),
        Err(_) => abspath(p),
    }
}

/// Windows : `\\?\C:\x` → `C:\x` (ce que rend Python).
pub fn strip_verbatim(s: String) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            return rest.to_string();
        }
    }
    s
}

// ─── base64.b64decode (non strict) ───────────────────────────────────────────

/// `base64.b64decode(v)` ; l'erreur est `str(e)` (sans le nom du type).
pub fn b64decode(v: &serde_json::Value) -> Result<Vec<u8>, String> {
    let s = match v {
        serde_json::Value::String(s) => s,
        other => {
            return Err(format!(
                "argument should be a bytes-like object or ASCII string, not '{}'",
                py_type(other)
            ))
        }
    };
    if !s.is_ascii() {
        return Err("string argument should contain only ASCII characters".into());
    }
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut quad, mut left, mut pads) = (0u8, 0u8, 0u8);
    for &c in s.as_bytes() {
        if c == b'=' {
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(ch) = val(c) else { continue };
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = ch;
            }
            1 => {
                quad = 2;
                out.push((left << 2) | (ch >> 4));
                left = ch & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (ch >> 2));
                left = ch & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | ch);
                left = 0;
            }
        }
    }
    match quad {
        0 => Ok(out),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more than a multiple of 4",
            out.len() / 3 * 4 + 1
        )),
        _ => Err("Incorrect padding".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats() {
        assert_eq!(float_repr(27.34), "27.34");
        assert_eq!(float_repr(27.0), "27.0");
        assert_eq!(float_repr(-0.0), "-0.0");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(1.5e-5), "1.5e-05");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(1760870000.123456), "1760870000.123456");
        assert_eq!(float_repr(123456789012345.6), "123456789012345.6");
    }

    #[test]
    fn json() {
        let v = jobj! {"ok" => true, "é" => "a\"b\u{1F600}", "n" => J::Null, "l" => vec![J::from(1i64), J::from(2.5)]};
        assert_eq!(
            v.dumps(),
            r#"{"ok": true, "\u00e9": "a\"b\ud83d\ude00", "n": null, "l": [1, 2.5]}"#
        );
    }

    #[test]
    fn b64() {
        let s = |x: &str| b64decode(&serde_json::Value::String(x.into()));
        assert_eq!(s("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(s("aGVs\nbG8=").unwrap(), b"hello");
        assert_eq!(s("aGVsbG8").unwrap_err(), "Incorrect padding");
        assert!(s("a").unwrap_err().contains("(1)"));
    }

    #[test]
    fn paths() {
        assert_eq!(normpath("/a/./b/../c//"), "/a/c");
        assert_eq!(normpath("//a"), "//a");
        assert_eq!(path_str("/a//b/./c/"), "/a/b/c");
        assert_eq!(path_name("/a/b/"), "b");
        assert_eq!(stem_suffix("a."), ("a".into(), ".".into()));
        assert_eq!(stem_suffix(".a"), (".a".into(), "".into()));
        assert_eq!(query_first("id=&id=x%41+y", "id").as_deref(), Some("xA y"));
        assert_eq!(splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
    }
}
