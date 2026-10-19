//! Certificat local.redlinks.fr (Let's Encrypt) du serveur HTTPS :49443.
//!
//! Ordre de recherche (le premier trouvé gagne) :
//!   1. `$REDSTARS_HELPER_CERT_DIR/{cert.pem,key.pem}`
//!   2. le dossier data local de l'app (`fr.redlinks.redstars-helper/`)
//!      `cert.pem` + `key.pem`
//!   3. les constantes `_EMBEDDED_CERT_B64` / `_EMBEDDED_KEY_B64` du helper.py
//!      signé en cache dans ce même dossier — c'est ainsi que le parc reçoit
//!      les certificats renouvelés : un helper.py signé est publié à chaque
//!      renouvellement, et le shell le télécharge et le vérifie déjà.
//!   4. la copie embarquée à la compilation (`bundled/cert.pem`, `key.pem`).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine as _;
use regex::Regex;

const IDENTIFIER: &str = "fr.redlinks.redstars-helper";
static EMBEDDED_CERT: &[u8] = include_bytes!("../../bundled/cert.pem");
static EMBEDDED_KEY: &[u8] = include_bytes!("../../bundled/key.pem");

fn data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join(IDENTIFIER))
}

fn looks_like_pem(cert: &[u8], key: &[u8]) -> bool {
    let has = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).any(|w| w == needle);
    has(cert, b"-----BEGIN CERTIFICATE-----") && has(key, b"PRIVATE KEY-----")
}

fn read_pair(dir: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    let cert = std::fs::read(dir.join("cert.pem")).ok()?;
    let key = std::fs::read(dir.join("key.pem")).ok()?;
    looks_like_pem(&cert, &key).then_some((cert, key))
}

/// Extrait `_EMBEDDED_CERT_B64` / `_EMBEDDED_KEY_B64` d'un helper.py.
pub fn extract_from_helper_py(path: &Path) -> Option<(Vec<u8>, Vec<u8>)> {
    static CERT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?m)^_EMBEDDED_CERT_B64\s*=\s*['"]([A-Za-z0-9+/=\s]+)['"]"#).expect("cert"));
    static KEY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?m)^_EMBEDDED_KEY_B64\s*=\s*['"]([A-Za-z0-9+/=\s]+)['"]"#).expect("key"));
    let text = std::fs::read_to_string(path).ok()?;
    let dec = |re: &Regex| -> Option<Vec<u8>> {
        let b64: String = re.captures(&text)?[1].chars().filter(|c| !c.is_whitespace()).collect();
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    };
    let (cert, key) = (dec(&CERT)?, dec(&KEY)?);
    looks_like_pem(&cert, &key).then_some((cert, key))
}

/// Les sources dans l'ordre, chacune avec son nom (pour les journaux).
pub(crate) fn sources() -> Vec<(String, (Vec<u8>, Vec<u8>))> {
    let mut out = Vec::new();
    if let Some(d) = std::env::var_os("REDSTARS_HELPER_CERT_DIR").filter(|d| !d.is_empty()) {
        let d = PathBuf::from(d);
        if let Some(p) = read_pair(&d) {
            out.push((d.display().to_string(), p));
        }
    }
    if let Some(d) = data_dir() {
        if let Some(p) = read_pair(&d) {
            out.push((d.display().to_string(), p));
        }
        let py = d.join("helper.py");
        if let Some(p) = extract_from_helper_py(&py) {
            out.push((py.display().to_string(), p));
        }
    }
    out.push(("embarqué".to_string(), (EMBEDDED_CERT.to_vec(), EMBEDDED_KEY.to_vec())));
    out
}

/// (chaîne de certificats PEM, clé PEM) — la première source trouvée.
#[allow(dead_code)] // API publique ; le serveur, lui, essaie les sources une à une
pub fn load_pem() -> (Vec<u8>, Vec<u8>) {
    sources().into_iter().next().map(|(_, p)| p).unwrap_or_else(|| (EMBEDDED_CERT.to_vec(), EMBEDDED_KEY.to_vec()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn extract_bundled_helper_py() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bundled/helper.py");
        let (c, k) = super::extract_from_helper_py(&p).expect("constantes présentes");
        assert!(c.starts_with(b"-----BEGIN CERTIFICATE-----"));
        assert!(k.starts_with(b"-----BEGIN PRIVATE KEY-----"));
    }
}
