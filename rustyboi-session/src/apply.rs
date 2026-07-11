//! The single implementation of [`UiAction`] behavior:
//! [`Session::apply`](crate::session::Session::apply) and its outcome types.
//!
//! `apply` performs everything toolkit- and OS-independent inline (pause,
//! palette, SGB/touch toggles, hardware rebuild, slots, rewind config, printer,
//! breakpoints, debug stepping requests) and returns the pieces of work only the
//! host can do as [`PlatformRequest`]s the frontend performs after the call.

use crate::action::{LoadPurpose, PaletteChoice, UiAction};
use crate::session::Session;

/// Why a URL is being fetched, so the frontend routes the downloaded bytes back
/// to the right finisher. Kept typed (not just a bare URL) so the same
/// [`PlatformRequest::FetchUrl`] mechanism can serve future network features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchPurpose {
    /// A libretro `.cht` cheat file; the body goes to
    /// [`Session::finish_fetched_cheats`](crate::session::Session::finish_fetched_cheats).
    Cheats,
    /// A libretro No-Intro `.dat` (game-name index) file; the body goes to
    /// [`Session::finish_no_intro_dats`](crate::session::Session::finish_no_intro_dats).
    NoIntro,
}

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
    /// Hand `bytes` to the user as a downloadable/saveable file under
    /// `suggested_name` (File → Export battery/RTC/state). Path-free so it works
    /// uniformly on web (browser download), desktop (rfd save dialog), and
    /// Android (SAF create-document).
    SaveBytes { suggested_name: String, bytes: Vec<u8> },
    /// The frontend must read the bytes behind a picked file and feed them back
    /// via the finisher for `purpose`
    /// ([`finish_load_rom`](crate::session::Session::finish_load_rom),
    /// [`finish_load_state`](crate::session::Session::finish_load_state),
    /// [`finish_import_battery`](crate::session::Session::finish_import_battery),
    /// or [`finish_import_rtc`](crate::session::Session::finish_import_rtc)).
    /// Carried through the frontend's file resolver (path→bytes on desktop,
    /// content bytes on web/Android).
    LoadFile { file: crate::action::FileData, purpose: crate::action::LoadPurpose },
    /// Fetch a URL over HTTP(S) and feed the response body back to the session
    /// for `purpose`. `urls` are tried in order until one succeeds (the libretro
    /// cheat DB occasionally misfiles an entry across the GB/GBC folders). The
    /// session is WASM-clean and never performs the request itself; each frontend
    /// downloads the bytes and calls the matching finisher
    /// ([`finish_fetched_cheats`](crate::session::Session::finish_fetched_cheats)
    /// for [`FetchPurpose::Cheats`]).
    FetchUrl { urls: Vec<String>, purpose: FetchPurpose },
    /// A status line to show the user.
    Status(String),
    /// An error to show the user.
    Error(String),
    /// Clear any UI error overlay (a load succeeded / error was dismissed).
    ClearError,
    /// Toggle host fullscreen. Serviced by the windowed frontend's
    /// [`Frontend::toggle_fullscreen`](crate::apply::PlatformRequest): desktop
    /// flips the winit window, web the canvas Fullscreen API, Android no-ops.
    ToggleFullscreen,
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

            // Record from the current machine state (works from anywhere: the
            // movie carries a savestate so replay reconstructs exactly here); a
            // second toggle finishes it and hands the bytes to the frontend as a
            // saveable `.rbmovie` (browser download on web).
            UiAction::ToggleRecording => {
                if self.is_recording() {
                    match self.stop_recording() {
                        Some(movie) => {
                            let frames = movie.inputs.len();
                            let mut o = ActionOutcome::default();
                            o.push(PlatformRequest::SaveBytes {
                                suggested_name: "recording.rbmovie".into(),
                                bytes: movie.to_bytes(),
                            });
                            o.push(PlatformRequest::Status(format!(
                                "Recording stopped ({frames} frames)"
                            )));
                            o
                        }
                        None => ActionOutcome::status("Not recording"),
                    }
                } else {
                    match self.start_recording_from_state() {
                        Ok(()) => ActionOutcome::status("Recording started"),
                        Err(e) => {
                            ActionOutcome::error(format!("Failed to start recording: {e}"))
                        }
                    }
                }
            }
            UiAction::LoadMovie(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile {
                    file,
                    purpose: LoadPurpose::Movie,
                }],
                pause_changed: false,
            },
            UiAction::StopReplay => {
                if self.is_playing() {
                    self.stop_playback();
                    ActionOutcome::status("Replay stopped")
                } else {
                    ActionOutcome::default()
                }
            }

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
            UiAction::SetGbcDmgPalette(choice) => {
                self.set_gbc_dmg_palette(choice);
                let (w, h) = self.content_size();
                ActionOutcome {
                    requests: vec![PlatformRequest::ResizeContent { width: w, height: h }],
                    pause_changed: true,
                }
            }
            UiAction::SetColorCorrection(conversion) => {
                self.set_color_correction(conversion);
                ActionOutcome::default()
            }
            UiAction::SetRealBootRom(enabled) => {
                self.set_real_boot_rom(enabled);
                let (w, h) = self.content_size();
                ActionOutcome {
                    requests: vec![
                        PlatformRequest::ResizeContent { width: w, height: h },
                        PlatformRequest::Status(if enabled {
                            "Real boot ROM enabled; ROM restarted".into()
                        } else {
                            "Real boot ROM disabled; ROM restarted".into()
                        }),
                    ],
                    pause_changed: true,
                }
            }
            UiAction::SetTextureFilter(filter) => {
                self.set_texture_filter(filter);
                ActionOutcome::default()
            }
            UiAction::SetLcdEffect(effect) => {
                self.set_lcd_effect(effect);
                ActionOutcome::default()
            }
            UiAction::SetPrinterScale(scale) => {
                self.set_printer_scale(scale);
                ActionOutcome::default()
            }
            UiAction::SetTouchOpacity(opacity) => {
                self.set_touch_opacity(opacity);
                ActionOutcome::default()
            }
            UiAction::LoadBootRom(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile {
                    file,
                    purpose: LoadPurpose::BootRom,
                }],
                pause_changed: false,
            },
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
            UiAction::SetVolume(volume) => {
                self.set_volume(volume);
                ActionOutcome::default()
            }
            UiAction::SetScalingMode(scaling) => {
                self.set_scaling_mode(scaling);
                ActionOutcome::default()
            }
            UiAction::ToggleFullscreen => ActionOutcome {
                requests: vec![PlatformRequest::ToggleFullscreen],
                pause_changed: false,
            },

            UiAction::SetInputConfig(input) => {
                self.set_input_config(input);
                ActionOutcome::default()
            }

            UiAction::AddCheat(code) => match self.add_cheat(&code) {
                Ok(_) => ActionOutcome::status(format!("Cheat added: {code}")),
                Err(e) => ActionOutcome::error(format!("Invalid cheat code '{code}': {e}")),
            },
            UiAction::AddCheats(codes) => {
                let mut added = 0;
                let mut failed = 0;
                for code in &codes {
                    match self.add_cheat(code) {
                        Ok(_) => added += 1,
                        Err(_) => failed += 1,
                    }
                }
                self.clear_fetched_cheats();
                if failed == 0 {
                    ActionOutcome::status(format!("Added {added} cheats"))
                } else {
                    ActionOutcome::status(format!("Added {added} cheats ({failed} failed to decode)"))
                }
            }
            UiAction::RemoveCheat(code) => {
                if self.remove_cheat(&code) {
                    ActionOutcome::status(format!("Cheat removed: {code}"))
                } else {
                    ActionOutcome::error(format!("No such cheat: {code}"))
                }
            }
            UiAction::GetCheats => match self.cheat_fetch_urls() {
                Some(urls) => {
                    let mut o = ActionOutcome::status("Fetching cheats…");
                    o.push(PlatformRequest::FetchUrl {
                        urls,
                        purpose: FetchPurpose::Cheats,
                    });
                    o
                }
                None if self.original_rom_bytes().is_none() => {
                    ActionOutcome::status("Load a ROM first to fetch cheats")
                }
                None => ActionOutcome::status(
                    "Couldn't identify this game — no cheats found for it",
                ),
            },
            UiAction::ClearFetchedCheats => {
                self.clear_fetched_cheats();
                ActionOutcome::default()
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
            UiAction::LoadRom(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile { file, purpose: LoadPurpose::Rom }],
                pause_changed: false,
            },
            UiAction::LoadState(file) | UiAction::ImportState(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile { file, purpose: LoadPurpose::State }],
                pause_changed: false,
            },
            UiAction::ImportBatterySave(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile { file, purpose: LoadPurpose::Battery }],
                pause_changed: false,
            },
            UiAction::ImportRtc(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile { file, purpose: LoadPurpose::Rtc }],
                pause_changed: false,
            },
            UiAction::ApplyPatch(file) => ActionOutcome {
                requests: vec![PlatformRequest::LoadFile { file, purpose: LoadPurpose::Patch }],
                pause_changed: false,
            },

            // Export: produce a path-free SaveBytes request the frontend delivers
            // as a file (download on web, save dialog on desktop/Android).
            UiAction::ExportState => match self.gb().to_state_bytes() {
                Ok(bytes) => {
                    let mut o = ActionOutcome::default();
                    o.push(PlatformRequest::SaveBytes {
                        suggested_name: "savestate.rustyboisave".into(),
                        bytes,
                    });
                    o
                }
                Err(e) => ActionOutcome::error(format!("Failed to export state: {e}")),
            },
            UiAction::ExportBatterySave => match self.export_battery() {
                Some(bytes) => {
                    let mut o = ActionOutcome::default();
                    o.push(PlatformRequest::SaveBytes {
                        suggested_name: "battery.sav".into(),
                        bytes,
                    });
                    o
                }
                None => ActionOutcome::error("This cartridge has no battery save"),
            },
            UiAction::ExportRtc => match self.export_rtc() {
                Some(bytes) => {
                    let mut o = ActionOutcome::default();
                    o.push(PlatformRequest::SaveBytes {
                        suggested_name: "clock.rtc".into(),
                        bytes,
                    });
                    o
                }
                None => ActionOutcome::error("This cartridge has no real-time clock"),
            },

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
            ToggleRecording,
            StopReplay,
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
            SetPalette(PaletteChoice::Pocket),
            SetColorCorrection(crate::CgbColorConversion::Lcd),
            SetRealBootRom(false),
            SetTextureFilter(crate::action::TextureFilter::Linear),
            SetLcdEffect(crate::action::LcdEffect::Grid),
            SetRewindEnabled(false),
            SetRewindInterval(4),
            SetRewindDepth(30),
            SetVolume(50),
            SetScalingMode(crate::action::ScalingMode::IntegerAspect),
            ToggleFullscreen,
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
        s.apply(UiAction::SetPalette(PaletteChoice::GreenLcd), 0);
        assert_eq!(s.palette(), PaletteChoice::GreenLcd);
        assert_eq!(s.config().dmg_palette.shades, PaletteChoice::GreenLcd.rgba_shades());
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

    // With no ROM loaded, GetCheats reports "Load a ROM first" as a non-fatal
    // status (never the fatal error screen) and emits no fetch.
    #[test]
    fn get_cheats_without_rom_reports_status() {
        let mut s = session();
        let out = s.apply(UiAction::GetCheats, 0);
        assert!(!out
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::FetchUrl { .. })));
        match out.requests.as_slice() {
            [PlatformRequest::Status(msg)] => assert!(msg.contains("Load a ROM")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    // A fetched `.cht` body populates the picker; AddCheats routes the picked
    // codes through the normal add-cheat path and clears the fetched list.
    #[test]
    fn finish_and_add_fetched_cheats() {
        let mut s = session();
        let body = "cheats = 1\ncheat0_desc = \"Test\"\ncheat0_code = \"01FFDEC0\"\n";
        assert_eq!(s.finish_fetched_cheats(body), 1);
        assert_eq!(s.fetched_cheats().len(), 1);

        let out = s.apply(UiAction::AddCheats(vec!["01FFDEC0".into()]), 0);
        assert!(out
            .requests
            .iter()
            .any(|r| matches!(r, PlatformRequest::Status(_))));
        assert!(s.cheats().any(|c| c == "01FFDEC0"));
        // Adding clears the pending fetched list.
        assert!(s.fetched_cheats().is_empty());

        // ClearFetchedCheats is a no-op-safe dismiss.
        s.finish_fetched_cheats(body);
        s.apply(UiAction::ClearFetchedCheats, 0);
        assert!(s.fetched_cheats().is_empty());
    }
}
