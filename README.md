# Redstars Helper

Local agent that runs alongside the Redstars dashboard to expose USB scale
readings, the lsusb device list, and one-click WebGPU activation (Firefox
`user.js` writer).

The dashboard at `https://dev.redstars.redlinks.fr/dashboard` (or your
deployment) calls `/api/v1/agents/latest` to detect whether you've installed
this helper, and surfaces a download banner when it's missing or outdated.

## Install (Linux)

```bash
sudo apt install ./redstars-helper_0.1.0_amd64.deb
# or extract to ~/.local for non-system install
```

The app lives in your application menu as **Redstars Helper**, runs in the
system tray, and serves on `http://localhost:8080/`.

## Ouvrir RedStars en console

Le paquet installe deux commandes courtes sur le `PATH`
(`/usr/local/bin/redhelper` et `/usr/local/bin/minitel`) pour lancer le
client console dans le terminal courant, sans passer par le menu du tray :

```bash
redhelper console              # client console, taille réelle du terminal
redhelper console --app eau    # options passées telles quelles à helper.py
redhelper minitel              # raccourci : console 40×24, façon Minitel
minitel                        # idem, via le lien direct « minitel »
```

`redhelper help` affiche l'aide ; `redhelper console --help` liste toutes les
options du client (`--user`, `--app`, `--accessible`, …). Sans argument, le
binaire démarre l'agent dans la barre système comme avant.

## What it does

- Static HTTP server for the autoencoder demo page (`output/demo/`).
- `/helper/*` JSON API:
  - `GET /helper/status` → version + ok
  - `GET /helper/lsusb` → connected USB devices
  - `GET /helper/scale` → live USB scale reading (CH340/FTDI/CP210x auto-detect)
  - `POST /helper/enable-webgpu` → writes Firefox `user.js` flags
  - `POST /helper/reset-webgpu` → removes them
- Tray icon with Open / Restart / Quit.

## Build from source

```bash
npm install
npx tauri build
```

Output in `src-tauri/target/release/bundle/`.

## Auto-update

Pulls signed releases from this repo's GitHub Releases page. Configure
`tauri.conf.json` `updater.pubkey` with the matching public key — see
[Tauri updater docs](https://v2.tauri.app/plugin/updater/).
