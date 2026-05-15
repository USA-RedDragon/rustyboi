# Builds the first-party test ROMs with RGBDS.
#
# Each src/<category>/<name>.<model>.<grading>.asm builds to
# build/<category>/<name>.<model>.<grading>.{gb,gbc}. The <model> token in the
# filename selects the header: dmg -> .gb (no -C); cgb/agb -> .gbc (-C).
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
	    dmg)     ext=gb;  fixflags="-v -p 0xFF" ;; \
	    cgb|agb) ext=gbc; fixflags="-v -C -p 0xFF" ;; \
	    *) echo "unknown model '$$model' in $$src (expected dmg|cgb|agb)"; exit 1 ;; \
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
