// rustyboi web — emulator Web Worker.
//
// Owns the whole emulator: the wasm core + session, IndexedDB storage, and
// rendering to a transferred OffscreenCanvas (so video never crosses a
// postMessage boundary). It self-paces at the GB frame rate with a
// performance.now() accumulator + setTimeout — workers have no
// requestAnimationFrame, and that is exactly the point: emulation cadence is
// decoupled from the display refresh, so a 175 Hz monitor can't cause jank.
//
// Audio is produced here each frame and posted (transferable Float32Array) to
// the main thread, which owns the AudioContext (WebAudio is main-thread only).
//
// Message protocol
//   main -> worker:
//     Init{canvas}            transferred OffscreenCanvas; boots the emulator
//     LoadRom{bytes}          transferred ArrayBuffer of ROM bytes
//     SetButton{code,pressed} KeyboardEvent.code + state
//     ClearInput              drop all held buttons
//     TogglePause
//     SetHardware{model}      "dmg" | "cgb"
//     SetPalette{shades}      16 bytes = 4 RGBA shades (lightest->darkest)
//     SaveSlot{n,timestamp}
//     LoadSlot{n}
//     Quicksave{timestamp}
//     Quickload
//     SetFastForward{on}
//   worker -> main:
//     Ready                   emulator constructed, loop running
//     Audio{samples}          transferred interleaved Float32Array [l,r,l,r,...]
//     Status{msg}
//     Saved{slot} / Loaded{slot,frames}
//     Slots{list}             slot numbers with saved state
//     Error{msg}

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
    const samples = emu.run_frame(); // draws to the OffscreenCanvas
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

async function handleInit(canvas) {
  await init();
  emu = await Emulator.create(canvas);
  post({ type: "Ready", hardware: emu.hardware() });
  status("Ready — load a ROM to start.");
  startLoop();
}

self.onmessage = async (e) => {
  const m = e.data;
  try {
    switch (m.type) {
      case "Init":
        await handleInit(m.canvas);
        return;
    }
    if (!emu) return; // ignore control messages until booted

    switch (m.type) {
      case "LoadRom":
        emu.load_rom(new Uint8Array(m.bytes));
        status(`Running: ${m.name || "ROM"}`);
        post({ type: "Slots", list: Array.from(emu.list_slots()) });
        break;
      case "SetButton":
        emu.set_button(m.code, m.pressed);
        break;
      case "ClearInput":
        emu.clear_input();
        break;
      case "TogglePause":
        emu.toggle_pause();
        break;
      case "SetHardware":
        emu.set_hardware(m.model);
        break;
      case "SetPalette":
        emu.set_palette(new Uint8Array(m.shades));
        break;
      case "SetFastForward":
        emu.set_fast_forward(!!m.on);
        break;
      case "SaveSlot":
        emu.save_slot(m.n, m.timestamp);
        post({ type: "Saved", slot: m.n });
        post({ type: "Slots", list: Array.from(emu.list_slots()) });
        break;
      case "LoadSlot": {
        const frames = emu.load_slot(m.n);
        post({ type: "Loaded", slot: m.n, frames });
        break;
      }
      case "Quicksave":
        emu.quicksave(m.timestamp);
        post({ type: "Saved", slot: "quick" });
        break;
      case "Quickload": {
        const frames = emu.quickload();
        post({ type: "Loaded", slot: "quick", frames });
        break;
      }
      default:
        break;
    }
  } catch (err) {
    fail(err);
  }
};
