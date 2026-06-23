// rustyboi web — emulator Web Worker.
//
// Owns the whole emulator: the wasm core + session and IndexedDB storage. It
// self-paces with setTimeout ticks — workers have no requestAnimationFrame,
// and that is exactly the point: emulation cadence is decoupled from the
// display refresh, so a 175 Hz monitor can't cause jank. Frames-per-tick come
// from the shared pacing regulator inside the wasm module (the same
// wall-clock + DAC-trim token bucket every native platform runs).
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
//     ImportFile{purpose,bytes}  import a picked file (purpose=state|battery|rtc|patch)
//     RequestExport{kind}  ask for export bytes (kind=state|battery|rtc)
//     SetInput{mask}       GB button bitmask (keyboard ∪ egui touch overlay)
//     SetDebugDetail{active,bits}  which debug snapshot to build (open panels)
//     Action{json}         a UiAction (JSON) applied via Session::apply
//   worker -> main:
//     Ready{hardware}      emulator constructed, loop running
//     Frame{rgba,width,height}  transferred RGBA ArrayBuffer + pixel size
//     Audio{samples}       transferred interleaved Float32Array [l,r,l,r,...]
//     Export{name,bytes}   transferred export bytes for the main thread to download
//     UiState{json}        SessionUiState snapshot (posted on change)
//     DebugSnapshot{bytes} transferred bincode debug read-model (only while a
//                          debug panel is open)
//     Status{msg} / Error{msg} / ClearError / ResizeContent{width,height}
//                          PlatformRequest objects an `apply`-backed call returned

import init, { Emulator } from "./pkg/rustyboi_web.js";

// DMG/CGB LCD refresh is ~59.7275 Hz (tick cadence only — the regulator owns
// the frame budget and banks time across late ticks).
const GB_FPS = 59.7275;
const FRAME_MS = 1000 / GB_FPS;

let emu = null;
let running = false;
// Hold-to-rewind: while set (Backspace held on the main thread) the loop steps
// back through the rewind buffer instead of running frames forward.
let rewinding = false;
// Debug detail requested before the emulator finished booting (applied in
// handleInit). The main thread posts SetDebugDetail only on change.
let pendingDebug = null;

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

// Recycled frame ArrayBuffers, transferred back by the main thread after each
// upload. Reusing them means zero per-frame framebuffer allocation (no GC churn
// on either thread). Bounded so a stall can't grow it without limit.
const framePool = [];
const FRAME_POOL_MAX = 4;

// Post the just-rendered frame's RGBA (transferring its buffer — no copy) and,
// if the session UI-state changed, the fresh snapshot for the egui UI.
function postFrameAndState() {
  const w = emu.frame_width();
  const h = emu.frame_height();
  const need = w * h * 4;
  // Reuse a returned buffer of the right size (GB 160x144 vs SGB 256x224 differ).
  let buf = framePool.pop();
  if (!buf || buf.byteLength !== need) buf = new ArrayBuffer(need);
  const rgba = new Uint8Array(buf);
  emu.frame_into(rgba); // wasm copies this frame's RGBA into the pooled buffer
  post({ type: "Frame", rgba, width: w, height: h }, [buf]);
  const uiState = emu.take_ui_state(); // JSON string, or undefined when unchanged
  if (uiState) post({ type: "UiState", json: uiState });
  // Debug read-model: empty (length 0) unless a main-thread panel is open, so
  // nothing crosses the boundary in the common case. Transfer the buffer (no
  // copy) when present.
  const dbg = emu.take_debug_snapshot(); // Uint8Array (empty when no panel open)
  if (dbg && dbg.length > 0) {
    post({ type: "DebugSnapshot", bytes: dbg }, [dbg.buffer]);
  }
}

// Self-paced loop. The SHARED pacing regulator (rustyboi-session's wall-clock
// token bucket with bounded DAC trim — the same one every native platform
// runs) decides how many frames each tick emulates from performance.now() and
// the audio backlog the main thread posts back on ReturnBuffer. setTimeout(0)
// yields between ticks so the worker event loop stays responsive to control
// messages; timer coarseness/jitter cannot affect game speed (the bucket banks
// elapsed time).
function loop() {
  if (!running) return;

  // Uncapped fast-forward: run emulation as fast as the host allows — advance a
  // batch (each `run_frame` runs several GB frames in-core) unregulated, then
  // re-schedule immediately. Audio is muted in-core while uncapped, so there's
  // nothing to post. Rewind always takes the paced path.
  const uncapped = !rewinding && emu.uncapped_fast_forward && emu.uncapped_fast_forward();

  let ran = 0;
  try {
    if (uncapped) {
      emu.frames_to_run(performance.now()); // keep the bucket clock current
      emu.run_frame();
      if (emu.has_rom()) postFrameAndState();
      ran = 1;
    } else {
      const grant = emu.frames_to_run(performance.now());
      const runs = grant > 0 ? grant : emu.is_paused() ? 1 : 0;
      while (ran < runs) {
        if (rewinding) {
          // Step back one snapshot; if the buffer is exhausted, hold on the oldest
          // frame (do NOT resume forward play while Backspace is still held). No
          // audio while rewinding.
          if (emu.rewind_step()) postFrameAndState();
        } else {
          const samples = emu.run_frame(); // fills the RGBA framebuffer
          if (emu.has_rom()) postFrameAndState();
          if (samples.length > 0) {
            // Transfer the underlying buffer — no copy across the boundary.
            post({ type: "Audio", samples }, [samples.buffer]);
          }
        }
        ran++;
      }
    }
    // Hand any completed Game Boy Printer sheets (PNG bytes) to the main thread
    // as downloads. Prints are rare, so this is an empty array almost every tick.
    if (ran > 0) drainPrints();
  } catch (err) {
    running = false;
    fail(err);
    return;
  }

  const delay = uncapped ? 0 : Math.min(FRAME_MS, emu.ms_until_next_frame());
  setTimeout(loop, delay);
}

