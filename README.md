# tuityloops

A tiny terminal-based step sequencer (TUI) with realtime audio output.

- **UI:** `ratatui` + `crossterm`
- **Audio:** `cpal` (realtime output)
- **Offline render:** writes `out.wav` via `hound`

The sequencer is **16 bars** long (256 steps total), with a bar/beat-style grid inspired by classic step sequencers.

## Features

- Realtime playback with a playhead
- Step grid editing with keyboard navigation
- 6 lanes/instruments:
  - Kick
  - Snare
  - Hat
  - Clap
  - Tom
  - Rim
- 16-bar pattern with a horizontal “paged” view (shows as many full bars as fit your terminal width)
- Offline rendering to a WAV file

## Requirements

- Rust toolchain (stable)
- A working audio output device

macOS note: `cpal` will use your default output device.

## Build

```bash
cargo build
```

## Run

```bash
cargo run
```

## Controls

- **Arrows**: move cursor (lane/step)
- **Space**: toggle step on/off
- **p**: play/stop
- **+ / -**: BPM up/down
- **] / [**: master gain up/down
- **r**: render `out.wav`
- **q** (or **Ctrl+C**): quit

## UI notes

- The Steps panel is grouped by beats and bars:
  - `│` separates beats (every 4 steps)
  - `║` separates bars (every 16 steps)
- The title shows the visible bar range: `Steps (Bars X–Y / 16)`

## Offline render

Press **r** to render the full 16-bar pattern to `out.wav`.

The render length is derived from the current BPM:

- steps per beat: 4
- steps per bar: 16
- total steps: 256

## Screen Shots

...

## Troubleshooting

### "found possibly newer version of crate `core` / `std`" (E0460)

This usually happens after upgrading Rust or switching toolchains.

```bash
cargo clean
cargo build
```

### No sound

- Make sure your system output device is set and unmuted.
- Try closing other apps that might be holding exclusive access to the audio device.

## Project layout

This project is intentionally small and currently lives in a single file:

- `src/main.rs` — TUI + audio engine + instruments + WAV renderer

## License

Unlicensed / TBD.
