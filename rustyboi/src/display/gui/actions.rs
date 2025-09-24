pub enum FileData {
    #[cfg(not(target_arch = "wasm32"))]
    Path(std::path::PathBuf),
    #[cfg(target_arch = "wasm32")]
    Contents { name: String, data: Vec<u8> },
}

pub enum GuiAction {
    Exit,
    SaveState(std::path::PathBuf),
    LoadState(FileData),
    LoadRom(FileData),
    TogglePause,
    Restart,
    ClearError,
    StepCycles(u32),
    StepFrames(u32),
    SetBreakpoint(u16),
    RemoveBreakpoint(u16),
}
