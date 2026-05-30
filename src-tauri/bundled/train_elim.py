#!/usr/bin/env python3
"""Ensemble 32 modeles, elimination par tournoi -> 9 survivants.

Tous les ELIM_EVERY steps on supprime le modele dont l'accuracy moyenne sur les
ROLLING derniers steps est la plus basse, jusqu'a N_FINAL. Le rendu d'eval est
le vote majoritaire (mode pixel par pixel) sur les survivants.

Total = (N_INIT - N_FINAL) * ELIM_EVERY steps.
Dashboard : output32/live.{png,json}.
"""
import json, signal, collections
from pathlib import Path
import numpy as np
import torch, torch.nn as nn, torch.nn.functional as F
from PIL import Image

ROOT = Path(__file__).parent
OUT  = ROOT / "output32"; OUT.mkdir(exist_ok=True)
DEV  = "cuda" if torch.cuda.is_available() else "cpu"

# ===== config =====
GRID         = 32
N_INIT       = 32
N_FINAL      = 9
ELIM_EVERY   = 5000
ROLLING      = 500          # fenetre (steps) pour la moyenne d'accuracy par modele
BATCH        = 1
CH           = 64
LIVE_EVERY   = 25           # refresh dashboard
SAVE_EVERY   = 1000

_palette_paths = [ROOT / "output/demo/demo_data.json", ROOT / "demo_data.json"]
_palette_file  = next((p for p in _palette_paths if p.exists()), None)
if _palette_file is None:
    raise FileNotFoundError(f"demo_data.json introuvable ({[str(p) for p in _palette_paths]})")
_hex  = json.load(open(_palette_file))["palette"]
PAL   = np.array([[int(h[k:k+2], 16) for k in (1, 3, 5)] for h in _hex], np.uint8)
_BITS = torch.arange(8, device=DEV).view(1, 8, 1, 1)


def to_bits(idx):
    return ((idx.unsqueeze(1) >> _BITS) & 1).float()


class STEbin(torch.autograd.Function):
    @staticmethod
    def forward(ctx, x): return torch.round(x.clamp(0, 1))
    @staticmethod
    def backward(ctx, g): return g


class Ensemble(nn.Module):
    """N AE en parallele via groups=n."""
    def __init__(self, n, ch):
        super().__init__()
        self.n = n
        self.e1 = nn.Conv2d(8*n,  ch*n,  3, padding=1, groups=n)
        self.e2 = nn.Conv2d(ch*n, 8*n,   1,            groups=n)
        self.d1 = nn.Conv2d(8*n,  ch*n,  3, padding=1, groups=n)
        self.d2 = nn.Conv2d(ch*n, 256*n, 1,            groups=n)

    def forward(self, bits):
        B = bits.shape[0]
        x = bits.repeat(1, self.n, 1, 1)
        z = STEbin.apply(torch.sigmoid(self.e2(F.gelu(self.e1(x)))))
        out = self.d2(F.gelu(self.d1(z)))
        return out.view(B, self.n, 256, GRID, GRID)


def _up(a, s=8):
    return np.array(Image.fromarray(a, "RGB").resize(
        (a.shape[1]*s, a.shape[0]*s), Image.NEAREST))


def drop_model(model, victim):
    """Renvoie un nouvel Ensemble(n-1, CH) avec les memes poids sauf le modele
    `victim` (index dans [0, n)). Conv2d groups=n : slice per_out le long dim 0
    pour weight ET bias."""
    keep = [i for i in range(model.n) if i != victim]
    new  = Ensemble(model.n - 1, CH).to(DEV)
    src_layers = dict(model.named_modules())
    for name, dst_layer in new.named_modules():
        if not isinstance(dst_layer, nn.Conv2d):
            continue
        src_layer = src_layers[name]
        per_out   = src_layer.out_channels // model.n
        for tag in ("weight", "bias"):
            src = getattr(src_layer, tag)
            if src is None:
                continue
            dst = getattr(dst_layer, tag)
            kept = torch.cat([src[i*per_out:(i+1)*per_out] for i in keep], dim=0)
            dst.data.copy_(kept)
    return new


