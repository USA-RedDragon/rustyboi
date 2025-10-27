# RustyBoi

[![Release](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml/badge.svg)](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml) [![License](https://badgen.net/github/license/USA-RedDragon/rustyboi)](https://github.com/USA-RedDragon/rustyboi/blob/main/LICENSE) [![Release](https://img.shields.io/github/release/USA-RedDragon/rustyboi.svg)](https://github.com/USA-RedDragon/rustyboi/releases/) [![codecov](https://codecov.io/gh/USA-RedDragon/rustyboi/graph/badge.svg?token=4ZnAhYzHtU)](https://codecov.io/gh/USA-RedDragon/rustyboi)

A Game Boy emulator written in Rust for learning purposes. Not usable, likely never will be. Use something else.

## RetroArch / libretro core

The `rustyboi-libretro` crate builds a [libretro](https://www.libretro.com/) core
(a shared library) that can be loaded by RetroArch and other libretro frontends.
It provides video (XRGB8888), stereo audio, joypad input, and save states for
both DMG and CGB games.

### Build

The core is not part of the default workspace build, so build it explicitly:

```sh
cargo build -p rustyboi-libretro --release
```

> Note: `rust-libretro` generates its FFI bindings with bindgen, which needs
> libclang. libclang 22 mis-parses one libretro struct and the build will fail
> with a `transmute between types of different sizes` error. If you hit that,
> point bindgen at an older libclang (21 or earlier works):
>
> ```sh
> LIBCLANG_PATH=/usr/lib/llvm21/lib cargo build -p rustyboi-libretro --release
> ```

The resulting core lands at:

- Linux: `target/release/librustyboi_libretro.so`
- macOS: `target/release/librustyboi_libretro.dylib`
- Windows: `target/release/rustyboi_libretro.dll`

### Install

Copy the built library into RetroArch's `cores/` directory, and the bundled
`rustyboi_libretro.info` into its `info/` directory so RetroArch shows the core
name, supported extensions, and capabilities. On Linux that is typically:

```sh
cp target/release/librustyboi_libretro.so ~/.config/retroarch/cores/
cp rustyboi-libretro/rustyboi_libretro.info ~/.config/retroarch/info/
```

The `.info` file is not required to *run* the core, but without it RetroArch
lists the core by its filename and won't associate `.gb`/`.gbc` content with it.
The library and `.info` basenames must match (`rustyboi_libretro`).

### Load

1. In RetroArch, choose **Load Core** and select `librustyboi_libretro.so`.
2. Choose **Load Content** and pick a `.gb`, `.gbc`, or zipped ROM.

Under **Quick Menu > Options** you can set the hardware model
(Auto / Game Boy Color / Game Boy DMG; Auto selects CGB unless the ROM header
marks it DMG-only), the DMG palette (Grayscale / Green / Game Boy Pocket), and
the GBC colour correction (Linear / Gambatte).

### Supported features

- **Video / audio / input** — XRGB8888 video, stereo audio, joypad input.
- **Save states** — RetroArch save/load state (bincode-serialized).
- **Battery saves (SRAM)** — `RETRO_MEMORY_SAVE_RAM`; RetroArch persists the
  cartridge's battery RAM to a `.srm` file. The core never writes its own
  sidecar save when run under libretro, so RetroArch owns persistence.
- **RTC** — MBC3 real-time-clock registers via `RETRO_MEMORY_RTC` (`.rtc`).
- **Cheats** — Game Genie (`AAA-BBB[-CCC]`, applied as ROM patches) and
  GameShark (`ABCDGHIJ`, applied as RAM pokes each frame).
- **Memory maps** — WRAM, HRAM, VRAM and cartridge SRAM are exposed via
  `SET_MEMORY_MAPS`, enabling RetroAchievements and RAM tools.
- **Rumble** — MBC5 rumble cartridges drive the frontend rumble motor.
- **Palette / colour options** — DMG palette presets and a GBC colour
  correction mode (wired to the core's CGB colour conversion).

### Limitations

- **Sensors** (MBC7 accelerometer, Boktai light sensor) and the **microphone**
  are unsupported: the core has no sensor/mic input path.
- **Link cable / multiplayer subsystems** are out of scope; the core has no
  serial-link networking.
- Disk control and hardware (GL) rendering are not applicable to the Game Boy.
