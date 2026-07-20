//! The compile-time frontend capability contract.
//!
//! [`Frontend`] is the set of host capabilities the shared action driver needs
//! from any *windowed* frontend (desktop, Android, later web). [`drive_action`]
//! is the single dispatcher: it hands a [`UiAction`] to [`Session::apply`] and
//! then routes each returned [`PlatformRequest`] to a `Frontend` method.
//!
//! The enforcement is structural: `drive_action` is generic over `F: Frontend`
//! and calls **every** capability method, so a frontend type that fails to
//! implement one of them cannot be passed to `drive_action` — it won't compile.
//! Libretro is deliberately NOT a `Frontend` (RetroArch owns its UI/input); it
//! calls `Session::apply`/the session directly instead of using this driver.

use rustyboi_session::action::{FileData, LoadPurpose};
use rustyboi_session::apply::{FetchPurpose, PlatformRequest};
use rustyboi_session::{Session, UiAction};

/// The capabilities the shared action driver requires of a windowed frontend.
///
/// A frontend implements this once; [`drive_action`] then performs every UI
/// command through it. Missing any method is a compile error at the
/// `drive_action::<ThisFrontend>` instantiation.
pub(crate) trait Frontend {
    /// Mutable access to the owned session (the driver applies actions to it).
    fn session_mut(&mut self) -> &mut Session;

    /// Show a transient status line.
    fn set_status(&mut self, message: String);

    /// Show an error to the user.
    fn set_error(&mut self, message: String);

    /// Clear any error overlay (a load succeeded / the error was dismissed).
    fn clear_error(&mut self);

    /// The user asked to quit; perform the host exit.
    fn exit(&mut self);

    /// Toggle host fullscreen: desktop flips the winit window, web the canvas
    /// Fullscreen API, Android no-ops (already fullscreen).
    fn toggle_fullscreen(&mut self);

    /// The presented content size changed (SGB border / hardware toggle); resize
    /// the window/surface to fit `width x height` (pre-scale pixels).
    fn resize_content(&mut self, width: u32, height: u32);

    /// Write serialized savestate `bytes` to `path` (File → Save State).
    fn save_state_bytes(&mut self, path: std::path::PathBuf, bytes: Vec<u8>);

    /// Deliver `bytes` to the user as a downloadable/saveable file named
    /// `suggested_name` (File → Export battery/RTC/state): a browser download on
    /// web, an rfd save dialog on desktop, a SAF create-document on Android.
    fn save_bytes(&mut self, suggested_name: String, bytes: Vec<u8>);

    /// Resolve + apply a picked file. The frontend reads the bytes (path on
    /// desktop, content on web/Android) and feeds them into the session via the
    /// finisher for `purpose` (`finish_load_rom` / `finish_load_state` /
    /// `finish_import_battery` / `finish_import_rtc`).
    fn load_file(&mut self, file: FileData, purpose: LoadPurpose);

    /// Fetch `urls` (tried in order) over HTTP and feed the response body back to
    /// the session for `purpose` (e.g. parse a libretro `.cht` via
    /// `Session::finish_fetched_cheats`). Desktop/Android do the GET on a
    /// background thread; web hands it to the JS `fetch()` bridge.
    fn fetch_url(&mut self, urls: Vec<String>, purpose: FetchPurpose);

    /// The session run/pause state changed in a way the frontend's pause model
    /// must observe (toggle pause, restart, frame advance, error clear, load).
    fn on_pause_changed(&mut self, action_hint: PauseHint);

    /// Service an Android ROM-library / SAF action the session handed back.
    #[cfg(target_os = "android")]
    fn android_library(&mut self, action: UiAction);
}

/// Which pause-affecting command triggered an [`Frontend::on_pause_changed`], so
/// the frontend can apply the exact bookkeeping each one needs (they differ:
/// toggle flips user-pause, restart resets it, error-clear pauses).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseHint {
    TogglePause,
    Restart,
    ClearError,
    FrameAdvance,
    /// Hardware change rebuilt the machine: clear error/frame but keep the
    /// user's pause state (matches the pre-refactor behavior).
    SetHardware,
    Load,
}

