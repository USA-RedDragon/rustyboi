# rustyboi-web

A WASM frontend for the rustyboi Game Boy / Game Boy Color emulator, built on
the shared [`rustyboi-session`](../rustyboi-session) crate and targeting
**Firefox** (works in any modern browser).

## Why these choices (Firefox-safe by design)

| Concern    | Choice                                        | Why not the desktop path |
|------------|-----------------------------------------------|--------------------------|
| Rendering  | 2D canvas `putImageData` (`web-sys ImageData`)| `pixels`/`wgpu`/WebGPU is **not stable in Firefox**. At 160×144 a per-frame `ImageData` blit is trivially fast; CSS `image-rendering: pixelated` upscales crisply. |
| Audio      | WebAudio queued `AudioBufferSourceNode` ring  | Core emits stereo f32 @ 44100 Hz; buffers declare that rate and the graph resamples. AudioWorklet is the natural future upgrade. |
| Input      | DOM `keydown`/`keyup` → `AbstractInput`       | Host→abstract classification is the adapter's job; the session applies its own remap. |
| ROM load   | `<input type=file>` → `ArrayBuffer`           | The **File System Access API is Chrome-only** — not in Firefox. |
| Storage    | IndexedDB (write-through in-memory cache)     | Same reason: no FS Access API. IndexedDB is the durable web store. |

The `rustyboi-session` `Storage` trait is synchronous but IndexedDB is async;
`storage.rs` bridges them with an in-memory cache hydrated from IndexedDB at
startup and mirrored back on every write. The session stays WASM-clean.

## Build

Prerequisites (already present in this repo's dev env):

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack          # or wasm-bindgen-cli
```

Build the wasm module + JS bindings into `www/pkg/`:

```bash
# from the workspace root
wasm-pack build rustyboi-web --target web --out-dir www/pkg --release
# (use --dev instead of --release for a faster, unoptimized build)
```

The raw cargo gate (no bindings) also works:

```bash
cargo build -p rustyboi-web --target wasm32-unknown-unknown
```

## Serve + open in Firefox

WASM modules must be served over HTTP (not `file://`) with the correct MIME
type. Any static server works:

```bash
cd rustyboi-web/www
python3 -m http.server 8080
# then open http://localhost:8080/ in Firefox
```

Load a `.gb`/`.gbc` ROM with the **Load ROM…** button and play.

## Controls

- Arrows — D-pad
- `X` — A, `Z` — B
- `Enter` — Start, `Shift` — Select
- `P` — pause/resume
- `Tab` (hold) — fast-forward

Save/load use slot 0 (persisted to IndexedDB, keyed by ROM hash).

## Notes

- `www/pkg/` is generated and gitignored — run the build step to (re)create it.
- Audio is unlocked on the ROM-load click (browsers require a user gesture to
  start an `AudioContext`).
