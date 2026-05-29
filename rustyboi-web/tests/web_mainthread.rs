//! Headless-browser coverage for the MAIN-THREAD `WebApp` (egui + winit + wgpu),
//! as opposed to the worker `Emulator` (see `web_rewind.rs`). This is the path
//! where the reported `recursive use of an object detected which would lead to
//! unsafe aliasing in Rust` error lives — a wasm-bindgen `RefCell` reentrancy
//! surfaced by driving the app's event loop.
//!
//! The harness boots a real `WebApp` on a real `<canvas>`, pumps worker-style
//! frames, and dispatches the fast-forward (Tab) and frame-advance (Backslash)
//! hotkeys at winit, then lets the render loop run. A thrown reentrancy error
//! aborts the wasm module and fails the test.
//!
//! Run: `wasm-pack test --headless --chrome rustyboi-web --test web_mainthread`
//! (or `--firefox`). If the headless browser has no WebGL2, the renderer never
//! initializes and the app idles without crashing — the harness still verifies
//! boot + input plumbing don't panic, and is the vehicle for the reentrancy
//! repro once WebGL is available.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;

use rustyboi_web::WebApp;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

/// A do-nothing JS callback (the worker/DOM bridges the real shell supplies).
fn noop() -> js_sys::Function {
    js_sys::Function::new_no_args("")
}

/// A JS callback that bumps a shared counter each time it's invoked, so the test
/// can prove the `post_action` bridge actually fired (i.e. a hotkey reached
/// `dispatch_action`). The backing `Closure` is leaked to live for the run.
fn recording_fn(counter: Rc<Cell<u32>>) -> js_sys::Function {
    let cb = Closure::<dyn FnMut(wasm_bindgen::JsValue)>::new(move |_arg| {
        counter.set(counter.get() + 1);
    });
    cb.into_js_value().unchecked_into()
}

/// Await roughly `ms` of real time so the winit rAF loop gets to run.
async fn sleep(ms: i32) {
    let win = web_sys::window().unwrap();
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        win.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    let _ = JsFuture::from(promise).await;
}

/// Create + attach a real canvas for winit to adopt.
fn make_canvas() -> web_sys::HtmlCanvasElement {
    let doc = web_sys::window().unwrap().document().unwrap();
    let canvas: web_sys::HtmlCanvasElement = doc
        .create_element("canvas")
        .unwrap()
        .dyn_into()
        .unwrap();
    canvas.set_width(160);
    canvas.set_height(144);
    doc.body().unwrap().append_child(&canvas).unwrap();
    canvas
}

/// Dispatch a keyboard event (by `code`) at both the canvas and the window, so
/// it reaches winit's listeners wherever they're attached.
fn dispatch_key(canvas: &web_sys::HtmlCanvasElement, ty: &str, code: &str) {
    let init = js_sys::Object::new();
    js_sys::Reflect::set(&init, &"code".into(), &code.into()).unwrap();
    js_sys::Reflect::set(&init, &"key".into(), &code.into()).unwrap();
    js_sys::Reflect::set(&init, &"bubbles".into(), &true.into()).unwrap();
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict(
        ty,
        &init.unchecked_into(),
    )
    .unwrap();
    let _ = canvas.dispatch_event(&ev);
    if let Some(win) = web_sys::window() {
        let _ = win.dispatch_event(&ev);
    }
}

// Boot the main-thread app ONCE (winit's `EventLoop` is a process singleton —
// only one test per wasm module may `start()`), then hammer fast-forward (Tab)
// and frame-advance (Backslash) through the real winit event path while
// worker-style callbacks (frames, status, error, clear) arrive — the reported
// reentrancy scenario. Passing = no `recursive use of an object` (or any) wasm
// trap from a borrow held across a re-entrant JS→Rust call.
//
// Crucially, the test PROVES it exercised the real path rather than idling: the
// `post_action` recorder must fire. `post_action` is only ever reached from
// `dispatch_action` <- `dispatch_hotkeys` <- INSIDE `draw()`, and `draw()` only
// runs once wgpu initialized and the renderer is live. So `posted > 0` is itself
// proof that a full `draw()` executed the input+dispatch path — a fast-forward /
// frame-advance hotkey actually drove it. If it stays zero (wgpu failed headless,
// or the synthetic keys never reached winit) the harness fails loudly rather than
// passing vacuously.
#[wasm_bindgen_test]
async fn mainthread_drive_no_reentrancy() {
    let posted = Rc::new(Cell::new(0u32));
    let canvas = make_canvas();
    let mut app = WebApp::new(
        recording_fn(posted.clone()), // post_action
        noop(),                       // post_input
        noop(), noop(), noop(), noop(), noop(), noop(), noop(),
    );
    app.start(canvas.clone()).await.expect("WebApp::start");
    canvas.focus().ok(); // winit only routes key events to the focused canvas

    // Let winit create the window and init wgpu.
    sleep(400).await;

    let rgba = vec![0u8; 160 * 144 * 4];
    for i in 0..40 {
        // Worker callbacks arrive (main-thread wasm-bindgen entry points)...
        app.on_frame(&rgba, 160, 144);
        app.on_status(format!("status {i}"));
        if i % 5 == 0 {
            app.on_error(format!("error {i}"));
            app.clear_error();
        }
        // Press fast-forward (Tab) + frame-advance (\) and hold across a draw so
        // the rAF `draw()` samples them held — otherwise a same-tick down+up is
        // released before draw() resolves and the rising edge is missed.
        dispatch_key(&canvas, "keydown", "Tab");
        dispatch_key(&canvas, "keydown", "Backslash");
        sleep(24).await; // >= one draw: FF engages, FrameAdvance rising edge fires
        dispatch_key(&canvas, "keyup", "Tab");
        dispatch_key(&canvas, "keyup", "Backslash");
        sleep(24).await; // >= one draw: release observed
    }

    // Proof the harness actually exercised draw() + the dispatch path (not a
    // vacuous pass): `post_action` is only reachable from inside draw(), so a
    // non-zero count means wgpu initialized, draw() ran, and a fast-forward /
    // frame-advance hotkey drove dispatch_action.
    assert!(
        posted.get() > 0,
        "no fast-forward/frame-advance hotkey reached dispatch_action — draw() \
         never ran the input path (wgpu init failed headless, or the synthetic \
         keys didn't reach winit), so the reentrancy scenario wasn't exercised"
    );
}