/// Apply a [`UiAction`] through the shared [`Session::apply`], then route the
/// resulting host work to the `Frontend`. This is the one path every windowed
/// frontend uses; the generic bound makes the capability set compile-checked.
pub(crate) fn drive_action<F: Frontend>(frontend: &mut F, action: UiAction, timestamp: u64) {
    let pause_hint = pause_hint_for(&action);
    let outcome = frontend.session_mut().apply(action, timestamp);

    for req in outcome.requests {
        match req {
            PlatformRequest::Exit => frontend.exit(),
            PlatformRequest::ToggleFullscreen => frontend.toggle_fullscreen(),
            PlatformRequest::ResizeContent { width, height } => {
                frontend.resize_content(width, height)
            }
            PlatformRequest::SaveStateBytes { path, bytes } => {
                frontend.save_state_bytes(path, bytes)
            }
            PlatformRequest::SaveBytes { suggested_name, bytes } => {
                frontend.save_bytes(suggested_name, bytes)
            }
            PlatformRequest::LoadFile { file, purpose } => frontend.load_file(file, purpose),
            PlatformRequest::FetchUrl { urls, purpose } => frontend.fetch_url(urls, purpose),
            PlatformRequest::Status(s) => frontend.set_status(s),
            PlatformRequest::Error(e) => frontend.set_error(e),
            PlatformRequest::ClearError => frontend.clear_error(),
            #[cfg(target_os = "android")]
            PlatformRequest::AndroidLibrary(a) => frontend.android_library(a),
        }
    }

    if outcome.pause_changed
        && let Some(hint) = pause_hint
    {
        frontend.on_pause_changed(hint);
    }
}

