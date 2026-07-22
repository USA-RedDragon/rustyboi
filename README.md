# RustyBoi

[![Release](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml/badge.svg)](https://github.com/USA-RedDragon/rustyboi/actions/workflows/release.yaml) [![License](https://badgen.net/github/license/USA-RedDragon/rustyboi)](https://github.com/USA-RedDragon/rustyboi/blob/main/LICENSE) [![Release](https://img.shields.io/github/release/USA-RedDragon/rustyboi.svg)](https://github.com/USA-RedDragon/rustyboi/releases/) [![codecov](https://codecov.io/gh/USA-RedDragon/rustyboi/graph/badge.svg?token=4ZnAhYzHtU)](https://codecov.io/gh/USA-RedDragon/rustyboi)

A Game Boy emulator written in Rust for learning purposes. Not usable, likely never will be. Use something else.

## Test suite accuracy

Passing cases per suite, refreshed automatically on every pull request
(`make report`). See [SUITES.md](SUITES.md) for what each suite
is, how it is graded, and where its ROMs come from.
[KNOWN_FAILURES.md](KNOWN_FAILURES.md) adjudicates the failing set: every
gbmicrotest and gambatte failure is individually proven there; the
gbc_hw_tests failures are newly adopted and still pending per-ROM
adjudication.

<!-- SUITE-PROGRESS:START -->
| Suite | Passing | Total |
| :--- | ---: | ---: |
| rustyboi | 50 | 50 |
| acid2 | 3 | 3 |
| cgb_acid_hell | 1 | 1 |
| mealybug | 51 | 51 |
| mooneye | 193 | 193 |
| mooneye_wilbertpol | 194 | 194 |
| age | 56 | 56 |
| gbmicrotest | 509 | 512 |
| samesuite_apu | 70 | 70 |
| samesuite_nonapu | 6 | 6 |
| samesuite_sgb | 2 | 2 |
| sgb | 1 | 1 |
| blargg | 15 | 15 |
| blargg_singles | 51 | 51 |
| scribbltests | 10 | 10 |
| turtle_tests | 4 | 4 |
| little_things_gb | 4 | 4 |
| bully | 2 | 2 |
| strikethrough | 2 | 2 |
| daid | 8 | 8 |
| rtc3test | 6 | 6 |
| mbc3_tester | 2 | 2 |
| cpp | 3 | 3 |
| magentests | 11 | 11 |
| little_things_extra | 4 | 4 |
| sketchtests | 6 | 6 |
| gbc_hw_tests | 338 | 342 |
| gambatte | 5248 | 5257 |
| **Total** | **6850** | **6866** |
<!-- SUITE-PROGRESS:END -->

The table above is the **hardware-graded** regression gate. The one below is a
separate, **non-gating regression tripwire**: it grades rustyboi against
[docboy](https://github.com/Docheinstein/docboy)'s own F12 self-screenshots,
which carry no hardware provenance. A disagreement is a diff *lead* to
investigate, never a correctness verdict, so these counts live in their own
labeled sub-table with its own subtotal and are deliberately kept **out of the
hardware Total**. "Matching" is the number of frames that equal docboy's
screenshot under a screen-ever-matches scan. The references that rustyboi and
SameBoy-from-source agree docboy rendered wrong (a spurious artifact, cross-
checked against mealybug hardware) are excluded rather than asserted: 46 on DMG,
and now 23 on CGB, adjudicated against SameBoy at both CGB-C and CGB-E (a
handful of CGB-C-vs-E revision splits are left as-is, not dropped). The corpus is
provisioned automatically by `tools/run-suites.sh setup`, gated by
`tools/run-suites.sh all`, and refreshed alongside the hardware table by
`tools/run-suites.sh report-update` (whenever the corpus is present).

<!-- DOCBOY-TRIPWIRE:START -->
| Tripwire (docboy diff, non-gating) | Matching | Total |
| :--- | ---: | ---: |
| docboy_diff_dmg | 518 | 531 |
| docboy_diff_cgb | 62 | 96 |
| docboy_diff_cgb_dmg_mode | 283 | 444 |
| **Tripwire total** | **863** | **1071** |
<!-- DOCBOY-TRIPWIRE:END -->

## RetroArch / libretro core

The `rustyboi-libretro` crate builds a [libretro](https://www.libretro.com/) core
(a shared library) that can be loaded by RetroArch and other libretro frontends.
It provides video (XRGB8888), stereo audio, joypad input, and save states for
both DMG and CGB games.

### Build

The core is not part of the default workspace build, so build it explicitly with
`make libretro`. This builds every libretro target **inside the
[`rust-cross`](https://github.com/USA-RedDragon/dockers/tree/main/images/rust-cross)
container image**, which bundles all the cross toolchains (gnu + musl linkers,
llvm-mingw, osxcross, the Android NDK).

```sh
make targets                                    # the target table
make libretro TARGETS="linux-x86_64 windows-arm64"   # specific targets
make libretro TARGETS=all                       # every target
RUSTBOI_CROSS_IMAGE=… make libretro TARGETS=…   # override the image
```

### Install

Copy the built library into RetroArch's `cores/` directory, and the bundled
`rustyboi_libretro.info` into its `info/` directory so RetroArch shows the core
name, supported extensions, and capabilities. On Linux that is typically:

```sh
cp target/libretro/linux-x86_64/rustyboi_libretro.so ~/.config/retroarch/cores/
cp rustyboi-libretro/rustyboi_libretro.info ~/.config/retroarch/info/
```

The `.info` file is not required to *run* the core, but without it RetroArch
lists the core by its filename and won't associate `.gb`/`.gbc` content with it.
The library and `.info` basenames must match (`rustyboi_libretro`).

### Android (RetroArch)

The desktop `.so` is glibc/x86-64 and will **not** load on Android — build the
Android cores with the same script (they emit
`target/libretro/android-<abi>/rustyboi_libretro_android.so`, with the mandatory
`_android` suffix and **no** `lib` prefix that RetroArch Android requires):

```sh
make libretro TARGETS="android-arm64 android-armv7 android-x86_64 android-x86"
```

Install via the in-app menu — RetroArch's Android cores live in an app-private
directory you can't push into without root:

```sh
adb push target/libretro/android-arm64/rustyboi_libretro_android.so /sdcard/Download/
```

Then **Main Menu → Load Core → Install or Restore a Core → Downloads →
`rustyboi_libretro_android.so`** (RetroArch copies it into its real cores dir,
shown under **Settings → Directory → Cores**). The `.info` is optional and not
copied by that flow; to add it, push `rustyboi_libretro.info` into the path under
**Settings → Directory → Core Info** if that path is writable.

### Load

1. In RetroArch, choose **Load Core** and select the `rustyboi_libretro` core.
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
