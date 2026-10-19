//! Décodeur d'images `nn:` côté helper — portage natif de `bundled/img_render.py`.
//!
//! Les images du réseau (avatars, vignettes) sont stockées en `nn:` hash (un latent
//! compact 4×32×32). Le helper :
//!   1. décode le hash → indices de palette 256×256 avec le décodeur INT8 du SITE
//!      (mêmes fichiers que /models du frontend, embarqués dans le binaire, voir decoder.rs),
//!   2. sous-échantillonne la grille vers une petite tuile (moyenne RGB, re-quantifiée),
//!   3. la rend en texte quadrant truecolor — le MÊME algorithme que le moteur TS
//!      (`icon-image.ts` renderIconImage), porté fidèlement,
//! et la console blitte la tuile dans la trame.
//!
//! Parité octet pour octet avec img_render.py : voir `tests/img_parity.rs`.

mod decoder;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;

/// Latent du décodeur : 4 canaux × 32 × 32 (nn-decoder.ts LATENT_C/H/W).
pub const LAT_C: usize = 4;
pub const LAT_H: usize = 32;
pub const LAT_W: usize = 32;
/// Sortie du modèle : 256×256 indices (nn-decoder DECODE_SIZE).
pub const DECODE_SIZE: usize = 256;
/// Indice transparent du moteur de rendu (icon-image).
pub const TRANSPARENT: u8 = 255;

pub type Rgb = (u8, u8, u8);

/// Arrondi identique à JS `Math.round` : au plus proche, 0.5 vers +∞ (floor(x+0.5)).
/// Pas d'arrondi bancaire : la palette tombe pile sur des .5.
fn js_round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

// ── Palette canonique 4×4×4×4 — IDENTIQUE à buildUniformPalette (décodeur) et à
//    buildPalette (icon-image). ──────────────────────────────────────────────────
fn build_palette() -> [Rgb; 256] {
    let mut p = [(0u8, 0u8, 0u8); 256];
    let ch = |v: usize, s: f64| js_round((v as f64 * 85.0 + s).clamp(0.0, 255.0)) as u8;
    let mut i = 0;
    for r in 0..4 {
        for g in 0..4 {
            for b in 0..4 {
                for l in 0..4 {
                    let s = (l as f64 - 1.5) * 25.0;
                    p[i] = (ch(r, s), ch(g, s), ch(b, s));
                    i += 1;
                }
            }
        }
    }
    p
}

/// La palette (256 entrées r,g,b).
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn palette() -> &'static [Rgb; 256] {
    static P: OnceLock<[Rgb; 256]> = OnceLock::new();
    P.get_or_init(build_palette)
}

// ── Décodage nn: hash → indices 256×256 ─────────────────────────────────────────

fn decoder() -> Option<&'static decoder::Decoder> {
    static D: OnceLock<Option<decoder::Decoder>> = OnceLock::new();
    D.get_or_init(|| decoder::Decoder::load().ok()).as_ref()
}

/// Le décodeur est-il utilisable (modèle embarqué chargé et optimisé) ? Le premier appel
/// charge le modèle (une fois pour toutes, puis gardé en cache).
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn capable() -> bool {
    decoder().is_some()
}

/// Taille au-delà de laquelle un cache est vidé (évite une croissance sans fin dans une
/// longue session ; Python ne bornait pas — un avatar ne coûte que 64 Ko).
const CACHE_MAX: usize = 512;

fn cache_put<K: std::hash::Hash + Eq, V>(m: &mut HashMap<K, V>, k: K, v: V) {
    if m.len() >= CACHE_MAX {
        m.clear();
    }
    m.insert(k, v);
}

/// `nn:<base64>` → latent float32 (4·32·32, ordre C de (1,4,32,32)). Deux formats,
/// comme unpackLatent :
///   - 16384 octets : float32 brut little-endian (sortie encodeur) ;
///   - sinon : legacy 2 bits/valeur, 4 valeurs par octet, MSB d'abord, niveaux {-3,-1,1,3}
///     (au plus 1024 octets lus ; si moins, le reste vaut 0 — numpy laissait de la mémoire
///     non initialisée).
///
/// None si le base64 est invalide.
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn unpack_latent(b64hash: &str) -> Option<Vec<f32>> {
    let b64 = b64hash.strip_prefix("nn:").unwrap_or(b64hash);
    // base64.b64decode (validate=False) ignore les caractères hors alphabet.
    let clean: String = b64
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=')
        .collect();
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    let raw = engine.decode(clean.trim_end_matches('=')).ok()?;
    let total = LAT_C * LAT_H * LAT_W;
    if raw.len() == total * 4 {
        return Some(
            raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        );
    }
    let mut vals = vec![0f32; total];
    for (i, &byte) in raw.iter().take(1024).enumerate() {
        for k in 0..4 {
            let q = (byte >> (6 - 2 * k)) & 3;
            vals[i * 4 + k] = q as f32 * 2.0 - 3.0;
        }
    }
    Some(vals)
}

