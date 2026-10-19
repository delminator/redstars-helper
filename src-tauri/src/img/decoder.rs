//! Le décodeur INT8 du site (`decoder.graph.onnx` + `decoder.int8`), embarqué dans le
//! binaire et exécuté par tract (ONNX en Rust pur : aucune bibliothèque native à livrer
//! sur Linux, macOS ni Windows).
//!
//! Le graphe porte ses poids en données EXTERNES (`decoder.weights`) ; on ne livre pas ce
//! fichier : on déquantifie `decoder.int8` en mémoire (même calcul que
//! `packages/render/src/int8-weights.ts` et que `_dequantize` de img_render.py) et on
//! injecte les tenseurs float32 en `raw_data` des initialiseurs avant de construire le
//! modèle — tract préfère `raw_data` aux données externes.

use std::collections::HashMap;

use tract_onnx::pb::tensor_proto::DataLocation;
use tract_onnx::prelude::*;
use tract_onnx::tract_core::internal::{anyhow, bail};
use tract_onnx::prelude::multithread::Executor;

use super::{DECODE_SIZE, LAT_C, LAT_H, LAT_W};

const GRAPH: &[u8] = include_bytes!("../../bundled/decoder.graph.onnx");
const WEIGHTS: &[u8] = include_bytes!("../../bundled/decoder.int8");

type Plan = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub(super) struct Decoder {
    plan: Plan,
}

fn u32le(b: &[u8], at: usize) -> TractResult<usize> {
    let s = b.get(at..at + 4).ok_or_else(|| anyhow!("decoder.int8 tronqué"))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// Conteneur `decoder.int8` → {nom: poids float32 (octets little-endian)}.
///   int8     w = f16( float32(q) × float32(échelle[canal]) )
///   float16  w = le demi-flottant décodé
/// f16 = arrondi IEEE binary16 au pair (le cast numpy, `half::f16::from_f32`).
/// Format : 'RSI8' · u32 version · u32 longueur d'entête · entête JSON · bourrage à 8 · données.
fn dequantize(buf: &[u8]) -> TractResult<HashMap<String, Vec<u8>>> {
    if buf.len() < 12 || &buf[..4] != b"RSI8" || u32le(buf, 4)? != 1 {
        bail!("decoder.int8 : format inconnu");
    }
    let n = u32le(buf, 8)?;
    let header: serde_json::Value = serde_json::from_slice(
        buf.get(12..12 + n).ok_or_else(|| anyhow!("decoder.int8 : entête tronquée"))?,
    )?;
    let base = (12 + n).div_ceil(8) * 8;
    let tensors = header["tensors"].as_array().ok_or_else(|| anyhow!("entête sans tensors"))?;
    let num = |v: &serde_json::Value| -> TractResult<usize> {
        v.as_u64().map(|x| x as usize).ok_or_else(|| anyhow!("entête : champ numérique manquant"))
    };
    let mut out = HashMap::new();
    for t in tensors {
        let name = t["name"].as_str().ok_or_else(|| anyhow!("tenseur sans nom"))?.to_string();
        let len = num(&t["len"])?;
        let data = base + num(&t["data"])?;
        let mut w = Vec::with_capacity(len * 4);
        if t["dtype"] == "int8" {
            let rows = num(&t["shape"][0])?;
            let per = len / rows.max(1);
            let q = buf.get(data..data + len).ok_or_else(|| anyhow!("{name} : données hors bornes"))?;
            let sb = base + num(&t["scales"])?;
            let sc = buf.get(sb..sb + rows * 4).ok_or_else(|| anyhow!("{name} : échelles hors bornes"))?;
            for (i, &qi) in q.iter().enumerate() {
                let k = (i / per) * 4;
                let s = f32::from_le_bytes([sc[k], sc[k + 1], sc[k + 2], sc[k + 3]]);
                let v = f16::from_f32((qi as i8) as f32 * s).to_f32();
                w.extend_from_slice(&v.to_le_bytes());
            }
        } else {
            let h = buf.get(data..data + len * 2).ok_or_else(|| anyhow!("{name} : données hors bornes"))?;
            for c in h.chunks_exact(2) {
                w.extend_from_slice(&f16::from_le_bytes([c[0], c[1]]).to_f32().to_le_bytes());
            }
        }
        out.insert(name, w);
    }
    Ok(out)
}

/// Pool de threads du produit matriciel : comme le helper Python (onnxruntime
/// intra_op_num_threads = cœurs − 1), plafonné à 8 (au-delà le gain est marginal).
/// Le découpage porte sur les tuiles de sortie, pas sur la somme : résultat identique
/// quel que soit le nombre de threads.
fn executor() -> Executor {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let n = n.saturating_sub(1).clamp(1, 8);
    if n == 1 {
        Executor::SingleThread
    } else {
        Executor::multithread_with_name(n, "rs-img")
    }
}

impl Decoder {
    pub(super) fn load() -> TractResult<Decoder> {
        let onnx = tract_onnx::onnx();
        let mut proto = onnx.proto_model_for_read(&mut &GRAPH[..])?;
        let mut weights = dequantize(WEIGHTS)?;
        let graph = proto.graph.as_mut().ok_or_else(|| anyhow!("graphe absent"))?;
        for init in graph.initializer.iter_mut() {
            let w = weights
                .remove(&init.name)
                .ok_or_else(|| anyhow!("poids manquant dans decoder.int8 : {}", init.name))?;
            let expect: i64 = init.dims.iter().product::<i64>() * 4;
            if w.len() as i64 != expect {
                bail!("{} : {} octets, attendu {}", init.name, w.len(), expect);
            }
            init.raw_data = w;
            init.external_data.clear();
            init.data_location = Some(DataLocation::Default as i32);
        }
        let plan = onnx
            .model_for_proto_model(&proto)?
            .with_input_fact(0, f32::fact([1, LAT_C, LAT_H, LAT_W]).into())?
            .into_optimized()?
            .into_runnable_with_options(&PlanOptions { executor: Some(executor()), ..Default::default() })?;
        Ok(Decoder { plan })
    }

    /// Latent (4·32·32 float32) → indices de palette 256×256 (argmax sur la classe ; à
    /// égalité exacte, le PREMIER indice gagne, comme numpy `argmax`).
    pub(super) fn run(&self, latent: Vec<f32>) -> TractResult<Vec<u8>> {
        let input = tract_ndarray::Array4::from_shape_vec((1, LAT_C, LAT_H, LAT_W), latent)?;
        let out = self.plan.run(tvec!(Tensor::from(input).into()))?;
        let logits = out[0].as_slice::<f32>()?;
        let px = DECODE_SIZE * DECODE_SIZE;
        if logits.len() != 256 * px {
            bail!("sortie inattendue : {} valeurs", logits.len());
        }
        // (1,256,H,W) classes d'abord : on balaie classe par classe, en gardant le max
        // strict par pixel — parcours mémoire contigu.
        let mut best = logits[..px].to_vec();
        let mut idx = vec![0u8; px];
        for c in 1..256 {
            let plane = &logits[c * px..(c + 1) * px];
            for p in 0..px {
                if plane[p] > best[p] {
                    best[p] = plane[p];
                    idx[p] = c as u8;
                }
            }
        }
        Ok(idx)
    }
}
