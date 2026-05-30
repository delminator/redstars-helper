#!/usr/bin/env python3
"""redEC — BOÎTE NOIRE miroir EXACT de redDEC. Compresse un fichier binaire en 1 hash.

Pipeline interne (1 unité = mirror de redDEC, 1 Mo → 1 hash) :
   Entrée : 1 Mo = 1024 hashes × 1024 o.
   1. réseau LATENT decode sur chaque 8192-bit hash → 1024 × image 32×32 (palette indices)
   2. assemblage des 1024 cases en mosaïque 1024×1024 px (= 1024 cascade snapshots)
   3. cascade inverse → snapshot 0 (= la case 32×32 d'origine) = top-left 32×32 de la mosaïque
   4. réseau IMG encode sur cette case 32×32 → 1 hash de 8192 bits
   Sortie : 1024 octets = 8192 bits = 1 BYTEA.

Niveau de chaînage AUTO selon la taille du fichier d'entrée :
   ≤ 1 Mo  → Red1  (1 application)
   ≤ 1 Gio → Red2  (2 applications chaînées : input → 1 Mo → 1 hash)
   ≤ 1 TiB → Red3  (3 applications)
   ≤ 1 PiB → Red4  (4 applications)

Round-trip avec redDEC : redEC(redDEC(hash)) == hash  (réseau bijectif).

Usage CLI :
    python3 redEC.py <input_file> [output_file]
    python3 redEC.py output32/chaine_etape2_out.bin output32/redEC_out.bin
"""
import sys, json, argparse
from pathlib import Path
import numpy as np
import torch, torch.nn.functional as F
from train_elim import Ensemble, to_bits, CH

ROOT   = Path(__file__).parent
DEV    = "cuda" if torch.cuda.is_available() else "cpu"
BASE   = ROOT/"final"/"solo_champion.pth"
CK_LAT = ROOT/"final"/"solo_champion_latent.pth"
CK_IMG = ROOT/"final"/"solo_champion_img.pth"

def _load(p):
    q = p if p.exists() else BASE
    n = Ensemble(1, CH).to(DEV); ck = torch.load(q, map_location=DEV)
    n.load_state_dict(ck["model"] if "model" in ck else ck); n.eval()
    for w in n.parameters(): w.requires_grad_(False)
    return n
_net_lat = _load(CK_LAT)
_net_img = _load(CK_IMG)

@torch.no_grad()
def _dec(z):                                          # (B,8,32,32) → (B,32,32)
    out = []
    for i in range(0, len(z), 256):
        zt = torch.from_numpy(z[i:i+256].astype(np.float32)).to(DEV)
        out.append(_net_lat.d2(F.gelu(_net_lat.d1(zt))).argmax(1).cpu().numpy().astype(np.uint8))
    return np.concatenate(out)

@torch.no_grad()
def _enc(idx):                                        # (B,32,32) → (B,8,32,32)
    out = []
    for i in range(0, len(idx), 256):
        x = torch.from_numpy(idx[i:i+256].astype(np.int64)).to(DEV)
        out.append(torch.round(torch.sigmoid(_net_img.e2(F.gelu(_net_img.e1(to_bits(x))))).clamp(0,1)).cpu().numpy().astype(np.uint8))
    return np.concatenate(out)

UNIT = 1024 * 1024  # 1 Mo

def _level_for_size(size: int) -> int:
    """≤ 1 Mo → 1 (Red1) · ≤ 1 Gio → 2 (Red2) · ≤ 1 TiB → 3 · ≤ 1 PiB → 4"""
    if size <= UNIT:                return 1
    if size <= UNIT * 1024:         return 2
    if size <= UNIT * 1024 ** 2:    return 3
    return 4

