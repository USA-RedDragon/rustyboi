# Third-Party Notices

rustyboi is licensed under the MIT License (see `LICENSE`). It bundles or
derives from the third-party material listed below.

## Bundled data

### No-Intro game-name index (`rustyboi-session/data/no_intro.bin`)

A compact CRC32 → game-name index used only to identify a loaded ROM for
display and for locating its cheat file. Built by `tools/gen-nointro.py` from
the No-Intro "Nintendo - Game Boy" and "Nintendo - Game Boy Color" DAT files as
mirrored in the [libretro-database](https://github.com/libretro/libretro-database)
project (`metadat/no-intro/`). The index contains factual checksum→title
associations only. Credit to the No-Intro project and the libretro-database
maintainers.

## Hardware constants and de-facto standard formats

Some constants are fixed properties of the Game Boy / Game Boy Color hardware or
of physical accessories, and are used as factual data regardless of where they
were first documented. These include CGB power-on OBJ palette RAM contents, the
Rocket Games mapper XOR mask, unlicensed-mapper bank-reorder tables, and the
Game Genie / GameShark cheat-code bit layouts. Reference emulators (Gambatte,
mGBA, hhugboy, SameBoy) are cited in source comments where they document the
same hardware behavior; those citations are behavioral references, not a claim
that their code was copied.

## Nintendo boot ROM data

The CGB DMG-compatibility palette tables (`rustyboi-core/src/cgb_compat_palette.rs`)
are extracted from Nintendo's copyrighted CGB boot ROM and reproduce its
per-title colorization behavior. Nintendo boot ROMs themselves are **not**
distributed with rustyboi; the user must supply their own.
