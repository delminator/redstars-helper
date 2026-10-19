//! Balance série à la demande (portage de `ScaleReader`).
//!
//! AUCUNE lecture en continu : le port série est ouvert à la première requête
//! GET /helper/scale et gardé ouvert tant que la page continue de demander. Un
//! seul minuteur (un réveil toutes les IDLE_CLOSE secondes, SEULEMENT pendant
//! l'usage) referme le port quelques secondes après la dernière lecture — le
//! helper redevient alors complètement dormant.
//!
//! Les balances CH340 bon marché émettent des lignes de poids en continu : chaque
//! lecture vide le tampon et rend la ligne la plus fraîche.

use std::io::{self, Read};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regex::Regex;

use super::py::{self, J};

const SCALE_PORT: &str = "/dev/ttyUSB0";
const SCALE_BAUD: u32 = 9600;
const IDLE_CLOSE: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_millis(400);
/// Pont Android : plus vieux que ça ⇒ balance débranchée.
const STALE_MS: f64 = 4000.0;

// Format observé sur les balances CH340 : "WTST    +27.34  g"
static SCALE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?P<status>[A-Z]{2,4})\s*(?P<sign>[+-])(?P<value>\d+(?:\.\d+)?)\s*(?P<unit>g|kg|lb|oz|ml)?")
        .expect("SCALE_LINE")
});

#[derive(Clone, Default)]
struct State {
    connected: bool,
    value: Option<f64>,
    unit: Option<String>,
    sign: Option<String>,
    status: Option<String>,
    raw: Option<String>,
    updated_at: Option<f64>,
    error: Option<String>,
}

impl State {
    /// Réponse de GET /helper/scale (avec `age_ms` si `updated_at`).
    fn to_json(&self) -> J {
        let mut o = vec![
            ("connected".to_string(), J::from(self.connected)),
            ("value".to_string(), J::from(self.value)),
            ("unit".to_string(), J::from(self.unit.clone())),
            ("sign".to_string(), J::from(self.sign.clone())),
            ("status".to_string(), J::from(self.status.clone())),
            ("raw".to_string(), J::from(self.raw.clone())),
            ("updated_at".to_string(), J::from(self.updated_at)),
            ("error".to_string(), J::from(self.error.clone())),
        ];
        if let Some(t) = self.updated_at.filter(|t| *t != 0.0) {
            o.push(("age_ms".to_string(), J::Int(((now() - t) * 1000.0) as i128)));
        }
        J::Obj(o)
    }

    fn from_match(m: &regex::Captures, raw: &str) -> State {
        let sign = m.name("sign").map(|x| x.as_str()).unwrap_or("+");
        let v: f64 = m.name("value").and_then(|x| x.as_str().parse().ok()).unwrap_or(0.0);
        let s = if sign == "-" { -1.0 } else { 1.0 };
        State {
            connected: true,
            value: Some(s * v),
            unit: Some(m.name("unit").map(|x| x.as_str()).unwrap_or("g").to_lowercase()),
            sign: Some(sign.to_string()),
            status: m.name("status").map(|x| x.as_str().to_string()),
            raw: Some(raw.to_string()),
            updated_at: Some(now()),
            error: None,
        }
    }
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

struct Inner {
    port: Option<Box<dyn serialport::SerialPort>>,
    buf: Vec<u8>,
    last_request: Instant,
    timer: bool,
    last: State,
}

static SCALE: LazyLock<Mutex<Inner>> = LazyLock::new(|| {
    Mutex::new(Inner { port: None, buf: Vec::new(), last_request: Instant::now(), timer: false, last: State::default() })
});

fn lock() -> std::sync::MutexGuard<'static, Inner> {
    SCALE.lock().unwrap_or_else(|p| p.into_inner())
}

impl Inner {
    fn close(&mut self) {
        self.port = None;
        self.buf.clear();
    }

