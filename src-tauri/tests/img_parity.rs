//! Parité du décodeur/rendu Rust (src/img/) avec la référence Python img_render.py.
//!
//!   python3 tests/img_parity.py OUTDIR hashes.txt      # venv numpy + onnxruntime
//!   IMG_PARITY_DIR=OUTDIR cargo test --release --test img_parity -- --nocapture
//!
//! Sans IMG_PARITY_DIR, seul le test autonome (chargement du modèle + rendu d'un hash) tourne.
//! `img` est un module privé de la lib : on l'inclut ici par chemin.

#[allow(dead_code)]
#[path = "../src/img/mod.rs"]
mod img;

use base64::Engine as _;
use serde_json::Value;
use std::time::Instant;

fn b64(v: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(v.as_str().unwrap()).unwrap()
}

fn cells_json(rows: &[Vec<img::Cell>]) -> Value {
    let c = |o: Option<img::Rgb>| match o {
        Some((r, g, b)) => serde_json::json!([r, g, b]),
        None => Value::Null,
    };
    Value::Array(
        rows.iter()
            .map(|row| {
                Value::Array(
                    row.iter().map(|x| serde_json::json!([x.ch.to_string(), c(x.fg), c(x.bg)])).collect(),
                )
            })
            .collect(),
    )
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

#[test]
fn autonome() {
    assert!(img::capable(), "le modèle embarqué doit se charger");
    let h = format!("nn:{}", base64::engine::general_purpose::STANDARD.encode([0x1bu8; 1024]));
    let idx = img::decode_hash(&h).expect("décodage");
    assert_eq!(idx.len(), 256 * 256);
    let lines = img::render_tile(&h, 8, 4).expect("tuile");
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|l| l.ends_with("\x1b[0m")));
}

#[test]
fn parite_python() {
    let Ok(dir) = std::env::var("IMG_PARITY_DIR") else {
        eprintln!("IMG_PARITY_DIR non défini : parité Python sautée");
        return;
    };
    let data: Value =
        serde_json::from_slice(&std::fs::read(format!("{dir}/cases.json")).unwrap()).unwrap();

    // Palette.
    let pal: Vec<img::Rgb> = data["palette"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_u64().unwrap() as u8, p[1].as_u64().unwrap() as u8, p[2].as_u64().unwrap() as u8))
        .collect();
    assert_eq!(&pal[..], &img::palette()[..], "palette");

    let t0 = Instant::now();
    assert!(img::capable());
    eprintln!("chargement du modèle : {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);

    let cases = data["decode"].as_array().unwrap();
    let (mut exact, mut px_diff, mut max_diff_margin) = (0usize, 0usize, 0f32);
    let mut worst = 0usize;
    let (mut rs_ms, mut py_ms) = (Vec::new(), Vec::new());
    let (mut tiles_ok, mut tiles_n, mut rtile_ok) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for (k, c) in cases.iter().enumerate() {
        let h = c["hash"].as_str().unwrap();
        // Latent.
        let lat: Vec<u8> = img::unpack_latent(h).unwrap().iter().flat_map(|f| f.to_le_bytes()).collect();
        assert_eq!(lat, b64(&c["latent"]), "unpack_latent #{k}");
        // Décodage.
        let py = b64(&c["idx"]);
        let margin: Vec<f32> =
            b64(&c["margin"]).chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let t = Instant::now();
        let rs = img::decode_hash(h).expect("decode_hash");
        rs_ms.push(t.elapsed().as_secs_f64() * 1e3);
        py_ms.push(c["py_ms"].as_f64().unwrap());
        let diffs: Vec<usize> = (0..py.len()).filter(|&i| py[i] != rs[i]).collect();
        if diffs.is_empty() {
            exact += 1;
        } else {
            let mm = diffs.iter().map(|&i| margin[i]).fold(0f32, f32::max);
            eprintln!(
                "#{k}: {} pixels différents, marge top1-top2 Python max {:.2e} (aux pixels écartés)",
                diffs.len(),
                mm
            );
            px_diff += diffs.len();
            worst = worst.max(diffs.len());
            max_diff_margin = max_diff_margin.max(mm);
        }
        // Rendu, sur la grille PYTHON (parité du rendu indépendante du décodeur).
        for t in c["tiles"].as_array().unwrap() {
            let (w, hc) = (t["w"].as_u64().unwrap() as usize, t["h"].as_u64().unwrap() as usize);
            tiles_n += 1;
            let small = img::downsample_indices(&py, 256, 256, w, hc * 2);
            let mut ok = true;
            if small != b64(&t["small"]) {
                failures.push(format!("#{k} {w}x{hc}: downsample"));
                ok = false;
            }
            let ps = b64(&t["small"]);
            if cells_json(&img::render_cells(&ps, w, hc * 2)) != t["cells"] {
                failures.push(format!("#{k} {w}x{hc}: render_cells"));
                ok = false;
            }
            if img::render_ansi_lines(&ps, w, hc * 2) != strings(&t["ansi"]) {
                failures.push(format!("#{k} {w}x{hc}: render_ansi_lines"));
                ok = false;
            }
            tiles_ok += ok as usize;
            // Bout en bout (décodeur Rust compris) : identique dès que le décodage l'est.
            if img::render_tile(h, w, hc).unwrap() == strings(&t["tile"]) {
                rtile_ok += 1;
            } else if diffs.is_empty() {
                failures.push(format!("#{k} {w}x{hc}: render_tile alors que le décodage est identique"));
            }
        }
        if !c["full_ansi"].is_null() && img::render_ansi_lines(&py, 256, 256) != strings(&c["full_ansi"]) {
            failures.push(format!("#{k}: render_ansi_lines 256x256"));
        }
    }
    // Grilles synthétiques (transparence, tailles impaires).
    let grids = data["grids"].as_array().unwrap();
    for (k, g) in grids.iter().enumerate() {
        let (w, h) = (g["w"].as_u64().unwrap() as usize, g["h"].as_u64().unwrap() as usize);
        let idx = b64(&g["idx"]);
        if cells_json(&img::render_cells(&idx, w, h)) != g["cells"] {
            failures.push(format!("grille #{k} {w}x{h}: render_cells"));
        }
        if img::render_ansi_lines(&idx, w, h) != strings(&g["ansi"]) {
            failures.push(format!("grille #{k} {w}x{h}: render_ansi_lines"));
        }
    }

    let n = cases.len();
    let med = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    eprintln!("── décodage : {exact}/{n} grilles identiques ; {px_diff} pixels différents au total sur {} (pire image : {worst}) ; marge Python max aux écarts : {max_diff_margin:.2e}", n * 65536);
    eprintln!("── rendu (grille Python) : {tiles_ok}/{tiles_n} tuiles identiques (downsample+cells+ansi) ; {} grilles synthétiques", grids.len());
    eprintln!("── render_tile bout en bout : {rtile_ok}/{tiles_n} identiques");
    eprintln!("── temps de décodage médian : Rust {:.0} ms, Python/onnxruntime {:.0} ms", med(&mut rs_ms), med(&mut py_ms));
    for f in &failures {
        eprintln!("ÉCHEC {f}");
    }
    assert!(failures.is_empty(), "{} écarts de rendu", failures.len());
}
