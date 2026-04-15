// rustyboi web — emulator Web Worker.
//
// Owns the whole emulator: the wasm core + session and IndexedDB storage. It
// self-paces at the GB frame rate with a performance.now() accumulator +
// setTimeout — workers have no requestAnimationFrame, and that is exactly the
// point: emulation cadence is decoupled from the display refresh, so a 175 Hz
// monitor can't cause jank.
//
// Rendering happens on the MAIN thread (egui + wgpu WebGL2). Each frame the
// worker posts the RGBA framebuffer (transferable ArrayBuffer, zero-copy), the
// interleaved audio (transferable Float32Array), and — when it changes — a
// SessionUiState snapshot (JSON) for the egui UI to render from.
//
// Message protocol
//   main -> worker:
//     Init                 boot the emulator (no canvas — main thread renders)
//     LoadRom{name,bytes}  transferred ArrayBuffer of ROM bytes
//     LoadState{bytes}     transferred ArrayBuffer of a .rustyboisave savestate
//     SetInput{mask}       GB button bitmask (keyboard ∪ egui touch overlay)
//     Action{json}         a WebAction (JSON) applied via Session::apply
//   worker -> main:
//     Ready{hardware}      emulator constructed, loop running
//     Frame{rgba,width,height}  transferred RGBA ArrayBuffer + pixel size
//     Audio{samples}       transferred interleaved Float32Array [l,r,l,r,...]
//     UiState{json}        SessionUiState snapshot (posted on change)
//     Status{msg} / Error{msg} / ClearError / ResizeContent{width,height}
//                          PlatformRequest objects an `apply`-backed call returned

import init, { Emulator } from "./pkg/rustyboi_web.js";

// DMG/CGB LCD refresh is ~59.7275 Hz.
const GB_FPS = 59.7275;
const FRAME_MS = 1000 / GB_FPS;
// Cap catch-up so a long stall (e.g. a save) doesn't run a huge burst.
const MAX_FRAMES_PER_TICK = 4;

let emu = null;
let running = false;
let lastNow = 0;
let acc = 0;

const post = (msg, transfer) => self.postMessage(msg, transfer || []);
const status = (msg) => post({ type: "Status", msg });
const fail = (msg) => post({ type: "Error", msg: String(msg) });

// Forward the PlatformRequest objects an `Emulator.apply`-backed method returns
// (a JS Array of {type, ...}) to the main thread as-is; each is already a valid
// worker->main message.
const emit = (reqs) => {
  if (!reqs) return;
  for (const r of reqs) post(r);
};

// Post the just-rendered frame's RGBA (transferring its buffer — no copy) and,
// if the session UI-state changed, the fresh snapshot for the egui UI.
function postFrameAndState() {
  const rgba = emu.frame(); // Uint8Array copy of this frame's RGBA
  post(
    { type: "Frame", rgba, width: emu.frame_width(), height: emu.frame_height() },
    [rgba.buffer],
  );
  const uiState = emu.take_ui_state(); // JSON string, or undefined when unchanged
  if (uiState) post({ type: "UiState", json: uiState });
}

// Self-paced fixed-timestep loop. We accumulate real elapsed time and run whole
// GB frames while the accumulator holds at least one frame's worth, capped per
// tick. setTimeout(0) yields between ticks so the worker event loop stays
// responsive to control messages; the accumulator absorbs timer coarseness so
// the *average* rate stays locked to GB_FPS regardless of timer jitter.
function loop() {
  if (!running) return;
  const now = performance.now();
  acc += now - lastNow;
  lastNow = now;

  let ran = 0;
  while (acc >= FRAME_MS && ran < MAX_FRAMES_PER_TICK) {
    const samples = emu.run_frame(); // fills the RGBA framebuffer
    if (emu.has_rom()) postFrameAndState();
    if (samples.length > 0) {
      // Transfer the underlying buffer — no copy across the boundary.
      post({ type: "Audio", samples }, [samples.buffer]);
    }
    acc -= FRAME_MS;
    ran++;
  }
  // Shed a large backlog (backgrounded tab / long GC) instead of sprinting.
  if (acc > FRAME_MS * MAX_FRAMES_PER_TICK) acc = 0;

  // Sleep until roughly the next frame boundary; clamp to >= 0.
  const delay = Math.max(0, FRAME_MS - acc);
  setTimeout(loop, delay);
}

function startLoop() {
  if (running) return;
  running = true;
  lastNow = performance.now();
  acc = 0;
  loop();
}

async function handleInit() {
  await init();
  emu = await Emulator.create();
  post({ type: "Ready", hardware: emu.hardware() });
  // Push the initial UI-state so the egui menus reflect persisted config.
  const uiState = emu.take_ui_state();
  if (uiState) post({ type: "UiState", json: uiState });
  status("Ready — load a ROM to start.");
  startLoop();
}

self.onmessage = async (e) => {
  const m = e.data;
  try {
    switch (m.type) {
      case "Init":
        await handleInit();
        return;
    }
    if (!emu) return; // ignore control messages until booted

    switch (m.type) {
      case "LoadRom":
        emit(emu.load_rom(m.name || "ROM", new Uint8Array(m.bytes)));
        if (emu.has_rom()) status(`Running: ${m.name || "ROM"}`);
        break;
      case "LoadState":
        emit(emu.load_state(new Uint8Array(m.bytes)));
        break;
      case "SetInput":
        emu.set_input_mask(m.mask & 0xff);
        break;
      case "Action":
        emit(emu.apply_action(m.json));
        break;
      default:
        break;
    }
  } catch (err) {
    fail(err);
  }
};