fn pause_hint_for(action: &UiAction) -> Option<PauseHint> {
    match action {
        UiAction::TogglePause => Some(PauseHint::TogglePause),
        UiAction::Restart => Some(PauseHint::Restart),
        UiAction::ClearError => Some(PauseHint::ClearError),
        UiAction::FrameAdvance => Some(PauseHint::FrameAdvance),
        UiAction::LoadRom(_) | UiAction::LoadState(_) | UiAction::ImportState(_) => {
            Some(PauseHint::Load)
        }
        UiAction::SetHardware(_) => Some(PauseHint::SetHardware),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyboi_session::action::HardwareChoice;

    fn file() -> FileData {
        FileData::Path(std::path::PathBuf::from("x.gb"))
    }

    #[test]
    fn pausing_actions_map_to_their_hint() {
        assert_eq!(pause_hint_for(&UiAction::TogglePause), Some(PauseHint::TogglePause));
        assert_eq!(pause_hint_for(&UiAction::Restart), Some(PauseHint::Restart));
        assert_eq!(pause_hint_for(&UiAction::ClearError), Some(PauseHint::ClearError));
        assert_eq!(pause_hint_for(&UiAction::FrameAdvance), Some(PauseHint::FrameAdvance));
        assert_eq!(pause_hint_for(&UiAction::LoadRom(file())), Some(PauseHint::Load));
        assert_eq!(pause_hint_for(&UiAction::LoadState(file())), Some(PauseHint::Load));
        assert_eq!(pause_hint_for(&UiAction::ImportState(file())), Some(PauseHint::Load));
        assert_eq!(
            pause_hint_for(&UiAction::SetHardware(HardwareChoice::Cgb)),
            Some(PauseHint::SetHardware)
        );
    }

    #[test]
    fn non_pausing_actions_have_no_hint() {
        assert_eq!(pause_hint_for(&UiAction::ToggleFastForward), None);
        assert_eq!(pause_hint_for(&UiAction::Quicksave), None);
        assert_eq!(pause_hint_for(&UiAction::TogglePrinter), None);
    }

    use rustyboi_session::config::Config;
    use rustyboi_session::ports::{MemRumble, MemStorage, MemWebcam};
    use rustyboi_session::session::Ports;

    /// A `Frontend` that records which capability methods `drive_action` called,
    /// in order, so the request→method routing can be asserted without a window.
    struct RecordingFrontend {
        session: Session,
        calls: Vec<String>,
    }

    impl RecordingFrontend {
        fn new() -> Self {
            let ports = Ports {
                storage: Box::new(MemStorage::new()),
                rumble: Box::new(MemRumble::default()),
                webcam: Box::new(MemWebcam::default()),
            };
            RecordingFrontend {
                session: Session::new(Config::default(), ports, [0u8; 32]),
                calls: Vec::new(),
            }
        }
    }

    impl Frontend for RecordingFrontend {
        fn session_mut(&mut self) -> &mut Session {
            &mut self.session
        }
        fn set_status(&mut self, _message: String) {
            self.calls.push("set_status".into());
        }
        fn set_error(&mut self, _message: String) {
            self.calls.push("set_error".into());
        }
        fn clear_error(&mut self) {
            self.calls.push("clear_error".into());
        }
        fn exit(&mut self) {
            self.calls.push("exit".into());
        }
        fn toggle_fullscreen(&mut self) {
            self.calls.push("toggle_fullscreen".into());
        }
        fn resize_content(&mut self, _width: u32, _height: u32) {
            self.calls.push("resize_content".into());
        }
        fn save_state_bytes(&mut self, _path: std::path::PathBuf, _bytes: Vec<u8>) {
            self.calls.push("save_state_bytes".into());
        }
        fn save_bytes(&mut self, _suggested_name: String, _bytes: Vec<u8>) {
            self.calls.push("save_bytes".into());
        }
        fn load_file(&mut self, _file: FileData, _purpose: LoadPurpose) {
            self.calls.push("load_file".into());
        }
        fn fetch_url(&mut self, _urls: Vec<String>, _purpose: FetchPurpose) {
            self.calls.push("fetch_url".into());
        }
        fn on_pause_changed(&mut self, hint: PauseHint) {
            self.calls.push(format!("on_pause_changed({hint:?})"));
        }
        #[cfg(target_os = "android")]
        fn android_library(&mut self, _action: UiAction) {
            self.calls.push("android_library".into());
        }
    }

    fn drive(action: UiAction) -> Vec<String> {
        let mut f = RecordingFrontend::new();
        drive_action(&mut f, action, 0);
        f.calls
    }

    // Exit routes to exit(); no pause bookkeeping (not a pausing action).
    #[test]
    fn exit_routes_to_exit_only() {
        assert_eq!(drive(UiAction::Exit), vec!["exit"]);
    }

    // ToggleFullscreen routes to toggle_fullscreen(); non-pausing.
    #[test]
    fn toggle_fullscreen_routes_to_its_method() {
        assert_eq!(drive(UiAction::ToggleFullscreen), vec!["toggle_fullscreen"]);
    }

    // A load produces a single LoadFile request → load_file(); crucially its
    // outcome has pause_changed == false, so despite a Some(Load) pause hint,
    // on_pause_changed must NOT fire (the negative case).
    #[test]
    fn load_rom_routes_to_load_file_without_pause_callback() {
        let calls = drive(UiAction::LoadRom(file()));
        assert_eq!(calls, vec!["load_file"]);
        assert!(
            !calls.iter().any(|c| c.starts_with("on_pause_changed")),
            "pause_changed==false must suppress on_pause_changed even for a pausing action"
        );
    }

    // Restart emits ClearError, ResizeContent, Status in that order, then — since
    // its outcome sets pause_changed and the hint is Some — on_pause_changed last.
    #[test]
    fn restart_routes_requests_in_order_then_pause_callback() {
        assert_eq!(
            drive(UiAction::Restart),
            vec![
                "clear_error",
                "resize_content",
                "set_status",
                "on_pause_changed(Restart)",
            ]
        );
    }

    // TogglePause has no host requests but a pause change: only the callback fires.
    #[test]
    fn toggle_pause_fires_only_the_pause_callback() {
        assert_eq!(drive(UiAction::TogglePause), vec!["on_pause_changed(TogglePause)"]);
    }

    // ClearError routes clear_error + status, then the pause callback with the
    // matching hint.
    #[test]
    fn clear_error_routes_then_pause_callback() {
        assert_eq!(
            drive(UiAction::ClearError),
            vec!["clear_error", "set_status", "on_pause_changed(ClearError)"]
        );
    }

    // An action whose outcome is an Error request routes to set_error().
    #[test]
    fn error_outcome_routes_to_set_error() {
        // An unparseable cheat code yields an Error outcome (no ROM needed).
        assert_eq!(drive(UiAction::AddCheat("not-a-code".into())), vec!["set_error"]);
    }
}
