# Builds the first-party test ROMs with RGBDS.
#
# Each src/<category>/<name>.<model>.<grading>.asm builds to
# build/<category>/<name>.<model>.<grading>.{gb,gbc}. The <model> token in the
# filename selects the header:
#
#   dmg        -> .gb  (no -C)
#   cgb, agb   -> .gbc (-C)
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
#   make manifest   regenerate rustyboi-test-runner/suites/rustyboi.manifest
#   make clean      remove build/

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
	    cgb|agb)   ext=gbc; fixflags="-v -C -p 0xFF" ;; \
	    sgb|sgb2|sgbcart) ext=gb; fixflags="-v -s -l 0x33 -p 0xFF" ;; \
	    sgblocked) ext=gb;  fixflags="-v -p 0xFF" ;; \
	    *) echo "unknown model '$$model' in $$src (expected dmg|cgb|agb|sgb|sgb2|sgbcart|sgblocked)"; exit 1 ;; \
	  esac; \
	  out=build/$$base.$$ext; \
	  mkdir -p "$$(dirname "$$out")"; \
	  echo "  RGBDS $$out"; \
	  $(RGBASM) -i $(INC)/ -o "$$out.o" "$$src"; \
	  $(RGBLINK) -o "$$out" "$$out.o"; \
	  $(RGBFIX) $$fixflags "$$out"; \
	  rm -f "$$out.o"; \
	done

manifest:
	@cd .. && $(PYTHON) tools/gen_manifests.py --only rustyboi

clean:
	rm -rf build