def write_dashboard(label, probe, po, hist, caption, alive_idx):
    """live.png + live.json. probe=(32,32), po=(N_current,32,32)."""
    n = po.shape[0]
    indiv     = float((po == probe).float().mean())
    consensus = torch.mode(po, dim=0).values
    cacc      = float((consensus == probe).float().mean())

    inp_img  = PAL[probe.cpu().numpy()]
    cons_img = PAL[consensus.cpu().numpy()]
    # canvas fixe : 8 colonnes x 8 lignes = 64 cellules max (largement assez)
    cols, rows_g = 8, 8
    H, W_left    = rows_g * GRID, 8 * GRID            # 256 x 256
    grid = np.full((H, cols*GRID, 3), 255, np.uint8)
    psel = po.cpu().numpy()
    for k in range(n):
        r, c = divmod(k, cols)
        if r >= rows_g: break
        grid[r*GRID:(r+1)*GRID, c*GRID:(c+1)*GRID] = PAL[psel[k]]

    left = np.full((H, W_left, 3), 255, np.uint8)
    S    = 3                                          # input/consensus a 96x96
    x0   = (W_left - GRID*S) // 2
    y_top = (H//2 - GRID*S) // 2
    y_bot = H//2 + y_top
    left[y_top:y_top+GRID*S, x0:x0+GRID*S] = _up(inp_img,  S)
    left[y_bot:y_bot+GRID*S, x0:x0+GRID*S] = _up(cons_img, S)

    gap = np.full((H, 16, 3), 255, np.uint8)
    Image.fromarray(np.concatenate([left, gap, grid], 1), "RGB").save(OUT/"live.png")
    hist.append({"step": label, "acc": round(cacc, 4), "indiv": round(indiv, 4), "n": n})
    json.dump({"step": label, "n": n, "alive": alive_idx,
               "history": hist[-600:], "cols": caption},
              open(OUT/"live.json", "w"))
    return cacc, indiv


def main():
    torch.manual_seed(0)
    model = Ensemble(N_INIT, CH).to(DEV)
    opt   = torch.optim.Adam(model.parameters(), lr=3e-3)

    print(f"ELIM : {GRID}x{GRID} — N {N_INIT}->{N_FINAL}, CH={CH}, "
          f"{sum(p.numel() for p in model.parameters())/1e6:.1f}M params — {DEV}", flush=True)
    print(f"elim toutes les {ELIM_EVERY} steps, rolling window {ROLLING}, "
          f"total = {(N_INIT-N_FINAL)*ELIM_EVERY} steps", flush=True)

    # buffer rolling per-model : deque de (N,) tensors d'accuracy
    acc_buf  = collections.deque(maxlen=ROLLING)
    alive    = list(range(N_INIT))             # ids d'origine des survivants
    hist     = []
    stop     = {"v": False}
    for s in (signal.SIGINT, signal.SIGTERM):
        signal.signal(s, lambda *_: stop.__setitem__("v", True))

    torch.manual_seed(7)
    step  = 0
    total = (N_INIT - N_FINAL) * ELIM_EVERY
    ckpt  = lambda: OUT / f"elim_n{model.n}_latest.pth"

    while not stop["v"] and step < total:
        idx = torch.randint(0, 256, (BATCH, GRID, GRID), device=DEV)
        out = model(to_bits(idx))                                # (B,N,256,32,32)
        tfl = idx.unsqueeze(1).expand(BATCH, model.n, GRID, GRID).reshape(BATCH*model.n, GRID, GRID)
        loss = F.cross_entropy(out.reshape(BATCH*model.n, 256, GRID, GRID), tfl)
        opt.zero_grad(); loss.backward(); opt.step()
        step += 1

        # per-model accuracy sur ce step (B=1 -> 1 image)
        with torch.no_grad():
            po = out[0].argmax(1)                                # (N,32,32)
            per_model = (po == idx[0]).float().mean(dim=(-1, -2))  # (N,)
        acc_buf.append(per_model.detach())

        if step % LIVE_EVERY == 0:
            cap = (f"N={model.n} survivants — input (haut) / vote majoritaire (bas)  "
                   f"|  {model.n} reconstructions individuelles")
            cacc, indiv = write_dashboard(step, idx[0], po, hist, cap, alive)
            print(f"step {step:6d} | N={model.n:2d} | vote {cacc*100:6.2f}%  "
                  f"(individuel moyen {indiv*100:5.2f}%)", flush=True)

        if step % SAVE_EVERY == 0:
            torch.save({"model": model.state_dict(), "alive": alive}, ckpt())

        # --- elimination ---
        if step % ELIM_EVERY == 0 and model.n > N_FINAL:
            window = torch.stack(list(acc_buf), dim=0).mean(dim=0)   # (N,) moyenne rolling
            victim = int(window.argmin().item())
            elim_id = alive[victim]
            print(f"\n=== step {step}: elim modele local #{victim} (id origine {elim_id}, "
                  f"acc rolling {window[victim].item()*100:.2f}%) — N {model.n}->{model.n-1} ===",
                  flush=True)
            torch.save({"model": model.state_dict(), "alive": alive},
                       OUT / f"elim_before_drop_n{model.n}.pth")
            model = drop_model(model, victim)
            opt   = torch.optim.Adam(model.parameters(), lr=3e-3)
            alive.pop(victim)
            acc_buf.clear()                                          # rolling reset
            print(f"survivants ids : {alive}\n", flush=True)

    torch.save({"model": model.state_dict(), "alive": alive}, ckpt())
    print(f"\narrete a step {step} — N final = {model.n} — survivants {alive}", flush=True)
    print(f"checkpoint -> {ckpt()}", flush=True)


if __name__ == "__main__":
    main()
