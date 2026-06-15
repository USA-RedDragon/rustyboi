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

Copy the built library into RetroArch's `cores/` directory. On Linux that is
typically:

```sh
cp target/release/librustyboi_libretro.so ~/.config/retroarch/cores/
```

A matching `rustyboi_libretro.info` file (metadata shown in RetroArch's menus)
is optional and not required to run the core.

### Load

1. In RetroArch, choose **Load Core** and select `librustyboi_libretro.so`.
2. Choose **Load Content** and pick a `.gb`, `.gbc`, or zipped ROM.

Under **Quick Menu > Options** you can set the hardware model
(Auto / Game Boy Color / Game Boy DMG). Auto selects CGB unless the ROM header
marks it DMG-only. Save states (RetroArch's save/load state) are supported.