// Running count of prints this session, for stable download filenames.
let printCount = 0;

// Post each finished printer sheet (PNG bytes) as an Export the main thread
// downloads, reusing the same message the state/battery/RTC exports use.
function drainPrints() {
  const prints = emu.take_prints();
  for (let i = 0; i < prints.length; i++) {
    const bytes = prints[i];
    post({ type: "Export", name: `gb-print-${++printCount}.png`, bytes }, [bytes.buffer]);
  }
}

function startLoop() {
  if (running) return;
  running = true;
  loop();
}

async function handleInit() {
  await init();
  emu = await Emulator.create();
  if (pendingDebug) {
    emu.set_debug_detail(pendingDebug.active, pendingDebug.bits);
    pendingDebug = null;
  }
  post({ type: "Ready", hardware: emu.hardware() });
  // Push the initial UI-state so the egui menus reflect persisted config.
  const uiState = emu.take_ui_state();
  if (uiState) post({ type: "UiState", json: uiState });
  status("Ready — load a ROM to start.");
  // Kick off the No-Intro game-name index download (main thread owns fetch()).
  // The bodies come back via a FinishNoIntro message.
  const niUrls = emu.no_intro_fetch_urls();
  if (niUrls && niUrls.length) {
    post({ type: "FetchUrl", purpose: "no_intro", urls: Array.from(niUrls) });
  }
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
    if (m.type === "ReturnBuffer") {
      // Main thread finished uploading a frame and handed its buffer back.
      if (m.buf && framePool.length < FRAME_POOL_MAX) framePool.push(m.buf);
      // The audio backlog rides along: feed the pacing regulator's DAC trim
      // (-1 while the AudioContext isn't running yet).
      if (emu && typeof m.backlogPairs === "number") emu.set_audio_backlog_pairs(m.backlogPairs);
      return;
    }
    if (m.type === "SetRewind") {
      rewinding = !!m.on; // hold-to-rewind (Backspace) toggled on the main thread
      return;
    }
    if (m.type === "SetDebugDetail") {
      // Which debug snapshot to build each frame (which panels are open). If a
      // panel is opened before the emulator has booted, stash it so `handleInit`
      // can apply it (the main thread only posts this on change).
      if (emu) emu.set_debug_detail(!!m.active, m.bits & 0xff);
      else pendingDebug = { active: !!m.active, bits: m.bits & 0xff };
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
      case "ImportFile": {
        // m.purpose ∈ state|battery|rtc|patch|movie; m.bytes is a transferred ArrayBuffer.
        const data = new Uint8Array(m.bytes);
        if (m.purpose === "state") emit(emu.load_state(data));
        else if (m.purpose === "battery") emit(emu.import_battery(data));
        else if (m.purpose === "rtc") emit(emu.import_rtc(data));
        else if (m.purpose === "patch") emit(emu.apply_patch(data));
        else if (m.purpose === "movie") emit(emu.load_movie(data));
        break;
      }
      case "RequestExport": {
        // Produce the bytes on the worker (it owns the session) and post them to
        // the main thread, which triggers the browser download.
        let bytes, name;
        if (m.kind === "state") { bytes = emu.export_state(); name = "savestate.rustyboisave"; }
        else if (m.kind === "battery") { bytes = emu.export_battery(); name = "battery.sav"; }
        else if (m.kind === "rtc") { bytes = emu.export_rtc(); name = "clock.rtc"; }
        else break;
        if (bytes && bytes.length > 0) {
          post({ type: "Export", name, bytes }, [bytes.buffer]);
        } else {
          fail(`Nothing to export (${m.kind})`);
        }
        break;
      }
      case "SetInput":
        emu.set_input_mask(m.mask & 0xff);
        break;
      case "Action": {
        emit(emu.apply_action(m.json));
        // Push fresh UI state immediately: an action can change it (e.g. the
        // touch-controls toggle) while idle, when the frame loop — the only
        // other place state is posted — is not running because no ROM is loaded.
        const uiState = emu.take_ui_state();
        if (uiState) post({ type: "UiState", json: uiState });
        if (!running && emu.has_rom()) startLoop();
        break;
      }
      case "FinishCheats": {
        // The main thread fetched the libretro `.cht` body; parse it into the
        // session's fetched-cheat list and push the fresh UI state.
        emit(emu.finish_fetched_cheats(m.body || ""));
        const uiState = emu.take_ui_state();
        if (uiState) post({ type: "UiState", json: uiState });
        break;
      }
      case "FinishNoIntro": {
        // The main thread fetched the No-Intro DAT bodies; load them into the
        // runtime game-name index (re-resolves the current ROM's display name).
        emu.finish_no_intro_dats(m.bodies || []);
        const uiState = emu.take_ui_state();
        if (uiState) post({ type: "UiState", json: uiState });
        break;
      }
      default:
        break;
    }
  } catch (err) {
    fail(err);
  }
};
