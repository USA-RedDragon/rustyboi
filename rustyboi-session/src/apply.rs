//! The single implementation of [`UiAction`] behavior:
//! [`Session::apply`](crate::session::Session::apply) and its outcome types.
//!
//! `apply` performs everything toolkit- and OS-independent inline (pause,
//! palette, SGB/touch toggles, hardware rebuild, slots, rewind config, printer,
//! breakpoints, debug stepping requests) and returns the pieces of work only the
//! host can do as [`PlatformRequest`]s the frontend performs after the call.

use crate::action::{PaletteChoice, UiAction};
use crate::session::Session;

/// Something only the host (OS / window / filesystem) can carry out, surfaced by
/// [`Session::apply`] for the frontend to perform. This is the shared contract
/// mirror of the frontend's old `PlatformRequest` — now the one definition.
#[derive(Debug)]
pub enum PlatformRequest {
    /// The user asked to quit.
    Exit,
    /// The presented content size changed (SGB border / hardware toggle). The
    /// frontend resizes its window/surface to fit. Dimensions are the un-scaled
    /// content size in pixels.
    ResizeContent { width: u32, height: u32 },
    /// Write the serialized machine state to a user-chosen path (File → Save
    /// State). The session hands over the bytes; the frontend writes them.
    SaveStateBytes { path: std::path::PathBuf, bytes: Vec<u8> },
    /// The frontend must read the bytes behind a picked file and feed them back
    /// via [`Session::finish_load_rom`](crate::session::Session::finish_load_rom)
    /// / [`Session::finish_load_state`](crate::session::Session::finish_load_state).
    /// Carried through the frontend's file resolver (path→bytes on desktop,
    /// content bytes on web/Android).
    LoadFile(crate::action::FileData),
    /// A status line to show the user.
    Status(String),
    /// An error to show the user.
    Error(String),
    /// Clear any UI error overlay (a load succeeded / error was dismissed).
    ClearError,
    /// An Android ROM-library / SAF action the session can't service itself (it
    /// needs the JNI bridge + library panel, both host-owned).
    #[cfg(target_os = "android")]
    AndroidLibrary(UiAction),
}

/// What applying a [`UiAction`] produced. `requests` are performed by the
/// frontend; `pause_changed` tells a windowed frontend its run/pause bookkeeping
/// may need to re-sync (a pause toggle, restart, or content load happened).
#[derive(Debug, Default)]
pub struct ActionOutcome {
    pub requests: Vec<PlatformRequest>,
    /// Set when the action changed the session run mode in a way the frontend's
    /// pause model should observe (toggle pause, restart, frame advance, load).
    pub pause_changed: bool,
}

impl ActionOutcome {
    fn status(msg: impl Into<String>) -> Self {
        ActionOutcome {
            requests: vec![PlatformRequest::Status(msg.into())],
            pause_changed: false,
        }
    }

    fn error(msg: impl Into<String>) -> Self {
        ActionOutcome {
            requests: vec![PlatformRequest::Error(msg.into())],
            pause_changed: false,
        }
    }

    fn push(&mut self, req: PlatformRequest) {
        self.requests.push(req);
    }
}

