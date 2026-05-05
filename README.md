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
