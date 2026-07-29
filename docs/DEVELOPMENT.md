# Development — Drumentor

Guide for building and running Drumentor from source. Architecture: [ARCHITECTURE.md](ARCHITECTURE.md).

## Tech stack

Chosen for **low MIDI latency** and offline-first desktop use. MIDI, clock, matching, and audio run in Rust; the WebView is UI only (no Web Audio for playback).

| Layer | Technology | Role |
|------|------------|------|
| Shell | [Tauri 2](https://tauri.app/) | Window, FS, IPC |
| UI | React 19 + TypeScript + Vite | Kit, highway, transport |
| Styles | CSS Modules + CSS variables | Local styles, design tokens |
| UI state | Zustand | Transport, highlights, session UI |
| MIDI I/O | Rust `midir` | E-drum / controller input |
| MIDI parse | Rust `midly` | SMF, tempo map, tracks |
| Audio | Rust `oxisynth` + `cpal` | GM SoundFont → WASAPI (default) / ASIO (opt-in) |
| Persistence | SQLite (`rusqlite`) | Pad maps, sessions, settings |
| IPC | Tauri commands + events | UI ↔ Rust core |

**Platforms:** Windows 10/11 (primary). macOS / Linux are stack-supported later (CoreMIDI / ALSA).

**Avoid in MVP:** heavy UI kits (MUI, Ant Design), Electron, cloud backend, CDN SoundFonts as a hard dependency.

## Requirements

- [Node.js](https://nodejs.org/) 18+ (LTS) and npm
- Stable [Rust](https://rustup.rs/) (edition 2021+)
- [Tauri CLI](https://tauri.app/) 2.x (`npm` scripts use `@tauri-apps/cli`)
- Windows 10/11 and [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/) (usually preinstalled)
- MSVC Build Tools
- GM SoundFont `FluidR3.sf3` (~19 MB) — see below

### SoundFont

This file is required for playback and is not included in the repository (~19 MB).

1. Download [SF3.tar.bz2](https://github.com/Jacalz/fluid-soundfont/releases/download/v1.0/SF3.tar.bz2) (MIT, Frank Wen / [Jacalz/fluid-soundfont](https://github.com/Jacalz/fluid-soundfont)).
2. Extract the archive and place the `.sf3` file at `public/soundfonts/FluidR3.sf3`.

License notes: `public/soundfonts/NOTICE.txt` and `public/soundfonts/COPYING`.

### ASIO (Optional)

Audio uses **WASAPI** by default. To use ASIO:

1. Download the [ASIO SDK](https://www.steinberg.net/developers/) and set `CPAL_ASIO_DIR` to the SDK directory.
2. Build with the feature enabled: run `cargo build --features asio` from `src-tauri`, or pass `--features asio` to the Tauri build command.
3. In the UI, select a device with the `[asio]` host (use ASIO4ALL to test without hardware).

## Quick Start

```bash
npm install
# Place FluidR3.sf3 in public/soundfonts/ (see above)
npm run tauri dev
```

To run only the frontend in a browser (without native MIDI, audio, or dialogs):

```bash
npm run dev
```