/// `nn:` hash → indices de palette (256·256, ligne par ligne), ou None si le décodeur
/// manque ou le hash est illisible. Mis en cache : un avatar se redessine à chaque touche.
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn decode_hash(b64hash: &str) -> Option<Vec<u8>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().ok()?.get(b64hash) {
        return Some(v.clone());
    }
    let dec = decoder()?;
    let latent = unpack_latent(b64hash)?;
    let idx = dec.run(latent).ok()?;
    cache_put(&mut *cache.lock().ok()?, b64hash.to_string(), idx.clone());
    Some(idx)
}

/// `nn:` hash → lignes ANSI d'une tuile de `w_cells`×`h_cells` cellules (prêtes à blitter),
/// ou None si le décodeur manque. Décodage + sous-échantillonnage + rendu mis en cache.
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn render_tile(b64hash: &str, w_cells: usize, h_cells: usize) -> Option<Vec<String>> {
    if w_cells == 0 || h_cells == 0 {
        return None;
    }
    type Key = (String, usize, usize);
    static CACHE: OnceLock<Mutex<HashMap<Key, Vec<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = (b64hash.to_string(), w_cells, h_cells);
    if let Some(v) = cache.lock().ok()?.get(&key) {
        return Some(v.clone());
    }
    let idx = decode_hash(b64hash)?;
    // Une cellule = 2 sous-pixels de haut : pour h_cells lignes il faut 2*h_cells pixels.
    let small = downsample_indices(&idx, DECODE_SIZE, DECODE_SIZE, w_cells, h_cells * 2);
    let lines = render_ansi_lines(&small, w_cells, h_cells * 2);
    cache_put(&mut *cache.lock().ok()?, key, lines.clone());
    Some(lines)
}

// ── Sous-échantillonnage vers une petite tuile (moyenne en RGB, re-quantifié) ────

