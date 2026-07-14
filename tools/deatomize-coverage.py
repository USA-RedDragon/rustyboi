#!/usr/bin/env python3
"""De-atomize LLVM coverage counters in an instrumented ELF binary.

rustc hardcodes `Options.Atomic = true` for -Cinstrument-coverage counters
(compiler/rustc_llvm/llvm-wrapper/PassWrapper.cpp), emitting `lock incq` for
every counter hit. On rustyboi's cycle-accurate hot loop that 20-cycle
serializing RMW measures 15-21x wall (IPC 4.9 -> 0.4); plain increments are the
pre-atomic LLVM behavior and lose at most redundant counts on concurrently-hit
(i.e. already-counted) lines, which line coverage doesn't observe.

Rewrites `lock incq disp32(%rip)` (F0 48 FF 05 d32, 9 bytes) to
`nop; incq disp32(%rip)` (90 48 FF 05 d32) ONLY for instructions objdump
decodes at that address whose rip-relative target lands inside
__llvm_prf_cnts, so genuine application atomics are untouched. Same length,
same instruction start -> no jump target can break.

Usage: deatomize-coverage.py <elf-binary> [more binaries...]
"""

import re
import subprocess
import sys


def section_ranges(path):
    """{name: (vaddr, size, file_off)} from readelf -SW."""
    out = subprocess.run(
        ["readelf", "-SW", path], capture_output=True, text=True, check=True
    ).stdout
    secs = {}
    for line in out.splitlines():
        if "]" not in line:
            continue
        rest = line.split("]", 1)[1].split()
        if len(rest) >= 5 and rest[1] in ("PROGBITS", "NOBITS"):
            secs[rest[0]] = (int(rest[2], 16), int(rest[4], 16), int(rest[3], 16))
    return secs


def counter_lock_incs(path, cnts_va, cnts_end):
    """VAs of `lock incq <rip-rel>` instructions targeting __llvm_prf_cnts."""
    out = subprocess.run(
        ["objdump", "-d", "--section=.text", path],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    # "  362ff:  f0 48 ff 05 79 91 0c   lock incq 0xc9179(%rip)  # ff480 <...>"
    # (objdump may wrap trailing insn bytes to a continuation line, so only the
    # F0 48 FF 05 prefix+opcode+modrm is matched; modrm 05 = rip-relative.)
    rx = re.compile(
        r"^\s*([0-9a-f]+):\s+f0 48 ff 05 .*lock\s+incq\s+0x[0-9a-f]+\(%rip\)"
        r"\s*#\s*([0-9a-f]+)"
    )
    for line in out.splitlines():
        m = rx.match(line)
        if m and cnts_va <= int(m.group(2), 16) < cnts_end:
            yield int(m.group(1), 16)


def patch(path):
    secs = section_ranges(path)
    if "__llvm_prf_cnts" not in secs:
        print(f"{path}: no __llvm_prf_cnts section (not coverage-instrumented?)")
        return 0
    cnts_va, cnts_size, _ = secs["__llvm_prf_cnts"]
    text_va, _, text_off = secs[".text"]

    with open(path, "rb") as f:
        data = bytearray(f.read())

    patched = 0
    for va in counter_lock_incs(path, cnts_va, cnts_va + cnts_size):
        off = va - text_va + text_off
        assert data[off] == 0xF0, f"expected F0 lock prefix at {va:#x}"
        data[off] = 0x90
        patched += 1

    with open(path, "wb") as f:
        f.write(data)
    print(f"{path}: de-atomized {patched} coverage counter increments")
    return patched


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    for p in sys.argv[1:]:
        if "__llvm_prf_cnts" not in section_ranges(p):
            sys.exit(f"{p}: not coverage-instrumented (no __llvm_prf_cnts)")
        # 0 patched is fine: a cache-restored binary is already de-atomized.
        patch(p)
