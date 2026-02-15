# RustyBoi

[![Release](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml/badge.svg)](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml) [![License](https://badgen.net/github/license/USA-RedDragon/rustyboi)](https://github.com/USA-RedDragon/rustyboi/blob/main/LICENSE) [![Release](https://img.shields.io/github/release/USA-RedDragon/rustyboi.svg)](https://github.com/USA-RedDragon/rustyboi/releases/) [![codecov](https://codecov.io/gh/USA-RedDragon/rustyboi/graph/badge.svg?token=4ZnAhYzHtU)](https://codecov.io/gh/USA-RedDragon/rustyboi)

A Game Boy emulator written in Rust for learning purposes. Not usable, likely never will be. Use something else.

## Test suite accuracy

Passing cases per suite, refreshed automatically on every pull request
(`tools/run-suites.sh report`). See [SUITES.md](SUITES.md) for what each suite
is, how it is graded, and where its ROMs come from.

<!-- SUITE-PROGRESS:START -->
| Suite | Passing | Total |
| :--- | ---: | ---: |
| acid2 | 3 | 3 |
| cgb_acid_hell | 1 | 1 |
| mealybug | 39 | 51 |
| mooneye | 189 | 192 |
| mooneye_wilbertpol | 188 | 193 |
| age | 44 | 56 |
| gbmicrotest | 481 | 513 |
| samesuite_apu | 70 | 70 |
| samesuite_nonapu | 6 | 6 |
| samesuite_sgb | 2 | 2 |
| sgb | 1 | 1 |
| blargg | 15 | 15 |
| blargg_singles | 41 | 41 |
| scribbltests | 10 | 10 |
| turtle_tests | 4 | 4 |
| little_things_gb | 4 | 4 |
| bully | 2 | 2 |
| strikethrough | 2 | 2 |
| daid | 8 | 8 |
| rtc3test | 6 | 6 |
| mbc3_tester | 2 | 2 |
| cpp | 3 | 3 |
| gambatte | 5241 | 5257 |
| **Total** | **6362** | **6442** |
<!-- SUITE-PROGRESS:END -->

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

### Android (RetroArch)

The desktop `.so` is glibc/x86-64 and will **not** load on Android. Cross-compile
with the Android NDK using the helper script (needs `cargo-ndk`, the Android Rust
targets, and an NDK):

```sh
ANDROID_NDK_HOME=/path/to/ndk ./build-libretro-android.sh arm64-v8a   # or --all
```

This emits `target/libretro-android/<abi>/rustyboi_libretro_android.so` — note the
mandatory `_android` suffix and **no** `lib` prefix; RetroArch Android only loads
cores named `<name>_libretro_android.so`. The script handles the two cross-build
gotchas: a per-ABI bindgen `--target`/`--sysroot` (so 32-bit pointer/`size_t`
layouts are correct) and a host `libclang` ≤ 21 (libclang 22 mis-parses a libretro
struct; set `LIBCLANG_PATH` if it isn't auto-detected).

Install it via the in-app menu — RetroArch's Android cores live in an app-private
directory you can't push into without root:

```sh
adb push target/libretro-android/arm64-v8a/rustyboi_libretro_android.so /sdcard/Download/
```

Then **Main Menu → Load Core → Install or Restore a Core → Downloads →
`rustyboi_libretro_android.so`** (RetroArch copies it into its real cores dir,
shown under **Settings → Directory → Cores**). The `.info` is optional and not
copied by that flow; to add it, push `rustyboi_libretro.info` into the path under
**Settings → Directory → Core Info** if that path is writable.

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