fn nearest_index(r: i32, g: i32, b: i32) -> u8 {
    let (mut best, mut bd) = (0usize, i32::MAX);
    for (i, &(pr, pg, pb)) in palette().iter().enumerate() {
        let d = (pr as i32 - r).pow(2) + (pg as i32 - g).pow(2) + (pb as i32 - b).pow(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best as u8
}

/// Indices (`h`×`w`, ligne par ligne) → petite grille d'indices (`dh`×`dw`). Moyenne par
/// bloc EN RGB puis re-quantifiée sur la palette.
///
/// Reproduit le chemin numpy de img_render.py : sommes entières (exactes en float32 tant
/// que < 2^24 — 256·256·255 l'est), moyenne = division float32 correctement arrondie,
/// puis `round()` de numpy sur float32 = arrondi au PAIR (≠ js_round).
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn downsample_indices(idx: &[u8], w: usize, h: usize, dw: usize, dh: usize) -> Vec<u8> {
    let pal = palette();
    let mut out = vec![0u8; dw * dh];
    for j in 0..dh {
        let y0 = j * h / dh;
        let y1 = ((j + 1) * h / dh).max(y0 + 1);
        for i in 0..dw {
            let x0 = i * w / dw;
            let x1 = ((i + 1) * w / dw).max(x0 + 1);
            let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    let (r, g, b) = pal[idx[y * w + x] as usize];
                    sr += r as u64;
                    sg += g as u64;
                    sb += b as u64;
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let m = |s: u64| ((s as f32) / (n as f32)).round_ties_even() as i32;
            out[j * dw + i] = nearest_index(m(sr), m(sg), m(sb));
        }
    }
    out
}

// ── Rendu quadrant → texte ANSI truecolor — PORT FIDÈLE de icon-image.ts ─────────

/// 16 glyphes quadrant, indexés par un masque 4 bits (TL=1, TR=2, BL=4, BR=8).
const QUAD: [char; 16] =
    [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█'];
/// En deçà de cet écart, le bloc 2×2 est une seule couleur.
const FLAT: i32 = 24 * 24 * 3;

/// Une cellule rendue : glyphe + couleurs (RGB exact).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

fn px(idx: &[u8], w: usize, h: usize, x: isize, y: isize) -> Option<Rgb> {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
        return None;
    }
    let k = idx[y as usize * w + x as usize];
    if k == TRANSPARENT {
        None
    } else {
        Some(palette()[k as usize])
    }
}

fn half(idx: &[u8], w: usize, h: usize, x: isize, y: isize) -> Option<Rgb> {
    let a = px(idx, w, h, x, y);
    let b = px(idx, w, h, x + 1, y);
    match (a, b) {
        (Some(a), Some(b)) => {
            let m = |p: u8, q: u8| js_round((p as f64 + q as f64) / 2.0) as u8;
            Some((m(a.0, b.0), m(a.1, b.1), m(a.2, b.2)))
        }
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

fn avg(cs: &[Rgb]) -> Rgb {
    let n = cs.len() as f64;
    let s = |f: fn(&Rgb) -> u8| js_round(cs.iter().map(|c| f(c) as f64).sum::<f64>() / n) as u8;
    (s(|c| c.0), s(|c| c.1), s(|c| c.2))
}

fn d2(a: Rgb, b: Rgb) -> i32 {
    (a.0 as i32 - b.0 as i32).pow(2) + (a.1 as i32 - b.1 as i32).pow(2) + (a.2 as i32 - b.2 as i32).pow(2)
}

/// Grille d'indices (`h`×`w`) → lignes de cellules. `w` cellules de large, ceil(h/2) de
/// haut : mêmes dimensions que le renderer TS.
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn render_cells(idx: &[u8], w: usize, h: usize) -> Vec<Vec<Cell>> {
    let mut rows = Vec::with_capacity(h.div_ceil(2));
    for y in (0..h).step_by(2) {
        let y = y as isize;
        let mut row = Vec::with_capacity(w);
        for x in 0..w {
            let x = x as isize;
            let sub = [px(idx, w, h, x, y), half(idx, w, h, x, y), px(idx, w, h, x, y + 1), half(idx, w, h, x, y + 1)];
            let mut mask = 0usize;
            let mut opaque: Vec<Rgb> = Vec::with_capacity(4);
            for (i, s) in sub.iter().enumerate() {
                if let Some(c) = s {
                    mask |= 1 << i;
                    opaque.push(*c);
                }
            }
            if mask == 0 {
                row.push(Cell { ch: ' ', fg: None, bg: None });
                continue;
            }
            if mask != 0b1111 {
                row.push(Cell { ch: QUAD[mask], fg: Some(avg(&opaque)), bg: None });
                continue;
            }
            // 4 sous-pixels opaques → 2 couleurs (pôles = paire la plus éloignée).
            let s: [Rgb; 4] = [opaque[0], opaque[1], opaque[2], opaque[3]];
            let (mut p0, mut p1, mut bd) = (0, 1, -1);
            for i in 0..4 {
                for j in i + 1..4 {
                    let d = d2(s[i], s[j]);
                    if d > bd {
                        bd = d;
                        p0 = i;
                        p1 = j;
                    }
                }
            }
            if bd < FLAT {
                row.push(Cell { ch: '█', fg: Some(avg(&opaque)), bg: None });
                continue;
            }
            let mut fg_mask = 0usize;
            let (mut fg, mut bg) = (Vec::with_capacity(4), Vec::with_capacity(4));
            for (i, &c) in s.iter().enumerate() {
                if d2(c, s[p0]) <= d2(c, s[p1]) {
                    fg_mask |= 1 << i;
                    fg.push(c);
                } else {
                    bg.push(c);
                }
            }
            row.push(Cell { ch: QUAD[fg_mask], fg: Some(avg(&fg)), bg: Some(avg(&bg)) });
        }
        rows.push(row);
    }
    rows
}

/// Grille d'indices → lignes ANSI truecolor autonomes (chaque ligne se referme par un
/// reset). Pour un blit ligne à ligne à une position curseur donnée.
#[allow(dead_code)] // API publique (console, tests/img_parity.rs)
pub fn render_ansi_lines(idx: &[u8], w: usize, h: usize) -> Vec<String> {
    render_cells(idx, w, h)
        .into_iter()
        .map(|row| {
            let mut buf = String::new();
            let mut cur: Option<String> = None;
            for cell in row {
                let code = if cell.fg.is_none() && cell.bg.is_none() {
                    None
                } else {
                    let mut parts = Vec::with_capacity(2);
                    if let Some((r, g, b)) = cell.fg {
                        parts.push(format!("38;2;{r};{g};{b}"));
                    }
                    if let Some((r, g, b)) = cell.bg {
                        parts.push(format!("48;2;{r};{g};{b}"));
                    }
                    Some(parts.join(";"))
                };
                if code != cur {
                    match &code {
                        None => buf.push_str("\x1b[0m"),
                        Some(c) => {
                            buf.push_str("\x1b[");
                            buf.push_str(c);
                            buf.push('m');
                        }
                    }
                    cur = code;
                }
                buf.push(cell.ch);
            }
            buf.push_str("\x1b[0m");
            buf
        })
        .collect()
}
