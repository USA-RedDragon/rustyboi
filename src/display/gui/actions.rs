pub enum GuiAction {
    Exit,
    SaveState(std::path::PathBuf),
    LoadState(std::path::PathBuf),
    LoadRom(std::path::PathBuf),
    TogglePause,
    Restart,
    ClearError,
    StepCycles(u32),
    StepFrames(u32),
}
