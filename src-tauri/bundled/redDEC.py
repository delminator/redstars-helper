#!/usr/bin/env python3
"""redDEC — BOÎTE NOIRE : 1 hash (8192 bits = 1 BYTEA) → 1024 hashes (1 Mo).

Pipeline interne :
  Entrée   : 8192 bits raw (= 1024 octets = 1 BYTEA).
  Étape 1  : réseau LATENT decode → image 32×32 indices palette.
  Étape 2  : cascade d'étalement → 1024 snapshots 32×32 → grille 1024×1024 indices
             (chaque tour de cascade = 1 nouvel état 32×32, on garde tous les états).
  Étape 3  : réseau IMG encode sur chaque carré 32×32 de la grille 1024×1024 → 1024 hashes.
  Sortie   : concat des 1024 hashes = 1 048 576 octets = 1 Mo.

Usage CLI : python3 redDEC.py <input_hash_1024o> [output_1Mo]
"""
import sys, numpy as np, torch, torch.nn.functional as F
from pathlib import Path
from train_elim import Ensemble, to_bits, CH
from reorg_codec import xorbidir

ROOT  = Path(__file__).parent
DEV   = "cuda" if torch.cuda.is_available() else "cpu"
BASE  = ROOT/"final"/"solo_champion.pth"
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
def _dec(z):                                            # (B,8,32,32) → (B,32,32)
    out = []
    for i in range(0, len(z), 256):
        zt = torch.from_numpy(z[i:i+256].astype(np.float32)).to(DEV)
        out.append(_net_lat.d2(F.gelu(_net_lat.d1(zt))).argmax(1).cpu().numpy().astype(np.uint8))
    return np.concatenate(out)

@torch.no_grad()
def _enc(idx):                                          # (B,32,32) → (B,8,32,32)
    out = []
    for i in range(0, len(idx), 256):
        x = torch.from_numpy(idx[i:i+256].astype(np.int64)).to(DEV)
        out.append(torch.round(torch.sigmoid(_net_img.e2(F.gelu(_net_img.e1(to_bits(x))))).clamp(0,1)).cpu().numpy().astype(np.uint8))
    return np.concatenate(out)

def _cascade_snapshots(case_in: np.ndarray, n_states: int = 1024, N: int = 32):
    """Snapshooter la cascade reorg à chaque tour : n_states états (N×N) distincts.
       Vectorisé numpy par tour : ~ms par tour au lieu de 50ms en boucle Python."""
    g = case_in.astype(np.int64).copy()
    states = np.zeros((n_states, N*N), dtype=np.uint8)
    states[0] = case_in.astype(np.uint8)
    for r in range(1, n_states):
        k = r - 1
        s = k % N
        offset = k % 256
        if k & 1:
            pcs  = np.arange(N//2)
            c1   = (2*pcs + s) % N
            c2   = (2*pcs + 1 + s) % N
            rows = np.arange(N)
            idx1 = (rows[:, None] * N + c1[None, :]).flatten()
            idx2 = (rows[:, None] * N + c2[None, :]).flatten()
        else:
            pcs  = np.arange(N//2)
            r1   = (2*pcs + s) % N
            r2   = (2*pcs + 1 + s) % N
            cols = np.arange(N)
            idx1 = (r1[:, None] * N + cols[None, :]).flatten()
            idx2 = (r2[:, None] * N + cols[None, :]).flatten()
        v1 = g[idx1]; v2 = g[idx2]
        AA = (v1 >> 4) & 0xF; BB = v1 & 0xF
        AB = (v2 >> 4) & 0xF; BA = v2 & 0xF
        g[idx1] = ((((AA ^ AB ^ BA) << 4) | (BB ^ AB ^ BA)) + offset) & 0xFF
        g[idx2] = (((AB << 4) | BA) + offset) & 0xFF
        states[r] = g.astype(np.uint8)
    return states                                       # (n_states, N*N)

def redDEC(hash_bytes) -> bytes:
    """1 hash (1024 o = 8192 bits raw) → 1 Mo (= 1024 hashes BYTEA concaténés)."""
    arr = np.frombuffer(bytes(hash_bytes), np.uint8)[:1024]
    if arr.size != 1024:
        raise ValueError(f"redDEC besoin de 1024 octets en entrée, reçu {arr.size}")
    # 1. LATENT decode → 32×32 indices
    lat = np.unpackbits(arr).reshape(8, 32, 32)
    img32 = _dec(lat[None])[0]                          # (32,32) indices palette
    # 2. étalement cascade → 1024 cases 32×32
    states = _cascade_snapshots(img32.flatten(), n_states=1024, N=32)        # (1024, 1024)
    cases  = states.reshape(1024, 32, 32)                                    # 1024 carrés 32×32
    # 3. IMG encode sur chaque carré → 1024 latents → packbits → 1024 hashes
    lats   = _enc(cases)                                                     # (1024, 8, 32, 32)
    hashes = np.packbits(lats.reshape(1024, -1), axis=1)                     # (1024, 1024) octets
    return hashes.tobytes()                                                  # 1 048 576 o = 1 Mo

if __name__ == "__main__":
    inp = sys.argv[1] if len(sys.argv) > 1 else "output32/redEC_out.bin"
    out = sys.argv[2] if len(sys.argv) > 2 else "output32/redDEC_out.bin"
    h = (ROOT/inp).read_bytes()[:1024]
    result = redDEC(h)
    (ROOT/out).write_bytes(result)
    print(f"redDEC (boîte noire) : 1 hash (1024 o) → {len(result)} o = {len(result)//1024} hashes")