impl Session {
    /// Apply a [`UiAction`], performing all toolkit/OS-independent behavior and
    /// returning the host work + pause hint. This is the ONE place a UI command
    /// is interpreted; every frontend routes its commands through here.
    ///
    /// `timestamp` is caller-supplied wall-clock epoch seconds for slot saves
    /// (the session never reads a clock); pass 0 where unavailable.
    pub fn apply(&mut self, action: UiAction, timestamp: u64) -> ActionOutcome {
        match action {
            UiAction::Exit => ActionOutcome {
                requests: vec![PlatformRequest::Exit],
                pause_changed: false,
            },

            // Pause is windowed-frontend run-loop state (it interacts with the
            // menu-open auto-pause), so the session doesn't own the flip; it
            // only signals the frontend to re-sync its pause model.
            UiAction::TogglePause => {
                ActionOutcome { requests: Vec::new(), pause_changed: true }
            }

            UiAction::Restart => {
                self.restart();
                let (w, h) = self.content_size();
                ActionOutcome {
                    requests: vec![
                        PlatformRequest::ResizeContent { width: w, height: h },
                        PlatformRequest::Status("Emulation restarted".into()),
                    ],
                    pause_changed: true,
                }
            }

            UiAction::ClearError => ActionOutcome {
                requests: vec![
                    PlatformRequest::ClearError,
                    PlatformRequest::Status(
                        "Error cleared for debugging - CPU state preserved".into(),
                    ),
                ],
                pause_changed: true,
            },

            UiAction::TogglePrinter => {
                if self.gb().printer_attached() {
                    self.gb_mut().detach_serial_device();
                    ActionOutcome::status("Game Boy Printer disconnected")
                } else {
                    self.gb_mut().attach_printer();
                    ActionOutcome::status(
                        "Game Boy Printer connected - prints are saved next to the ROM",
                    )
                }
            }

            UiAction::StepCycles(count) => {
                self.request_step_cycles(count);
                ActionOutcome::default()
            }
            UiAction::StepFrames(count) => {
                self.request_step_frames(count);
                ActionOutcome::default()
            }
            UiAction::SetBreakpoint(address) => {
                self.gb_mut().add_breakpoint(address);
                ActionOutcome::status(format!("Breakpoint set at ${address:04X}"))
            }
            UiAction::RemoveBreakpoint(address) => {
                self.gb_mut().remove_breakpoint(address);
                ActionOutcome::status(format!("Breakpoint removed from ${address:04X}"))
            }

            UiAction::SaveSlot(slot) => match self.save_slot(slot, timestamp) {
                Ok(()) => ActionOutcome::status(format!("Saved to slot {slot}")),
                Err(e) => ActionOutcome::error(format!("Failed to save slot {slot}: {e}")),
            },
            UiAction::LoadSlot(slot) => match self.load_slot(slot) {
                Ok(_) => {
                    let mut o = ActionOutcome::status(format!("Loaded slot {slot}"));
                    o.requests.insert(0, PlatformRequest::ClearError);
                    o
                }
                Err(e) => ActionOutcome::error(format!("Failed to load slot {slot}: {e}")),
            },
            UiAction::Quicksave => match self.quicksave(timestamp) {
                Ok(()) => ActionOutcome::status("Quicksaved"),
                Err(e) => ActionOutcome::error(format!("Quicksave failed: {e}")),
            },
            UiAction::Quickload => match self.quickload() {
                Ok(_) => {
                    let mut o = ActionOutcome::status("Quickloaded");
                    o.requests.insert(0, PlatformRequest::ClearError);
                    o
                }
                Err(e) => ActionOutcome::error(format!("Quickload failed: {e}")),
            },

            UiAction::ToggleFastForward => {
                self.toggle_fast_forward();
                let on = self.is_fast_forward();
                ActionOutcome::status(if on { "Fast-forward on" } else { "Fast-forward off" })
            }
            UiAction::FrameAdvance => {
                self.frame_advance();
                ActionOutcome { requests: Vec::new(), pause_changed: true }
            }
            UiAction::ToggleSgbBorder => {
                self.set_sgb_border(!self.sgb_border());
                let (w, h) = self.content_size();
                ActionOutcome {
                    requests: vec![PlatformRequest::ResizeContent { width: w, height: h }],
                    pause_changed: false,
                }
            }
            UiAction::ToggleTouchControls => {
                self.set_touch_controls(!self.touch_controls());
                ActionOutcome::default()
            }

            UiAction::SetHardware(choice) => {
                self.set_hardware_choice(choice);
                let (w, h) = self.content_size();
                ActionOutcome {
                    requests: vec![
                        PlatformRequest::ClearError,
                        PlatformRequest::ResizeContent { width: w, height: h },
                        PlatformRequest::Status(format!(
                            "Hardware set to {choice:?}; ROM restarted"
                        )),
                    ],
                    pause_changed: true,
                }
            }
            UiAction::SetPalette(choice) => {
                self.set_palette_choice(choice);
                ActionOutcome::default()
            }
            UiAction::SetRewindEnabled(enabled) => {
                self.set_rewind_enabled(enabled);
                ActionOutcome::default()
            }
            UiAction::SetRewindInterval(interval) => {
                self.set_rewind_interval(interval);
                ActionOutcome::default()
            }
            UiAction::SetRewindDepth(depth) => {
                self.set_rewind_depth(depth);
                ActionOutcome::default()
            }

            UiAction::AddCheat(code) => match self.add_cheat(&code) {
                Ok(_) => ActionOutcome::status(format!("Cheat added: {code}")),
                Err(e) => ActionOutcome::error(format!("Invalid cheat code '{code}': {e}")),
            },
            UiAction::RemoveCheat(code) => {
                if self.remove_cheat(&code) {
                    ActionOutcome::status(format!("Cheat removed: {code}"))
                } else {
                    ActionOutcome::error(format!("No such cheat: {code}"))
                }
            }

            // OS-requiring: hand off to the frontend.
            UiAction::SaveState(path) => match self.gb().to_state_bytes() {
                Ok(bytes) => {
                    let mut o = ActionOutcome::default();
                    o.push(PlatformRequest::SaveStateBytes { path, bytes });
                    o
                }
                Err(e) => ActionOutcome::error(format!("Failed to save state: {e}")),
            },
            action @ (UiAction::LoadRom(_) | UiAction::LoadState(_)) => {
                let file = match action {
                    UiAction::LoadRom(f) | UiAction::LoadState(f) => f,
                    _ => unreachable!(),
                };
                ActionOutcome {
                    requests: vec![PlatformRequest::LoadFile(file)],
                    pause_changed: false,
                }
            }

            // Android library / SAF actions need the JNI bridge + panel.
            #[cfg(target_os = "android")]
            other => ActionOutcome {
                requests: vec![PlatformRequest::AndroidLibrary(other)],
                pause_changed: false,
            },
        }
    }
}