def redEC_unit_batched(chunks_list):
    """N chunks de 1 Mo → N hashes de 1024 o chacun.
       Pipeline : LATENT decode tous les 1024 hashes du chunk, on prend le snapshot 0
       (= top-left 32×32 de la mosaïque, = cascade inverse trivialement = LATENT_decode du 1er hash),
       puis IMG encode → 1 hash."""
    N = len(chunks_list)
    first_hashes = np.zeros((N, 1024), dtype=np.uint8)
    for k in range(N):
        first_hashes[k] = np.frombuffer(chunks_list[k][:1024], np.uint8)
    lats   = np.unpackbits(first_hashes, axis=1).reshape(N, 8, 32, 32)
    cases  = _dec(lats)                                # (N, 32, 32) = snapshot 0 de chaque chunk
    encs   = _enc(cases)                               # (N, 8, 32, 32) bits
    outs   = np.packbits(encs.reshape(N, -1), axis=1)  # (N, 1024)
    return [bytes(outs[k]) for k in range(N)]

def _apply_one_step(in_path: Path, out_path: Path, batch_size: int = 32, prog_cb=None, label: str = ""):
    """1 application de redEC : lit le fichier par chunks de 1 Mo, écrit 1 hash (1024 o) par chunk."""
    in_size = in_path.stat().st_size
    n_chunks = (in_size + UNIT - 1) // UNIT
    with open(in_path, "rb") as in_f, open(out_path, "wb") as out_f:
        chunk_idx = 0
        while chunk_idx < n_chunks:
            this_n = min(batch_size, n_chunks - chunk_idx)
            batch = []
            for _ in range(this_n):
                chunk = in_f.read(UNIT)
                if len(chunk) < UNIT:
                    chunk = chunk + b'\x00' * (UNIT - len(chunk))      # zero-pad le dernier chunk
                batch.append(chunk)
            outs = redEC_unit_batched(batch)
            for h in outs: out_f.write(h)
            chunk_idx += this_n
            if prog_cb: prog_cb(chunk_idx, n_chunks, label)

def redEC_chain(input_path: Path, output_path: Path, work_dir: Path = None, prog_cb=None):
    """Applique redEC en chaîne (1 à 4 étapes selon la taille) jusqu'à obtenir 1 hash de 1024 o."""
    if work_dir is None: work_dir = output_path.parent
    work_dir.mkdir(parents=True, exist_ok=True)
    in_size = input_path.stat().st_size
    level   = _level_for_size(in_size)

    current = input_path
    intermediates = []
    for step in range(level):
        is_final = (step == level - 1)
        next_path = output_path if is_final else (work_dir / f"redEC_tmp_step{step+1}.bin")
        if not is_final: intermediates.append(next_path)
        _apply_one_step(current, next_path, prog_cb=prog_cb, label=f"step {step+1}/{level}")
        if current != input_path: current.unlink(missing_ok=True)
        current = next_path

    final_bytes = output_path.read_bytes()[:1024]
    return level, final_bytes, in_size

if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("input",  help="fichier binaire d'entrée (taille arbitraire)")
    p.add_argument("output", nargs="?", default="output32/redEC_out.bin", help="fichier de sortie (= 1 hash 1024 o)")
    args = p.parse_args()

    in_path  = Path(args.input)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    def _cli_prog(k, n, lbl):
        print(f"  [{lbl}] chunk {k}/{n}", end="\r", flush=True)

    level, h, in_size = redEC_chain(in_path, out_path, prog_cb=_cli_prog)
    print()

    st = {
        "input_file":      str(in_path),
        "input_bytes":     in_size,
        "level":           f"Red{level}",
        "n_chain_steps":   level,
        "output_hash_hex": h.hex(),
        "output_bytes":    len(h),
        "output_file":     str(out_path),
    }
    json.dump(st, open(out_path.parent / "redEC_status.json", "w"))
    print(f"redEC : input {in_size:,} o → Red{level} ({level} step{'s' if level>1 else ''}) → hash {len(h)} o ({h.hex()[:32]}…)")
