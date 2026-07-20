# Builds the first-party test ROMs with RGBDS.
#
# Each src/<category>/<name>.<model>.<grading>.asm builds to
# build/<category>/<name>.<model>.<grading>.{gb,gbc}. The <model> token in the
# filename selects the header:
#
#   dmg        -> .gb  (no -C)
#   cgb, agb   -> .gbc (-C)
#   cgbe       -> .gbc (-C) the same CGB cart, but run on CPU-CGB-D/E silicon.
#                 For the CGB-side of a CGB revision asymmetry: the header is
#                 identical to `cgb`, only the runner's hardware pin differs.
#   sgb, sgb2  -> .gb  with the SGB header entries Pan Docs "Unlocking SGB
#                 Functions" requires: `-s` sets the SGB flag $0146 to $03 and
#                 `-l 0x33` sets the old licensee code $014B to $33. Both are
#                 needed — rgbfix(1) warns that the SGB flag "will be ignored by
#                 the SGB unless the old licensee code (-l) is 0x33".
#   sgbcart    -> .gb  the same SGB-flagged cart, but run on plain DMG hardware.
#                 For the DMG side of an SGB asymmetry: an SGB-enabled cart in a
#                 Game Boy must behave like a monochrome game. Flagging matters —
#                 an unflagged build would be gated out of SGB behaviour anyway
#                 and could not tell a correct DMG from an ungated one.
#   sgblocked  -> .gb  built for SGB hardware but deliberately WITHOUT those
#                 entries, so the SGB's command-unlock gate stays closed. Only
#                 for ROMs that assert the locked-out behaviour.
#
#   make            build all ROMs, then regenerate the runner manifest
#   make roms       build all ROMs only
#   make manifest   build all ROMs, then regenerate
#                   rustyboi-test-runner/suites/rustyboi.manifest from build/
#   make clean      remove build/
#
# Safe under `make -j`: the manifest is generated from the built ROMs, so it is
# ordered after `roms` rather than run alongside it.

RGBASM  := rgbasm
RGBLINK := rgblink
RGBFIX  := rgbfix
PYTHON  := python3
INC     := include

SRCS := $(shell find src -name '*.asm')

.PHONY: all roms manifest clean
all: roms manifest

roms:
	@set -e; for src in $(SRCS); do \
	  rel=$${src#src/}; base=$${rel%.asm}; \
	  model=$$(echo "$$base" | awk -F. '{print $$(NF-1)}'); \
	  case "$$model" in \
	    dmg)       ext=gb;  fixflags="-v -p 0xFF" ;; \
	    cgb|cgbe|agb) ext=gbc; fixflags="-v -C -p 0xFF" ;; \
	    sgb|sgb2|sgbcart) ext=gb; fixflags="-v -s -l 0x33 -p 0xFF" ;; \
	    sgblocked) ext=gb;  fixflags="-v -p 0xFF" ;; \
	    *) echo "unknown model '$$model' in $$src (expected dmg|cgb|cgbe|agb|sgb|sgb2|sgbcart|sgblocked)"; exit 1 ;; \
	  esac; \
	  out=build/$$base.$$ext; \
	  mkdir -p "$$(dirname "$$out")"; \
	  echo "  RGBDS $$out"; \
	  $(RGBASM) -i $(INC)/ -o "$$out.o" "$$src"; \
	  $(RGBLINK) -o "$$out" "$$out.o"; \
	  $(RGBFIX) $$fixflags "$$out"; \
	  rm -f "$$out.o"; \
	done

# Order-only on `roms`: the generator scans build/ and emits one manifest line
# per ROM found, so under `make -j` it would otherwise race the ROM build and
# silently write a manifest listing only whatever had been linked so far.
manifest: | roms
	@cd .. && $(PYTHON) tools/gen_manifests.py --only rustyboi

clean:
	rm -rf build
