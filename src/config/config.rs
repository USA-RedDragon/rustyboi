use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct RawConfig {
    /// BIOS file path, optional
    #[arg(short, long)]
    bios: Option<String>,

    /// ROM file path, optional
    #[arg(short, long)]
    rom: Option<String>,

    /// Save state file path to load on startup, optional
    #[arg(long)]
    state: Option<String>,

    /// Scale factor for GUI
    #[arg(short, long, default_value_t = 5)]
    scale: u8,

    /// Run with CLI (no GUI)
    #[arg(long, default_value_t = false)]
    cli: bool,

    /// Skip BIOS on startup
    #[arg(long, default_value_t = false)]
    skip_bios: bool,
}

pub struct CleanConfig {
    // path to BIOS file
    pub bios: Option<String>,
    // path to ROM file
    pub rom: Option<String>,
    // path to save state to load on startup
    pub state: Option<String>,
    // GUI scale factor
    pub scale: u8,
    // run in CLI mode (no GUI)
    pub cli: bool,
    // skip BIOS on startup
    pub skip_bios: bool,
}

impl RawConfig {
    pub fn clean(self) -> CleanConfig {
        let mut skip_bios = self.skip_bios;
        if self.bios.is_none() {
            skip_bios = true;
        }

        CleanConfig {
            bios: self.bios,
            rom: self.rom,
            state: self.state,
            scale: self.scale,
            cli: self.cli,
            skip_bios: skip_bios,
        }
    }
}