    fn pump(&mut self) -> io::Result<()> {
        let Some(port) = self.port.as_mut() else { return Ok(()) };
        let mut tmp = [0u8; 4096];
        match port.read(&mut tmp) {
            Ok(n) => {
                self.buf.extend_from_slice(&tmp[..n]);
                Ok(())
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn has_eol(&self) -> bool {
        self.buf.iter().any(|c| *c == b'\n' || *c == b'\r')
    }

    /// Une ligne (coupée sur \n ou \r), ou ce qui est arrivé avant le délai.
    fn readline(&mut self) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + READ_TIMEOUT;
        while !self.has_eol() && Instant::now() < deadline {
            self.pump()?;
            if !self.has_eol() {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        match self.buf.iter().position(|c| *c == b'\n' || *c == b'\r') {
            Some(i) => {
                let line = self.buf[..i].to_vec();
                self.buf.drain(..=i);
                Ok(line)
            }
            None => Ok(std::mem::take(&mut self.buf)),
        }
    }

    fn in_waiting(&mut self) -> io::Result<bool> {
        if self.has_eol() {
            return Ok(true);
        }
        match self.port.as_ref() {
            Some(p) => p.bytes_to_read().map(|n| n > 0).map_err(io::Error::from),
            None => Ok(false),
        }
    }
}

fn decode(line: &[u8]) -> String {
    // .decode('ascii', errors='replace').strip()
    let s: String = line.iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect();
    py::strip(&s).to_string()
}

fn arm_timer() {
    std::thread::spawn(|| loop {
        std::thread::sleep(IDLE_CLOSE);
        let mut g = lock();
        if g.last_request.elapsed() >= IDLE_CLOSE {
            g.close();
            g.timer = false;
            return;
        }
    });
}

fn open_port() -> Result<Box<dyn serialport::SerialPort>, String> {
    serialport::new(SCALE_PORT, SCALE_BAUD)
        .timeout(Duration::from_millis(0))
        .open()
        .map_err(|e| {
            use serialport::ErrorKind as K;
            match e.kind() {
                K::NoDevice | K::Io(io::ErrorKind::NotFound) if !std::path::Path::new(SCALE_PORT).exists() => {
                    format!("{SCALE_PORT} not present (scale unplugged?)")
                }
                K::Io(io::ErrorKind::PermissionDenied) => format!("{SCALE_PORT} permission denied (udev rule?)"),
                _ => format!("SerialException: {}", e.description),
            }
        })
}

/// Pont Android : `REDSTARS_SCALE_FILE` contient `<epoch_ms>\t<ligne brute>`.
fn read_from_file(g: &mut Inner, path: &str) -> State {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => py::strip(&c).to_string(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            g.last.connected = false;
            g.last.error = Some("balance bridge: no reading yet".into());
            return g.last.clone();
        }
        Err(e) => {
            g.last.connected = false;
            g.last.error = Some(py::oserror_full(&e, &[path]));
            return g.last.clone();
        }
    };
    let Some((ts, raw)) = content.split_once('\t') else {
        g.last.connected = false;
        g.last.error = Some("balance bridge: no reading yet".into());
        return g.last.clone();
    };
    let raw = py::strip(raw).to_string();
    let age = py::strip(ts).parse::<f64>().ok().map(|t| now() * 1000.0 - t);
    if age.is_some_and(|a| a > STALE_MS) {
        g.last.connected = false;
        g.last.raw = Some(raw);
        g.last.error = Some("scale idle (unplugged?)".into());
        return g.last.clone();
    }
    if let Some(m) = SCALE_LINE.captures(&raw) {
        g.last = State::from_match(&m, &raw);
    } else {
        g.last.connected = true;
        g.last.raw = Some(raw);
        g.last.updated_at = Some(now());
        g.last.error = None;
    }
    g.last.clone()
}

/// GET /helper/scale.
pub fn read_json() -> J {
    read().to_json()
}

fn read() -> State {
    let mut g = lock();
    if let Ok(bridge) = std::env::var("REDSTARS_SCALE_FILE") {
        if !bridge.is_empty() {
            return read_from_file(&mut g, &bridge);
        }
    }
    g.last_request = Instant::now();
    if g.port.is_none() {
        match open_port() {
            Ok(p) => {
                g.port = Some(p);
                g.buf.clear();
            }
            Err(msg) => {
                g.last.connected = false;
                g.last.error = Some(msg);
                return g.last.clone();
            }
        }
    }
    let res: io::Result<(Option<String>, Option<State>)> = (|| {
        let mut latest_raw = None;
        let mut latest = None;
        // Tout vider pour rendre la ligne LA PLUS FRAÎCHE.
        while g.in_waiting()? {
            let raw = decode(&g.readline()?);
            if raw.is_empty() {
                continue;
            }
            if let Some(m) = SCALE_LINE.captures(&raw) {
                latest = Some(State::from_match(&m, &raw));
            }
            latest_raw = Some(raw);
        }
        if latest_raw.is_none() {
            // Rien en tampon — une courte lecture bloquante.
            let raw = decode(&g.readline()?);
            if !raw.is_empty() {
                latest = SCALE_LINE.captures(&raw).map(|m| State::from_match(&m, &raw));
                latest_raw = Some(raw);
            }
        }
        Ok((latest_raw, latest))
    })();
    match res {
        Err(e) => {
            g.close();
            g.last.connected = false;
            g.last.error = Some(format!("SerialException: {e}"));
            return g.last.clone();
        }
        Ok((_, Some(st))) => g.last = st,
        Ok((Some(raw), None)) => {
            // Message de démarrage / ligne vide — garder raw pour debug.
            g.last.connected = true;
            g.last.raw = Some(raw);
            g.last.updated_at = Some(now());
            g.last.error = None;
        }
        Ok((None, None)) => {
            // Port ouvert mais rien cette fois — garder la dernière valeur.
            g.last.connected = true;
            g.last.error = None;
        }
    }
    if !g.timer {
        g.timer = true;
        arm_timer();
    }
    g.last.clone()
}