/// Map an application config palette to the RGBA shades stored in config, for
/// [`Session::set_palette_choice`].
pub(crate) fn palette_shades(choice: PaletteChoice) -> [[u8; 4]; 4] {
    choice.rgba_shades()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::HardwareChoice;
    use crate::config::Config;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};
    use crate::session::Ports;

    fn session() -> Session {
        let ports = Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        };
        Session::new(Config::default(), ports, [0u8; 32])
    }

    // Every non-file, non-Android `UiAction` must be handled by `apply` without
    // panicking. File loads route to a `LoadFile` request; this exercises the
    // rest of the match so a new variant can't silently fall through.
    #[test]
    fn apply_handles_each_pure_action() {
        use UiAction::*;
        let actions = [
            TogglePause,
            TogglePrinter,
            Restart,
            ClearError,
            StepCycles(3),
            StepFrames(2),
            SetBreakpoint(0x100),
            RemoveBreakpoint(0x100),
            Quicksave,
            Quickload,
            ToggleFastForward,
            FrameAdvance,
            ToggleSgbBorder,
            ToggleTouchControls,
            SetHardware(HardwareChoice::Dmg),
            SetPalette(PaletteChoice::Blue),
            SetRewindEnabled(false),
            SetRewindInterval(4),
            SetRewindDepth(30),
        ];
        let mut s = session();
        for a in actions {
            let _ = s.apply(a, 0);
        }
    }

    #[test]
    fn toggle_sgb_border_flips_and_requests_resize() {
        let mut s = session();
        let before = s.sgb_border();
        let out = s.apply(UiAction::ToggleSgbBorder, 0);
        assert_ne!(s.sgb_border(), before);
        assert!(out
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::ResizeContent { .. })));
    }

    #[test]
    fn set_palette_persists_choice() {
        let mut s = session();
        s.apply(UiAction::SetPalette(PaletteChoice::Red), 0);
        assert_eq!(s.palette(), PaletteChoice::Red);
        assert_eq!(s.config().dmg_palette.shades, PaletteChoice::Red.rgba_shades());
    }

    #[test]
    fn exit_requests_exit() {
        let mut s = session();
        let out = s.apply(UiAction::Exit, 0);
        assert!(matches!(out.requests.as_slice(), [PlatformRequest::Exit]));
    }

    // A valid Game Genie code (from `rustyboi_core::cheats` tests) added via the
    // action shows up in `cheats()` and yields a Status; an unparseable code
    // yields an Error and adds nothing.
    #[test]
    fn add_cheat_action_registers_and_rejects() {
        let mut s = session();

        let out = s.apply(UiAction::AddCheat("00A-B7F".into()), 0);
        assert!(out
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::Status(_))));
        assert!(s.cheats().any(|c| c == "00A-B7F"));

        let bad = s.apply(UiAction::AddCheat("nope".into()), 0);
        assert!(bad
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::Error(_))));

        let removed = s.apply(UiAction::RemoveCheat("00A-B7F".into()), 0);
        assert!(removed
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::Status(_))));
        assert!(s.cheats().next().is_none());
    }
}
