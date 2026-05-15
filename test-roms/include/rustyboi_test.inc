; rustyboi_test.inc — pass/fail signalling for the mooneye-graded ROMs.
;
; The test runner (rustyboi-test-runner, `mooneye` oracle) runs the ROM until the
; program counter reaches an `LD B,B` ($40) opcode, then checks the CPU registers
; against the Fibonacci signature B=3, C=5, D=8, E=13, H=21, L=34.
;
; A ROM must not execute any other `LD B,B` before its final verdict, or the
; runner stops early. Do not use `ld b, b` anywhere in a test body.

IF !DEF(RUSTYBOI_TEST_INC)
DEF RUSTYBOI_TEST_INC EQU 1

; Signal PASS: load the Fibonacci signature, then the `LD B,B` done-marker. The
; runner reads the registers at the marker; the trailing spin is only reached on
; real hardware / other emulators (where `LD B,B` is a no-op).
MACRO test_success
    ld b, 3
    ld c, 5
    ld d, 8
    ld e, 13
    ld h, 21
    ld l, 34
    ld b, b
.done\@:
    jr .done\@
ENDM

; Signal FAIL: non-Fibonacci sentinels, then the same done-marker.
MACRO test_failure
    ld b, $42
    ld c, $42
    ld d, $42
    ld e, $42
    ld h, $42
    ld l, $42
    ld b, b
.done\@:
    jr .done\@
ENDM

ENDC
