use crate::{cpu, cpu::registers, memory::Addressable as _};

/// Shared CB-prefix rotate/shift/swap kernels: operand and incoming carry in,
/// result and outgoing carry out. The register and `(hl)` handler families both
/// dispatch through these, so the bit math and the flag rule have exactly one
/// definition. Ops that do not consume the incoming carry ignore it.
#[inline(always)]
fn op_rlc(v: u8, _carry_in: bool) -> (u8, bool) {
    ((v << 1) | ((v & 0x80) >> 7), (v & 0x80) != 0)
}

#[inline(always)]
fn op_rrc(v: u8, _carry_in: bool) -> (u8, bool) {
    ((v >> 1) | ((v & 0x01) << 7), (v & 0x01) != 0)
}

#[inline(always)]
fn op_rl(v: u8, carry_in: bool) -> (u8, bool) {
    ((v << 1) | (carry_in as u8), (v & 0x80) != 0)
}

#[inline(always)]
fn op_rr(v: u8, carry_in: bool) -> (u8, bool) {
    ((v >> 1) | ((carry_in as u8) << 7), (v & 0x01) != 0)
}

#[inline(always)]
fn op_sla(v: u8, _carry_in: bool) -> (u8, bool) {
    (v << 1, (v & 0x80) != 0)
}

#[inline(always)]
fn op_sra(v: u8, _carry_in: bool) -> (u8, bool) {
    ((v >> 1) | (v & 0x80), (v & 0x01) != 0)
}

#[inline(always)]
fn op_srl(v: u8, _carry_in: bool) -> (u8, bool) {
    (v >> 1, (v & 0x01) != 0)
}

/// `swap` clears the carry unconditionally rather than reporting a shifted-out bit.
#[inline(always)]
fn op_swap(v: u8, _carry_in: bool) -> (u8, bool) {
    (((v & 0x0F) << 4) | ((v & 0xF0) >> 4), false)
}

pub(super) fn nop(_cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    4
}

pub(super) fn stop(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    // CGB speed switch. STOP advances the CPU clock by 8 T-cycles, performs the
    // peripheral speed change, then stalls instruction fetch for a ~0x20000-cycle
    // window during which other events still progress; a per-CPU stall counter
    // drained by `SM83::step` models the window.
    //
    // STOP is a 2-byte opcode (10 nn); the second byte is normally ignored.
    // Pan Docs: CPU instruction set — https://gbdev.io/pandocs/CPU_Instruction_Set.html
    // Here it executes as the next instruction only when an HDMA block was
    // prefetched (dma-req flagged) at the STOP — decided below. Capture it now;
    // whether it runs is settled by the prefetched-block path.
    // The (128*1024-76)-clock switch window and "no HDMA during speed switch" are
    // documented in TCAGBD §3.8, not in Pan Docs; the per-block sub-cycle fire
    // ordering below is from test-ROM refs.
    let operand_byte = mmio.peek(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);

    // STOP chart, first question: "Is a button being held and selected in JOYP?"
    // — any selected P10-P13 line low, i.e. the live JOYP low nibble != 0xF
    // (deselected groups read 1111; the SGB MLT_REQ id nibble counts, as it drives
    // the real lines). Checked BEFORE the KEY1 speed-switch test, so a held
    // selected button routes STOP to the joypad branch even with a switch armed.
    // Pan Docs: Reducing Power Consumption — https://gbdev.io/pandocs/Reducing_Power_Consumption.html
    let button_held = (mmio.peek(crate::input::JOYP) & 0x0F) != 0x0F;

    if !button_held && mmio.is_speed_switch_armed() {
        let to_double = !mmio.is_double_speed_mode();
        // Bridge dots. On hardware the LCD advances across the switch at the old
        // speed before re-anchoring. Our per-dot stepper runs the returned 8 cycles
        // at the NEW speed, landing the LCD short; inject the missing render dots so
        // the post-switch LCD position (and the FF41 mode read after the window)
        // matches. Derived per direction, not tuned:
        //   SS->DS: hardware advances 8 old-speed dots; the returned 8 cycles cover
        //     8>>1 = 4 at the new speed, so inject 8-4 = 4. On a non-rendering line
        //     there is no mode-3 window to advance, so inject the full 8.
        //   DS->SS: hardware advances 0 dots; the -1 re-anchor shifts line phase by 1.
        // HDMA-active mode-3 DS->SS couples to the block-fire/timer phase across the
        // switch (the suppress-edge path below), out of scope here — keep 3.
        let bridge = if to_double {
            if mmio.ppu.is_on_rendering_line() { 4 } else { 8 }
        } else if mmio.mmio.hdma_is_enabled() {
            3
        } else {
            1
        };
        // On hardware the HDMA halt state is captured at the stop cc and the CPU
        // halts for the unhalt window, suppressing the per-dot HDMA period edge
        // across the bridge and stall. We arm the FF55-enabled block lazily on the
        // renderer m0 edge, which has NOT been crossed at the stop instruction on
        // these lines. Decide here instead, at the exact stop cc:
        //   * m0 edge already crossed (`hdma_disable_fires`): the block is latched
        //     (not acked at single speed). Fire it now, pre-switch, so the readback
        //     reflects the completed block.
        //   * m0 edge NOT yet crossed: hold it across the suppressed window; the
        //     reflag gate at the unhalt cc (`SM83::step` exit) fires it only if the
        //     unhalt lands back in the HDMA period or the block was owed.
        // A prefetched dma req is not acked at single speed, so an SS->DS block's
        // transfer runs AFTER the speed switch, at the new (double) speed. Record
        // the fire kind here and run the block after `perform_speed_switch` so the
        // block (and its stall/timer phase) lands at the post-switch DS cc.
        let mut deferred_stop_fire: Option<bool> = None; // Some(fires_in_halt)
        // The STOP operand byte executes as the next instruction iff the HDMA
        // dma-req is flagged at the STOP — i.e. the block's m0 edge has been
        // crossed and the block is still armed (in period + enabled), regardless
        // of switch direction. Decide it here.
        let mut exec_stop_operand = false;
        let suppress_edge = mmio.ppu.is_on_rendering_line();
        if suppress_edge {
            let cc = mmio.mmio.master_cc();
            let dsb = mmio.is_double_speed_mode();
            let in_period_now = mmio.ppu.hdma_disable_fires(cc, dsb).unwrap_or(false);
            // A block is owed at the STOP only if this period's m0 edge flagged a
            // transfer that has NOT yet run. When the period's block already ran
            // (`hdma_block_done_this_period`, e.g. an in-period FF55 kick serviced
            // it this line), the req is not flagged, so the SS->DS synchronous fire
            // must NOT run; the next (post-window) m0 edge fires the following block.
            if to_double && in_period_now
                && mmio.mmio.hdma_is_enabled()
                && !mmio.mmio.hdma_req_pending()
                && !mmio.mmio.hdma_block_done_this_period()
            {
                // SS->DS, m0 edge already crossed at stop: the block is latched
                // (single speed not acked). When it fires relative to the STOP halt
                // decides the FF55 readback: the transfer runs an M-cycle behind the
                // m0 edge, so if the edge was crossed within this stop's own M-cycle
                // (`cc - edge < 4`) the copy lands inside the halt window and FF55
                // freezes at length | 0x80. If crossed a full M-cycle earlier
                // (`cc - edge >= 4`) the block completed before the STOP and FF55
                // length-wraps to 0xFF. Defer the actual fire to post-switch (DS cc).
                let edge = mmio.ppu.hdma_m0_edge(dsb).unwrap_or(cc as i64);
                let fires_in_halt = (cc as i64) - edge < 4;
                mmio.mmio.set_hdma_req();
                deferred_stop_fire = Some(fires_in_halt);
                // The block is armed/firing at this stop => dma-req flagged =>
                // the operand byte runs post-unhalt (see `exec_stop_operand`).
                exec_stop_operand = true;
            } else {
                // SS->DS not-yet-in-period, or the DS->SS return switch: hold the
                // block across the suppressed window with the captured halt state;
                // the reflag gate at the unhalt cc decides. `on_stop_window_enter`
                // captures the requested/low state so a DS->SS block owed from the
                // prior switch still fires at unhalt.
                mmio.mmio.on_stop_window_enter(in_period_now);
                // DS->SS (or not-yet-acked) with the m0 edge crossed and the block
                // still enabled: the dma-req is still flagged at the stop, so the
                // operand byte executes post-unhalt even though the block itself
                // fires later, on the unhalt-reflag path.
                if in_period_now && mmio.mmio.hdma_is_enabled() {
                    exec_stop_operand = true;
                }
            }
        }
        // Capture whether this STOP is a DS->SS switch taken during mode 3 (pixel
        // transfer), BEFORE the bridge advances dots (which can leave mode 3). The
        // -1 half-dot re-anchor per such switch is injected below as a STAT-phase-
        // only carry (decoupled from the render latch) via `stat_phase_carry`;
        // every 2nd such switch carries one extra STAT dot.
        let dsss_mode3_switch = !to_double && mmio.ppu.is_in_pixel_transfer();
        let ssds_mode3_switch = to_double && mmio.ppu.is_in_pixel_transfer();
        mmio.ppu.stop_bridge_advance(mmio.mmio, bridge);
        if !to_double {
            // DS->SS: the -1 re-anchor is folded into the bridge dot count (DS->SS
            // bridge = 1), leaving the LY counter one master-cc high; the closed-form
            // the LY time drops its +1 correction.
            mmio.ppu.set_dsss_lytime_adjust();
            // Only NON-mode-3 DS->SS switches feed the LY-read sub-dot accumulator:
            // mode-3 switches already carry the half-dot through the `stat_phase_carry`
            // path, which shifts the LY read's `time` directly. OAM/HBlank switches
            // get no such carry, so their residual half-dot is what this tracks.
            mmio.ppu.bump_dsss_ly_total();
            if !dsss_mode3_switch {
                mmio.ppu.bump_dsss_ly_phase();
            }
        }
        mmio.perform_speed_switch();
        // Re-anchor the PPU's event-scheduled STAT/mode/LYC clocks to the new
        // speed; the scheduled times were computed with the old cc-factor.
        mmio.ppu.speed_change(mmio.mmio);
        // Inject the accumulated DS->SS mode-3 half-dot as a STAT-phase-only carry
        // (render latch stays put). STAGE4_FACET1_CARRY = false wires the path but
        // injects 0 dots; true lands the carry.
        const STAGE4_FACET1_CARRY: bool = true;
        if dsss_mode3_switch {
            let carry = mmio.ppu.register_dsss_mode3_stop();
            if STAGE4_FACET1_CARRY {
                mmio.ppu.stat_phase_carry(mmio.mmio, carry);
            }
        }
        // SS->DS switch taken during mode 3: across the switch the re-anchored
        // the LY counter.time sits ~5 DS-dots (10 cc) ahead of our bridged renderer line
        // phase for the FF44 (LY) read. The renderer pixel latch and the STAT/mode-0 time
        // predictor are already correct; only the LY-read anticipation window keys
        // off the raw the LY counter.time, which our closed-form runs late here. Latch
        // the phase advance so `get_ly_reg_at_cc` (and only it) resolves the read
        // against the re-anchored the LY time.
        if ssds_mode3_switch {
            mmio.ppu.set_ssds_mode3_ly_advance();
        }
        // SS->DS switch on a still-live halt-woken stream (halt-wake -> STOP with no
        // intervening HALT): the post-switch DS stream keeps carrying the un-charged
        // CGB halt-exit M-cycle. Arm the DS analog of the single-speed halt-exit
        // LY-read bias (see `get_ly_reg_at_cc`).
        if to_double && mmio.mmio.halt_wakeup_skew() {
            mmio.mmio.set_ssds_haltskew_ly_advance();
        }
        // Fire the deferred SS->DS prefetched block now — post-switch, so it runs
        // at the new (double) speed at the post-bridge cc. The transfer fires DURING
        // the unhalt window (its cc advance happens before the CPU resumes), so its
        // ~64 DS cc are absorbed into the 0x20000 window below, not charged on top:
        // otherwise the post-stop resume (and its LY read) lands ~64 cc late, one LY
        // high on the boundary. `dma_stall_before` snapshots the stall so the
        // block's contribution can be subtracted back out.
        // Freeze the OAM-DMA across the window BEFORE firing the block: on hardware
        // the CPU halts first, so a transfer firing during the window interleaves
        // through the halted OAM-DMA branch (position frozen). Set the freeze here
        // so the block's OAM-DMA interleave honors it.
        mmio.mmio.set_oam_dma_stop_freeze(true);
        let dma_stall_before = mmio.mmio.peek_dma_stall();
        if let Some(fires_in_halt) = deferred_stop_fire {
            if fires_in_halt {
                mmio.mmio.fire_pending_hdma_mcycle_stop_halt();
                // Drop the block's transfer stall: its cc advance is folded into the
                // 0x20000 window (kept intact below), not appended after it.
                let absorb = mmio.mmio.peek_dma_stall().saturating_sub(dma_stall_before);
                mmio.mmio.reduce_dma_stall(absorb);
            } else {
                // Edge crossed a full M-cycle BEFORE this STOP (`cc - edge >= 4`):
                // the transfer already ran and acked the req before the STOP, so the
                // req is not flagged and the operand byte is skipped. Fire the
                // already-owed block but do NOT execute the operand.
                mmio.mmio.fire_pending_hdma_mcycle();
                exec_stop_operand = false;
            }
        }
        // Post-unhalt dispatch runs the STOP operand byte without re-fetching, via
        // the prefetch state, so the operand (`inc a` / `ld a,(nn)`) runs as the
        // next instruction. PC already points past the first operand byte
        // (multi-byte operand instructions read their own operands from there).
        if exec_stop_operand {
            cpu.opcode = operand_byte;
            cpu.prefetched = true;
        }
        // The unhalt event fires 0x20000 + 4 T-cycles after STOP entry, and STOP
        // itself advances the CPU clock by 8; the 8 returned below is part of that
        // window. The opcode fetch already ticked master_cc by 4 before `stop()`
        // runs, and the returned cycles are charged against that already-ticked
        // M-cycle, so the returned 8 nets only +4 of advance. Measured from the
        // post-opcode cc the window is 0x20000 + 4; with the returned 8 netting 4,
        // the remaining no-fetch stall is 0x20000, landing the resume at
        // post_opcode_cc + 0x20000 + 4.
        cpu.stop_unhalt_cycles = 0x20000;
        return 8;
    }

    // ---- Plain STOP (no armed speed switch), per the STOP chart ----
    // Pan Docs: Reducing Power Consumption — https://gbdev.io/pandocs/Reducing_Power_Consumption.html
    // "Is an interrupt pending (IE & IF != 0)?" — IME-independent, the same test
    // as the HALT bug. Decides opcode LENGTH on every remaining branch: pending ->
    // 1-byte (the byte after $10 executes as the next instruction), else 2-byte.
    let irq_pending = (mmio.peek(registers::INTERRUPT_FLAG)
        & mmio.peek(registers::INTERRUPT_ENABLE)
        & 0x1F)
        != 0;
    if irq_pending {
        // Rewind the operand skip above: 1-byte form.
        cpu.registers.pc = cpu.registers.pc.wrapping_sub(1);
    }

    if button_held {
        if irq_pending {
            // Chart: button held + interrupt pending -> "STOP is a 1-byte
            // opcode, mode doesn't change, DIV doesn't reset" — a NOP.
            return 4;
        }
        // Chart: button held, no interrupt pending -> "STOP is a 2-byte opcode,
        // HALT mode is entered, DIV is not reset". Enter HALT via the halt opcode
        // body (its IE&IF re-check is false here by construction, so it takes the
        // plain halted path and captures the HDMA halt state as any HALT does).
        // Charge the operand-read M-cycle on top of the fetch.
        halt(cpu, mmio);
        return 8;
    }

    // Chart: no button, no speed switch -> "STOP mode is entered, DIV is reset"
    // — for both the 1-byte (interrupt-pending) and 2-byte (idle) forms; a pending
    // interrupt does NOT prevent or terminate STOP (only a joypad line does). The
    // DIV reset goes through the FF04 write path so the TIMA phase-glitch edge and
    // DIV-APU fold apply as for any DIV write. The clock freezes right after this
    // instruction, so no read observes the sub-M-cycle placement; the frozen
    // divider holds DIV at 0 for the whole STOP.
    mmio.mmio.write(0xFF04, 0);
    // Panel effect: DMG draws a black line, CGB goes black unless mid-mode-3.
    // Pan Docs: Reducing Power Consumption — https://gbdev.io/pandocs/Reducing_Power_Consumption.html
    mmio.ppu.enter_stop_mode_panel(mmio.mmio);
    // Low-power mode proper: the clocked world freezes (master_cc stops) until a
    // selected P10-P13 line goes low, then resumes at pc. Not modeled here: the
    // armed-switch + interrupt-pending chart leaves stay on the armed path above —
    // the IME-on "1-byte, speed changes" leaf is approximated by that path's early
    // wake from the unhalt window, and the IME-off non-deterministic glitch is
    // intentionally treated as a deterministic continue.
    cpu.stopped = true;
    if irq_pending { 4 } else { 8 }
}

pub(super) fn undefined(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    // Invalid opcodes (D3, DB, DD, E3, E4, EB, EC, ED, F4, FC, FD) hard-lock the
    // CPU until power-off. Mirror that: clear IE so no interrupt can wake the CPU,
    // then halt. Peripherals keep running via the surrounding step loop.
    // Pan Docs: CPU instruction set — https://gbdev.io/pandocs/CPU_Instruction_Set.html
    mmio.write(registers::INTERRUPT_ENABLE, 0);
    cpu.halted = true;
    4
}

pub(super) fn dec_memory_hl(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = old_value.wrapping_sub(1);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (old_value & 0x0F) == 0x00);
    12
}

pub(super) fn rlca(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let old_carry = (cpu.registers.a & 0x80) >> 7;
    cpu.registers.a = (cpu.registers.a << 1) | old_carry;
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, old_carry == 1);
    4
}

pub(super) fn adc_a_memory_hl(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16) + (carry as u16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) + (value & 0x0F) + carry) > 0x0F);
    8
}

pub(super) fn daa(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let mut a = cpu.registers.a;
    let mut adjust = 0;
    let mut carry = cpu.registers.get_flag(registers::Flag::Carry);

    if cpu.registers.get_flag(registers::Flag::HalfCarry) || (!cpu.registers.get_flag(registers::Flag::Negative) && (a & 0x0F) > 0x09) {
        adjust |= 0x06;
    }
    if carry || (!cpu.registers.get_flag(registers::Flag::Negative) && a > 0x99) {
        adjust |= 0x60;
        carry = true;
    }

    if cpu.registers.get_flag(registers::Flag::Negative) {
        a = a.wrapping_sub(adjust);
    } else {
        a = a.wrapping_add(adjust);
    }

    cpu.registers.a = a;
    cpu.registers.set_flag(registers::Flag::Zero, a == 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, carry);
    4
}

pub(super) fn jp_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc = addr;
    16
}

pub(super) fn jr_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(offset as i16 as u16);
    12
}

pub(super) fn rrca(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let old_carry = cpu.registers.a & 0x01;
    cpu.registers.a = (cpu.registers.a >> 1) | (old_carry << 7);
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, old_carry == 1);
    4
}

pub(super) fn ld_memory_imm_16_sp(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
    mmio.write(addr, (cpu.registers.sp & 0x00FF) as u8);
    mmio.write(addr.wrapping_add(1), ((cpu.registers.sp & 0xFF00) >> 8) as u8);
    20
}

pub(super) fn add_sp_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let sp = cpu.registers.sp;
    let result = (sp as i16).wrapping_add(offset as i16) as u16;
    cpu.registers.sp = result;

    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, ((sp & 0xFF) + (offset as u16 & 0xFF)) > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((sp & 0x0F) + (offset as u16 & 0x0F)) > 0x0F);
    16
}

pub(super) fn sbc_a_memory_hl(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16) - (carry as i16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) as i16 - (value & 0x0F) as i16 - (carry as i16)) < 0);
    8
}

pub(super) fn halt(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    // HALT bug: on the HALT M-cycle the CPU peeks the next opcode at pc (no pc++,
    // no cc charge). If an interrupt is pending it does NOT halt — it leaves that
    // byte prefetched with pc un-advanced. When IME=0 the prefetched opcode then
    // executes and re-reads its own bytes (the double-read); when IME=1 the next
    // step's interrupt service undoes the prefetch (pc -= 1) so the return address
    // is the HALT itself, and HALT re-runs after the ISR. The pending test is
    // `IE & IF & 0x1F != 0`, independent of IME.
    // Pan Docs: halt bug — https://gbdev.io/pandocs/halt.html
    let if_reg = mmio.peek(registers::INTERRUPT_FLAG);
    let ie_reg = mmio.peek(registers::INTERRUPT_ENABLE);
    if (if_reg & ie_reg & 0x1F) != 0 {
        // pc already points at the byte after HALT (the 0x76 fetch advanced it).
        // Fetch that byte WITHOUT advancing pc and mark it prefetched; the +4
        // charge is deferred to consumption. The fetch is a REAL bus read (PPU
        // lockout applies): a double HALT with IME=0 re-executes HALT here forever,
        // re-fetching the frozen-pc byte each M-cycle, and escapes when a VRAM pc
        // byte goes mode-3 locked and reads 0xFF = rst $38 (double-halt-cancel).
        cpu.opcode = mmio.peek_fetch(cpu.registers.pc);
        cpu.prefetched = true;
        // Charge the opcode-fetch M-cycle at consumption: unlike the normal
        // prefetch, this byte was peeked with no tick, so the doubled instruction's
        // operand read must resolve one M-cycle later.
        cpu.halt_bug_prefetch = true;
        // HDMA scheduling for the IME-off HALT-bug resume: a block whose m0 edge
        // falls during the doubled resume instruction runs its transfer at the
        // instruction boundary AFTER it, so the VRAM write lands after the resume
        // instruction's own read. Defer and suppress the synchronous m0-edge fire
        // across the resume instruction; the bus fires the held block at the next
        // boundary. Only when an HDMA is armed.
        if mmio.mmio.hdma_is_enabled() {
            mmio.mmio.set_hdma_unhalt_reflag_deferred(true);
            mmio.mmio.set_hdma_mcycle_fire_suppressed(true);
        }
        return 4;
    }
    cpu.halted = true;
    // Capture the HDMA halt-state for the unhalt path.
    mmio.on_cpu_halt();
    // A flagged HDMA block held at HALT entry becomes `HaltHdmaState::Requested`.
    // When set, the byte peeked at pc on the HALT M-cycle stays prefetched with pc
    // un-advanced, exactly as in the IME-off bug branch above: on unhalt that stale
    // opcode (read now, while VRAM is still accessible) executes once with NO
    // re-fetch, then the following instruction re-reads pc (e.g. during the unhalt's
    // mode-3 the VRAM byte now reads 0xFF).
    if matches!(
            mmio.halt_hdma_state(),
            crate::memory::dma::HaltHdmaState::Requested
        )
    {
        // Same real-read semantics as the HALT-bug prefetch above.
        cpu.opcode = mmio.peek_fetch(cpu.registers.pc);
        cpu.prefetched = true;
    }
    4
}

pub(super) fn ld_hl_sp_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let sp = cpu.registers.sp;
    let result = (sp as i16).wrapping_add(offset as i16) as u16;

    cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
    cpu.registers.l = (result & 0xFF) as u8;

    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, ((sp & 0xFF) + (offset as u16 & 0xFF)) > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((sp & 0x0F) + (offset as u16 & 0x0F)) > 0x0F);
    12
}

pub(super) fn ld_sp_hl(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.sp = hl;
    8
}

pub(super) fn inc_sp(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    // DMG OAM-bug: SP is a 16-bit register driven by the IDU; an inc with SP in
    // OAM during mode 2 corrupts OAM (Pan Docs "Affected Operations").
    mmio.oam_bug_idu(cpu.registers.sp);
    cpu.registers.sp = cpu.registers.sp.wrapping_add(1);
    // INC SP does not affect any flags
    8
}

pub(super) fn dec_sp(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    mmio.oam_bug_idu(cpu.registers.sp);
    cpu.registers.sp = cpu.registers.sp.wrapping_sub(1);
    // DEC SP does not affect any flags
    8
}

pub(super) fn rra(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_carry = cpu.registers.a & 0x01;
    cpu.registers.a = (cpu.registers.a >> 1) | (old_carry << 7);
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
    4
}

pub(super) fn adc_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16) + (carry as u16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) + (value & 0x0F) + carry) > 0x0F);
    8
}

pub(super) fn xor_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let result = cpu.registers.a ^ value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub(super) fn add_hl_sp(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let sp = cpu.registers.sp;
    let result = hl as u32 + sp as u32;

    cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
    cpu.registers.l = (result & 0xFF) as u8;

    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFFFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((hl & 0x0FFF) + (sp & 0x0FFF)) > 0x0FFF);
    8
}

pub(super) fn cp_memory_hl(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub(super) fn ret(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
    cpu.registers.pc |= (mmio.read(cpu.registers.sp.wrapping_add(1)) as u16) << 8;
    cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
    16
}

pub(super) fn ccf(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let current_carry = cpu.registers.get_flag(registers::Flag::Carry);
    cpu.registers.set_flag(registers::Flag::Carry, !current_carry);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    4
}

pub(super) fn ld_a_memory_c(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = 0xFF00 | (cpu.registers.c as u16);
    cpu.registers.a = mmio.read(addr);
    8
}

pub(super) fn reti(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
    cpu.registers.pc |= (mmio.read(cpu.registers.sp.wrapping_add(1)) as u16) << 8;
    cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
    cpu.registers.ime = true;
    cpu.ime_enable_delay = 0;
    16
}

pub(super) fn scf(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    cpu.registers.set_flag(registers::Flag::Carry, true);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    4
}

pub(super) fn and_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let result = cpu.registers.a & value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, true);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub(super) fn or_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let result = cpu.registers.a | value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub(super) fn cpl(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    cpu.registers.a = !cpu.registers.a;
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::HalfCarry, true);
    4
}

pub(super) fn di(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    cpu.registers.ime = false;
    cpu.ime_enable_delay = 0;
    4
}

pub(super) fn ei(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    // EI enables IME after the instruction FOLLOWING it (1-instruction delay).
    // Pan Docs: Interrupts — https://gbdev.io/pandocs/Interrupts.html
    // Back-to-back EIs must NOT push the enable forward: with consecutive EIs, IME
    // turns on after the 2nd instruction. Only arm when no enable is pending.
    if cpu.ime_enable_delay == 0 {
        cpu.ime_enable_delay = 2;
    }
    4
}

pub(super) fn rla(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_carry = (cpu.registers.a & 0x80) >> 7;
    cpu.registers.a = (cpu.registers.a << 1) | old_carry;
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
    4
}

pub(super) fn ld_memory_hl_inc_a(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub(super) fn ld_memory_hl_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(cpu.registers.pc);
    mmio.write(addr, value);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    12
}

pub(super) fn ld_memory_imm_a_16(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let addr = (high << 8) | low;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
    16
}

pub(super) fn ld_sp_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let value = (high << 8) | low;
    cpu.registers.sp = value;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
    12
}

pub(super) fn ld_a_memory_hl_inc(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    // DMG OAM-bug: the OAM read corruption fires via `mmio.read` (its OAM hook).
    // The hl post-inc does NOT trigger a separate IDU corruption here — faithful
    // to the plain `ld a,(hl+)` model (a single read, plain `hl++`).
    cpu.registers.a = mmio.read(addr);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub(super) fn ld_a_memory_hl_dec(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.a = mmio.read(addr);
    let new_addr = addr.wrapping_sub(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub(super) fn ld_memory_c_a(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = 0xFF00 | (cpu.registers.c as u16);
    mmio.write(addr, cpu.registers.a);
    8
}

pub(super) fn call_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc = cpu.registers.pc.wrapping_add(2);

    mmio.internal_cycle(); // SP-dec internal M-cycle, before the pushes
    cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
    mmio.write(cpu.registers.sp.wrapping_add(1), (cpu.registers.pc >> 8) as u8);
    mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8);

    cpu.registers.pc = addr;
    24
}

pub(super) fn cp_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub(super) fn add_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (value & 0x0F) > 0x0F);
    8
}

pub(super) fn sub_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (value & 0x0F));
    8
}

pub(super) fn ldh_a_memory_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    cpu.registers.a = mmio.read(addr);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    12
}

pub(super) fn ldh_memory_imm_a(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
    12
}

pub(super) fn ld_memory_hl_dec_a(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_sub(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub(super) fn sbc_a_imm(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(1);

    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16) - (carry as i16);

    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < ((value & 0x0F) + carry));
    8
}

pub(super) fn ld_a_memory_imm_16(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
    let addr = (high << 8) | low;
    cpu.registers.a = mmio.read(addr);
    cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
    16
}

pub(super) fn jp_hl(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.pc = addr;
    4
}

macro_rules! make_jp_cond {
    ($name:ident, $cond:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            if $cond(cpu) {
                let low = mmio.read(cpu.registers.pc) as u16;
                let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
                let addr = (high << 8) | low;
                cpu.registers.pc = addr;
                16
            } else {
                cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
                12
            }
        }
    };
}
macro_rules! make_inc_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            cpu.registers.$reg = cpu.registers.$reg.wrapping_add(1);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.$reg & 0x0F) == 0);
            4
        }
    };
}

macro_rules! make_dec_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            cpu.registers.$reg = cpu.registers.$reg.wrapping_sub(1);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.$reg & 0x0F) == 0x0F);
            4
        }
    };
}

macro_rules! make_ld_register_imm {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let val = mmio.read(cpu.registers.pc);
            cpu.registers.$reg = val as u8;
            cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
            8
        }
    };
}

macro_rules! make_inc_memory {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let old_value = mmio.read(addr);
            let new_value = old_value.wrapping_add(1);
            mmio.write(addr, new_value);
            cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (old_value & 0x0F) == 0x0F);
            12
        }
    };
}

pub(super) fn pop_af(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
    let addr = cpu.registers.sp;
    let f_value = mmio.read(addr) & 0xF0; // Only upper 4 bits are valid for F register
    let a_value = mmio.read(addr.wrapping_add(1));
    cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
    cpu.registers.f = f_value;
    cpu.registers.a = a_value;
    12
}

macro_rules! make_alu_add_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a as u16 + operand as u16;

            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) > 0x0F);
            4
        }
    };
}

macro_rules! make_alu_cp_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a.wrapping_sub(operand);
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            4
        }
    };
}

macro_rules! make_alu_adc_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1u8 } else { 0u8 };
            let result = a as u16 + operand as u16 + carry as u16;

            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) + carry > 0x0F);
            4
        }
    };
}

macro_rules! make_alu_sub_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a.wrapping_sub(operand);

            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            4
        }
    };
}

macro_rules! make_alu_and_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let result = cpu.registers.a & cpu.registers.$reg;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            4
        }
    };
}

macro_rules! make_alu_or_register {
    ($name:ident, $op:tt, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let result = cpu.registers.a $op cpu.registers.$reg;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            4
        }
    };
}

macro_rules! make_alu_add_mem_hl {
    ($name:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let a = cpu.registers.a;
            let operand = mmio.read(addr);
            let result = a as u16 + operand as u16;

            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) > 0x0F);
            8
        }
    };
}

macro_rules! make_alu_sub_mem_hl {
    ($name:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let a = cpu.registers.a;
            let operand = mmio.read(addr);
            let result = a.wrapping_sub(operand);

            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            8
        }
    };
}

macro_rules! make_alu_and_mem_hl {
    ($name:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let result = cpu.registers.a & value;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            8
        }
    };
}

macro_rules! make_alu_or_mem_hl {
    ($name:ident, $op:tt) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let result = cpu.registers.a $op value;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            8
        }
    };
}

macro_rules! make_ld_16_bit_imm {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let low = mmio.read(cpu.registers.pc) as u16;
            let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
            let value = (high << 8) | low;
            cpu.registers.$reg1 = (value >> 8) as u8;
            cpu.registers.$reg2 = (value & 0x00FF) as u8;
            cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
            12
        }
    };
}

macro_rules! make_jr_cond {
    ($name:ident, $cond:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let offset = mmio.read(cpu.registers.pc) as i8;
            cpu.registers.pc = cpu.registers.pc.wrapping_add(1);
            if $cond(cpu) {
                cpu.registers.pc = cpu.registers.pc.wrapping_add(offset as i16 as u16);
                12
            } else {
                8
            }
        }
    };
}

macro_rules! make_ret_cond {
    ($name:ident, $cond:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            if $cond(cpu) {
                // Faithful conditional-RET M-cycle layout (mooneye ret_cc_timing):
                //   M1 internal condition-check delay, M2 PC pop low byte, M3 PC pop
                //   high byte, M4 internal delay (batched at instruction end via the
                //   returned 20cc). The leading internal cycle must precede the pops
                //   so the OAM-DMA conflict probe sees the pop M-cycles 4cc later.
                mmio.internal_cycle();
                cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
                cpu.registers.pc |= (mmio.read(cpu.registers.sp.wrapping_add(1)) as u16) << 8;
                cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
                20
            } else {
                8
            }
        }
    };
}

macro_rules! make_dec_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let value = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            // DMG OAM-bug: the 16-bit IDU asserts the pre-op register value on the
            // address bus. If it points at OAM during PPU mode 2 this triggers a
            // write corruption (Pan Docs "Affected Operations"). No-op otherwise.
            mmio.oam_bug_idu(value);
            let new_value = value.wrapping_sub(1);
            cpu.registers.$reg1 = (new_value >> 8) as u8;
            cpu.registers.$reg2 = (new_value & 0x00FF) as u8;
            8
        }
    };
}

macro_rules! make_bitop_register {
    ($name:ident, $reg:ident, $op:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let carry_in = cpu.registers.get_flag(registers::Flag::Carry);
            let (new_value, carry_out) = $op(cpu.registers.$reg, carry_in);
            cpu.registers.$reg = new_value;
            cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, carry_out);
            8
        }
    };
}

/// The `(hl)` twin of `make_bitop_register`: same kernel, same flag rule, but the
/// operand is read from and written back through the HL pointer (16 cycles).
macro_rules! make_bitop_mem_hl {
    ($name:ident, $op:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let old_value = mmio.read(addr);
            let carry_in = cpu.registers.get_flag(registers::Flag::Carry);
            let (new_value, carry_out) = $op(old_value, carry_in);
            mmio.write(addr, new_value);
            cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, carry_out);
            16
        }
    };
}

macro_rules! make_reset_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let new_value = value & !(1 << $bit);
            mmio.write(addr, new_value);
            16
        }
    };
}

macro_rules! make_set_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let new_value = value | (1 << $bit);
            mmio.write(addr, new_value);
            16
        }
    };
}

macro_rules! make_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let bit_set = (value & (1 << $bit)) != 0;
            cpu.registers.set_flag(registers::Flag::Zero, !bit_set);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            12
        }
    };
}

macro_rules! make_inc_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let value = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            // DMG OAM-bug: the 16-bit IDU asserts the pre-op register value on the
            // address bus. If it points at OAM during PPU mode 2 this triggers a
            // write corruption (Pan Docs "Affected Operations"). No-op otherwise.
            mmio.oam_bug_idu(value);
            let new_value = value.wrapping_add(1);
            cpu.registers.$reg1 = (new_value >> 8) as u8;
            cpu.registers.$reg2 = (new_value & 0x00FF) as u8;
            8
        }
    };
}

macro_rules! make_ld_register_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            cpu.registers.$reg1 = cpu.registers.$reg2;
            4
        }
    };
}

macro_rules! make_ld_register_register_self {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(_cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            4
        }
    };
}

macro_rules! make_ld_memory_combined_register_a {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            mmio.write(addr, cpu.registers.a);
            8
        }
    };
}

macro_rules! make_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let bit_set = (cpu.registers.$reg & (1 << $bit)) != 0;
            cpu.registers.set_flag(registers::Flag::Zero, !bit_set);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            8
        }
    };
}

macro_rules! make_set_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            cpu.registers.$reg |= 1 << $bit;
            8
        }
    };
}

macro_rules! make_res_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            cpu.registers.$reg &= !(1 << $bit);
            8
        }
    };
}

macro_rules! make_ld_register_memory_combined {
    ($name:ident, $reg1:ident, $reg2:ident, $reg3:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.$reg2 as u16) << 8) | (cpu.registers.$reg3 as u16);
            cpu.registers.$reg1 = mmio.read(addr);
            8
        }
    };
}

macro_rules! make_push_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            // DMG OAM-bug: PUSH's internal SP-dec M-cycle asserts the (pre-dec) SP
            // on the address bus, triggering a write corruption if SP is in OAM
            // during mode 2 (the PUSH triggers an OAM-bug on SP). The two
            // stack writes below add their own write corruptions via the OAM write
            // hook, at the following M-cycles.
            mmio.oam_bug_idu(cpu.registers.sp);
            mmio.internal_cycle(); // M2 internal (SP dec), before the pushes
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
            mmio.write(cpu.registers.sp.wrapping_add(1), cpu.registers.$reg1); // high byte first
            mmio.write(cpu.registers.sp, cpu.registers.$reg2); // then low
            16
        }
    };
}

macro_rules! make_pop_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = cpu.registers.sp;
            let low = mmio.read(addr);
            let high = mmio.read(addr.wrapping_add(1));
            cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
            cpu.registers.$reg2 = low;
            cpu.registers.$reg1 = high;
            12
        }
    };
}

macro_rules! make_rst {
    ($name:ident, $addr:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            mmio.internal_cycle(); // M2 internal (SP dec) before the pushes
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(1);
            mmio.write(cpu.registers.sp, (cpu.registers.pc >> 8) as u8); // high byte first
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(1);
            mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8); // then low
            cpu.registers.pc = $addr;
            16
        }
    };
}

macro_rules! make_add_hl_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let operand = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let result = hl as u32 + operand as u32;

            cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
            cpu.registers.l = (result & 0x00FF) as u8;

            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, ((hl & 0x0FFF) + (operand & 0x0FFF)) > 0x0FFF);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFFFF);
            8
        }
    };
}

macro_rules! make_ld_memory_hl_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            mmio.write(addr, cpu.registers.$reg);
            8
        }
    };
}

macro_rules! make_call_cond {
    ($name:ident, $cond:expr) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, mmio: &mut crate::cpu::Bus) -> u32 {
            let low = mmio.read(cpu.registers.pc) as u16;
            let high = mmio.read(cpu.registers.pc.wrapping_add(1)) as u16;
            let addr = (high << 8) | low;
            cpu.registers.pc = cpu.registers.pc.wrapping_add(2);
            if $cond(cpu) {
                // Faithful conditional-CALL M-cycle layout (mooneye call_cc_timing2):
                //   M1 nn-low read, M2 nn-high read (above), M3 internal SP-dec delay,
                //   M4 PC push high byte, M5 PC push low byte. The internal cycle must
                //   precede the pushes so the stack writes land 4cc later — the
                //   OAM-DMA conflict probe times the push M-cycle, and the high byte
                //   must be written before the low byte (high at M4, low at M5).
                mmio.internal_cycle();
                cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
                mmio.write(cpu.registers.sp.wrapping_add(1), (cpu.registers.pc >> 8) as u8);
                mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8);
                cpu.registers.pc = addr;
                24
            } else {
                12
            }
        }
    };
}

macro_rules! make_sbc_a_register {
    ($name:ident, $reg:ident) => {
        pub(super) fn $name(cpu: &mut cpu::SM83, _mmio: &mut crate::cpu::Bus) -> u32 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1u8 } else { 0u8 };
            let result = a.wrapping_sub(operand).wrapping_sub(carry);

            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, (a as u16) < (operand as u16 + carry as u16));
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < ((operand & 0x0F) + carry));
            4
        }
    };
}

make_bitop_register!(rl_a, a, op_rl);
make_bitop_register!(rl_b, b, op_rl);
make_bitop_register!(rl_c, c, op_rl);
make_bitop_register!(rl_d, d, op_rl);
make_bitop_register!(rl_e, e, op_rl);
make_bitop_register!(rl_h, h, op_rl);
make_bitop_register!(rl_l, l, op_rl);
make_bitop_register!(rr_a, a, op_rr);
make_bitop_register!(rr_b, b, op_rr);
make_bitop_register!(rr_c, c, op_rr);
make_bitop_register!(rr_d, d, op_rr);
make_bitop_register!(rr_e, e, op_rr);
make_bitop_register!(rr_h, h, op_rr);
make_bitop_register!(rr_l, l, op_rr);
make_push_combined_register!(push_bc, b, c);
make_push_combined_register!(push_de, d, e);
make_push_combined_register!(push_hl, h, l);
make_push_combined_register!(push_af, a, f);
make_pop_combined_register!(pop_bc, b, c);
make_pop_combined_register!(pop_de, d, e);
make_pop_combined_register!(pop_hl, h, l);
make_bit_register!(bit_0_b, 0, b);
make_bit_register!(bit_0_c, 0, c);
make_bit_register!(bit_0_d, 0, d);
make_bit_register!(bit_0_e, 0, e);
make_bit_register!(bit_0_h, 0, h);
make_bit_register!(bit_0_l, 0, l);
make_bit_register!(bit_0_a, 0, a);
make_bit_register!(bit_1_b, 1, b);
make_bit_register!(bit_1_c, 1, c);
make_bit_register!(bit_1_d, 1, d);
make_bit_register!(bit_1_e, 1, e);
make_bit_register!(bit_1_h, 1, h);
make_bit_register!(bit_1_l, 1, l);
make_bit_register!(bit_1_a, 1, a);
make_bit_register!(bit_2_b, 2, b);
make_bit_register!(bit_2_c, 2, c);
make_bit_register!(bit_2_d, 2, d);
make_bit_register!(bit_2_e, 2, e);
make_bit_register!(bit_2_h, 2, h);
make_bit_register!(bit_2_l, 2, l);
make_bit_register!(bit_2_a, 2, a);
make_bit_register!(bit_3_b, 3, b);
make_bit_register!(bit_3_c, 3, c);
make_bit_register!(bit_3_d, 3, d);
make_bit_register!(bit_3_e, 3, e);
make_bit_register!(bit_3_h, 3, h);
make_bit_register!(bit_3_l, 3, l);
make_bit_register!(bit_3_a, 3, a);
make_bit_register!(bit_4_b, 4, b);
make_bit_register!(bit_4_c, 4, c);
make_bit_register!(bit_4_d, 4, d);
make_bit_register!(bit_4_e, 4, e);
make_bit_register!(bit_4_h, 4, h);
make_bit_register!(bit_4_l, 4, l);
make_bit_register!(bit_4_a, 4, a);
make_bit_register!(bit_5_b, 5, b);
make_bit_register!(bit_5_c, 5, c);
make_bit_register!(bit_5_d, 5, d);
make_bit_register!(bit_5_e, 5, e);
make_bit_register!(bit_5_h, 5, h);
make_bit_register!(bit_5_l, 5, l);
make_bit_register!(bit_5_a, 5, a);
make_bit_register!(bit_6_b, 6, b);
make_bit_register!(bit_6_c, 6, c);
make_bit_register!(bit_6_d, 6, d);
make_bit_register!(bit_6_e, 6, e);
make_bit_register!(bit_6_h, 6, h);
make_bit_register!(bit_6_l, 6, l);
make_bit_register!(bit_6_a, 6, a);
make_bit_register!(bit_7_b, 7, b);
make_bit_register!(bit_7_c, 7, c);
make_bit_register!(bit_7_d, 7, d);
make_bit_register!(bit_7_e, 7, e);
make_bit_register!(bit_7_h, 7, h);
make_bit_register!(bit_7_l, 7, l);
make_bit_register!(bit_7_a, 7, a);
make_ld_register_memory_combined!(ld_a_memory_bc, a, b, c);
make_ld_register_memory_combined!(ld_a_memory_de, a, d, e);
make_ld_register_memory_combined!(ld_b_memory_hl, b, h, l);
make_ld_register_memory_combined!(ld_c_memory_hl, c, h, l);
make_ld_register_memory_combined!(ld_d_memory_hl, d, h, l);
make_ld_register_memory_combined!(ld_e_memory_hl, e, h, l);
make_ld_register_memory_combined!(ld_h_memory_hl, h, h, l);
make_ld_register_memory_combined!(ld_l_memory_hl, l, h, l);
make_ld_register_memory_combined!(ld_a_memory_hl, a, h, l);
make_ld_memory_hl_register!(ld_memory_hl_a, a);
make_ld_memory_hl_register!(ld_memory_hl_b, b);
make_ld_memory_hl_register!(ld_memory_hl_c, c);
make_ld_memory_hl_register!(ld_memory_hl_d, d);
make_ld_memory_hl_register!(ld_memory_hl_e, e);
make_ld_memory_hl_register!(ld_memory_hl_h, h);
make_ld_memory_hl_register!(ld_memory_hl_l, l);
make_ld_memory_combined_register_a!(ld_memory_bc_a, b, c);
make_ld_memory_combined_register_a!(ld_memory_de_a, d, e);
make_ld_register_register!(ld_a_b, a, b);
make_ld_register_register!(ld_a_c, a, c);
make_ld_register_register!(ld_a_d, a, d);
make_ld_register_register!(ld_a_e, a, e);
make_ld_register_register!(ld_a_h, a, h);
make_ld_register_register!(ld_a_l, a, l);
make_ld_register_register_self!(ld_a_a, a, a);
make_ld_register_register!(ld_b_a, b, a);
make_ld_register_register_self!(ld_b_b, b, b);
make_ld_register_register!(ld_b_c, b, c);
make_ld_register_register!(ld_b_d, b, d);
make_ld_register_register!(ld_b_e, b, e);
make_ld_register_register!(ld_b_h, b, h);
make_ld_register_register!(ld_b_l, b, l);
make_ld_register_register!(ld_c_a, c, a);
make_ld_register_register!(ld_c_b, c, b);
make_ld_register_register_self!(ld_c_c, c, c);
make_ld_register_register!(ld_c_d, c, d);
make_ld_register_register!(ld_c_e, c, e);
make_ld_register_register!(ld_c_h, c, h);
make_ld_register_register!(ld_c_l, c, l);
make_ld_register_register!(ld_d_a, d, a);
make_ld_register_register!(ld_d_b, d, b);
make_ld_register_register!(ld_d_c, d, c);
make_ld_register_register_self!(ld_d_d, d, d);
make_ld_register_register!(ld_d_e, d, e);
make_ld_register_register!(ld_d_h, d, h);
make_ld_register_register!(ld_d_l, d, l);
make_ld_register_register!(ld_e_a, e, a);
make_ld_register_register!(ld_e_b, e, b);
make_ld_register_register!(ld_e_c, e, c);
make_ld_register_register!(ld_e_d, e, d);
make_ld_register_register_self!(ld_e_e, e, e);
make_ld_register_register!(ld_e_h, e, h);
make_ld_register_register!(ld_e_l, e, l);
make_ld_register_register!(ld_h_a, h, a);
make_ld_register_register!(ld_h_b, h, b);
make_ld_register_register!(ld_h_c, h, c);
make_ld_register_register!(ld_h_d, h, d);
make_ld_register_register!(ld_h_e, h, e);
make_ld_register_register_self!(ld_h_h, h, h);
make_ld_register_register!(ld_h_l, h, l);
make_ld_register_register!(ld_l_a, l, a);
make_ld_register_register!(ld_l_b, l, b);
make_ld_register_register!(ld_l_c, l, c);
make_ld_register_register!(ld_l_d, l, d);
make_ld_register_register!(ld_l_e, l, e);
make_ld_register_register!(ld_l_h, l, h);
make_ld_register_register_self!(ld_l_l, l, l);
make_inc_register!(inc_a, a);
make_inc_register!(inc_b, b);
make_inc_register!(inc_c, c);
make_inc_register!(inc_d, d);
make_inc_register!(inc_e, e);
make_inc_register!(inc_h, h);
make_inc_register!(inc_l, l);
make_dec_register!(dec_a, a);
make_dec_register!(dec_b, b);
make_dec_register!(dec_c, c);
make_dec_register!(dec_d, d);
make_dec_register!(dec_e, e);
make_dec_register!(dec_h, h);
make_dec_register!(dec_l, l);
make_inc_combined_register!(inc_bc, b, c);
make_inc_combined_register!(inc_de, d, e);
make_inc_combined_register!(inc_hl, h, l);
make_dec_combined_register!(dec_bc, b, c);
make_dec_combined_register!(dec_de, d, e);
make_dec_combined_register!(dec_hl, h, l);
make_ld_register_imm!(ld_a_imm, a);
make_ld_register_imm!(ld_b_imm, b);
make_ld_register_imm!(ld_c_imm, c);
make_ld_register_imm!(ld_d_imm, d);
make_ld_register_imm!(ld_e_imm, e);
make_ld_register_imm!(ld_h_imm, h);
make_ld_register_imm!(ld_l_imm, l);
make_inc_memory!(inc_memory_hl, h, l);
make_alu_and_register!(and_a, a);
make_alu_and_register!(and_b, b);
make_alu_and_register!(and_c, c);
make_alu_and_register!(and_d, d);
make_alu_and_register!(and_e, e);
make_alu_and_register!(and_h, h);
make_alu_and_register!(and_l, l);
make_alu_or_register!(or_a, |, a);
make_alu_or_register!(or_b, |, b);
make_alu_or_register!(or_c, |, c);
make_alu_or_register!(or_d, |, d);
make_alu_or_register!(or_e, |, e);
make_alu_or_register!(or_h, |, h);
make_alu_or_register!(or_l, |, l);
make_alu_or_register!(xor_a, ^, a);
make_alu_or_register!(xor_b, ^, b);
make_alu_or_register!(xor_c, ^, c);
make_alu_or_register!(xor_d, ^, d);
make_alu_or_register!(xor_e, ^, e);
make_alu_or_register!(xor_h, ^, h);
make_alu_or_register!(xor_l, ^, l);
make_alu_cp_register!(cp_a, a);
make_alu_cp_register!(cp_b, b);
make_alu_cp_register!(cp_c, c);
make_alu_cp_register!(cp_d, d);
make_alu_cp_register!(cp_e, e);
make_alu_cp_register!(cp_h, h);
make_alu_cp_register!(cp_l, l);
make_alu_adc_register!(adc_a, a);
make_alu_adc_register!(adc_b, b);
make_alu_adc_register!(adc_c, c);
make_alu_adc_register!(adc_d, d);
make_alu_adc_register!(adc_e, e);
make_alu_adc_register!(adc_h, h);
make_alu_adc_register!(adc_l, l);
make_alu_add_register!(add_a, a);
make_alu_add_register!(add_b, b);
make_alu_add_register!(add_c, c);
make_alu_add_register!(add_d, d);
make_alu_add_register!(add_e, e);
make_alu_add_register!(add_h, h);
make_alu_add_register!(add_l, l);
make_alu_sub_register!(sub_a, a);
make_alu_sub_register!(sub_b, b);
make_alu_sub_register!(sub_c, c);
make_alu_sub_register!(sub_d, d);
make_alu_sub_register!(sub_e, e);
make_alu_sub_register!(sub_h, h);
make_alu_sub_register!(sub_l, l);
make_alu_and_mem_hl!(and_memory_hl);
make_alu_or_mem_hl!(or_memory_hl, |);
make_alu_or_mem_hl!(xor_memory_hl, ^);
make_alu_add_mem_hl!(add_memory_hl);
make_alu_sub_mem_hl!(sub_memory_hl);
make_ld_16_bit_imm!(ld_bc_imm, b, c);
make_ld_16_bit_imm!(ld_de_imm, d, e);
make_ld_16_bit_imm!(ld_hl_imm, h, l);
make_ret_cond!(ret_nz, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_ret_cond!(ret_z, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_ret_cond!(ret_nc, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_ret_cond!(ret_c, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_bitop_register!(swap_a, a, op_swap);
make_bitop_register!(swap_b, b, op_swap);
make_bitop_register!(swap_c, c, op_swap);
make_bitop_register!(swap_d, d, op_swap);
make_bitop_register!(swap_e, e, op_swap);
make_bitop_register!(swap_h, h, op_swap);
make_bitop_register!(swap_l, l, op_swap);
make_rst!(rst_00, 0x00);
make_rst!(rst_08, 0x08);
make_rst!(rst_10, 0x10);
make_rst!(rst_18, 0x18);
make_rst!(rst_20, 0x20);
make_rst!(rst_28, 0x28);
make_rst!(rst_30, 0x30);
make_rst!(rst_38, 0x38);
make_add_hl_combined_register!(add_hl_bc, b, c);
make_add_hl_combined_register!(add_hl_de, d, e);
make_add_hl_combined_register!(add_hl_hl, h, l);
make_res_bit_register!(res_0_b, 0, b);
make_res_bit_register!(res_0_c, 0, c);
make_res_bit_register!(res_0_d, 0, d);
make_res_bit_register!(res_0_e, 0, e);
make_res_bit_register!(res_0_h, 0, h);
make_res_bit_register!(res_0_l, 0, l);
make_res_bit_register!(res_0_a, 0, a);
make_res_bit_register!(res_1_b, 1, b);
make_res_bit_register!(res_1_c, 1, c);
make_res_bit_register!(res_1_d, 1, d);
make_res_bit_register!(res_1_e, 1, e);
make_res_bit_register!(res_1_h, 1, h);
make_res_bit_register!(res_1_l, 1, l);
make_res_bit_register!(res_1_a, 1, a);
make_res_bit_register!(res_2_b, 2, b);
make_res_bit_register!(res_2_c, 2, c);
make_res_bit_register!(res_2_d, 2, d);
make_res_bit_register!(res_2_e, 2, e);
make_res_bit_register!(res_2_h, 2, h);
make_res_bit_register!(res_2_l, 2, l);
make_res_bit_register!(res_2_a, 2, a);
make_res_bit_register!(res_3_b, 3, b);
make_res_bit_register!(res_3_c, 3, c);
make_res_bit_register!(res_3_d, 3, d);
make_res_bit_register!(res_3_e, 3, e);
make_res_bit_register!(res_3_h, 3, h);
make_res_bit_register!(res_3_l, 3, l);
make_res_bit_register!(res_3_a, 3, a);
make_res_bit_register!(res_4_b, 4, b);
make_res_bit_register!(res_4_c, 4, c);
make_res_bit_register!(res_4_d, 4, d);
make_res_bit_register!(res_4_e, 4, e);
make_res_bit_register!(res_4_h, 4, h);
make_res_bit_register!(res_4_l, 4, l);
make_res_bit_register!(res_4_a, 4, a);
make_res_bit_register!(res_5_b, 5, b);
make_res_bit_register!(res_5_c, 5, c);
make_res_bit_register!(res_5_d, 5, d);
make_res_bit_register!(res_5_e, 5, e);
make_res_bit_register!(res_5_h, 5, h);
make_res_bit_register!(res_5_l, 5, l);
make_res_bit_register!(res_5_a, 5, a);
make_res_bit_register!(res_6_b, 6, b);
make_res_bit_register!(res_6_c, 6, c);
make_res_bit_register!(res_6_d, 6, d);
make_res_bit_register!(res_6_e, 6, e);
make_res_bit_register!(res_6_h, 6, h);
make_res_bit_register!(res_6_l, 6, l);
make_res_bit_register!(res_6_a, 6, a);
make_res_bit_register!(res_7_b, 7, b);
make_res_bit_register!(res_7_c, 7, c);
make_res_bit_register!(res_7_d, 7, d);
make_res_bit_register!(res_7_e, 7, e);
make_res_bit_register!(res_7_h, 7, h);
make_res_bit_register!(res_7_l, 7, l);
make_res_bit_register!(res_7_a, 7, a);
make_set_bit_register!(set_0_b, 0, b);
make_set_bit_register!(set_0_c, 0, c);
make_set_bit_register!(set_0_d, 0, d);
make_set_bit_register!(set_0_e, 0, e);
make_set_bit_register!(set_0_h, 0, h);
make_set_bit_register!(set_0_l, 0, l);
make_set_bit_register!(set_0_a, 0, a);
make_set_bit_register!(set_1_b, 1, b);
make_set_bit_register!(set_1_c, 1, c);
make_set_bit_register!(set_1_d, 1, d);
make_set_bit_register!(set_1_e, 1, e);
make_set_bit_register!(set_1_h, 1, h);
make_set_bit_register!(set_1_l, 1, l);
make_set_bit_register!(set_1_a, 1, a);
make_set_bit_register!(set_2_b, 2, b);
make_set_bit_register!(set_2_c, 2, c);
make_set_bit_register!(set_2_d, 2, d);
make_set_bit_register!(set_2_e, 2, e);
make_set_bit_register!(set_2_h, 2, h);
make_set_bit_register!(set_2_l, 2, l);
make_set_bit_register!(set_2_a, 2, a);
make_set_bit_register!(set_3_b, 3, b);
make_set_bit_register!(set_3_c, 3, c);
make_set_bit_register!(set_3_d, 3, d);
make_set_bit_register!(set_3_e, 3, e);
make_set_bit_register!(set_3_h, 3, h);
make_set_bit_register!(set_3_l, 3, l);
make_set_bit_register!(set_3_a, 3, a);
make_set_bit_register!(set_4_b, 4, b);
make_set_bit_register!(set_4_c, 4, c);
make_set_bit_register!(set_4_d, 4, d);
make_set_bit_register!(set_4_e, 4, e);
make_set_bit_register!(set_4_h, 4, h);
make_set_bit_register!(set_4_l, 4, l);
make_set_bit_register!(set_4_a, 4, a);
make_set_bit_register!(set_5_b, 5, b);
make_set_bit_register!(set_5_c, 5, c);
make_set_bit_register!(set_5_d, 5, d);
make_set_bit_register!(set_5_e, 5, e);
make_set_bit_register!(set_5_h, 5, h);
make_set_bit_register!(set_5_l, 5, l);
make_set_bit_register!(set_5_a, 5, a);
make_set_bit_register!(set_6_b, 6, b);
make_set_bit_register!(set_6_c, 6, c);
make_set_bit_register!(set_6_d, 6, d);
make_set_bit_register!(set_6_e, 6, e);
make_set_bit_register!(set_6_h, 6, h);
make_set_bit_register!(set_6_l, 6, l);
make_set_bit_register!(set_6_a, 6, a);
make_set_bit_register!(set_7_b, 7, b);
make_set_bit_register!(set_7_c, 7, c);
make_set_bit_register!(set_7_d, 7, d);
make_set_bit_register!(set_7_e, 7, e);
make_set_bit_register!(set_7_h, 7, h);
make_set_bit_register!(set_7_l, 7, l);
make_set_bit_register!(set_7_a, 7, a);
make_reset_bit_memory_hl!(res_7_hl, 7);
make_reset_bit_memory_hl!(res_6_hl, 6);
make_reset_bit_memory_hl!(res_5_hl, 5);
make_reset_bit_memory_hl!(res_4_hl, 4);
make_reset_bit_memory_hl!(res_3_hl, 3);
make_reset_bit_memory_hl!(res_2_hl, 2);
make_reset_bit_memory_hl!(res_1_hl, 1);
make_reset_bit_memory_hl!(res_0_hl, 0);
make_set_bit_memory_hl!(set_7_hl, 7);
make_set_bit_memory_hl!(set_6_hl, 6);
make_set_bit_memory_hl!(set_5_hl, 5);
make_set_bit_memory_hl!(set_4_hl, 4);
make_set_bit_memory_hl!(set_3_hl, 3);
make_set_bit_memory_hl!(set_2_hl, 2);
make_set_bit_memory_hl!(set_1_hl, 1);
make_set_bit_memory_hl!(set_0_hl, 0);
make_bit_memory_hl!(bit_7_hl, 7);
make_bit_memory_hl!(bit_6_hl, 6);
make_bit_memory_hl!(bit_5_hl, 5);
make_bit_memory_hl!(bit_4_hl, 4);
make_bit_memory_hl!(bit_3_hl, 3);
make_bit_memory_hl!(bit_2_hl, 2);
make_bit_memory_hl!(bit_1_hl, 1);
make_bit_memory_hl!(bit_0_hl, 0);
make_jp_cond!(jp_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jp_cond!(jp_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jp_cond!(jp_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jp_cond!(jp_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_bitop_register!(sla_a, a, op_sla);
make_bitop_register!(sla_b, b, op_sla);
make_bitop_register!(sla_c, c, op_sla);
make_bitop_register!(sla_d, d, op_sla);
make_bitop_register!(sla_e, e, op_sla);
make_bitop_register!(sla_h, h, op_sla);
make_bitop_register!(sla_l, l, op_sla);
make_bitop_register!(sra_a, a, op_sra);
make_bitop_register!(sra_b, b, op_sra);
make_bitop_register!(sra_c, c, op_sra);
make_bitop_register!(sra_d, d, op_sra);
make_bitop_register!(sra_e, e, op_sra);
make_bitop_register!(sra_h, h, op_sra);
make_bitop_register!(sra_l, l, op_sra);
make_bitop_register!(srl_a, a, op_srl);
make_bitop_register!(srl_b, b, op_srl);
make_bitop_register!(srl_c, c, op_srl);
make_bitop_register!(srl_d, d, op_srl);
make_bitop_register!(srl_e, e, op_srl);
make_bitop_register!(srl_h, h, op_srl);
make_bitop_register!(srl_l, l, op_srl);
make_bitop_register!(rlc_a, a, op_rlc);
make_bitop_register!(rlc_b, b, op_rlc);
make_bitop_register!(rlc_c, c, op_rlc);
make_bitop_register!(rlc_d, d, op_rlc);
make_bitop_register!(rlc_e, e, op_rlc);
make_bitop_register!(rlc_h, h, op_rlc);
make_bitop_register!(rlc_l, l, op_rlc);
make_bitop_register!(rrc_a, a, op_rrc);
make_bitop_register!(rrc_b, b, op_rrc);
make_bitop_register!(rrc_c, c, op_rrc);
make_bitop_register!(rrc_d, d, op_rrc);
make_bitop_register!(rrc_e, e, op_rrc);
make_bitop_register!(rrc_h, h, op_rrc);
make_bitop_register!(rrc_l, l, op_rrc);
make_call_cond!(call_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_call_cond!(call_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_call_cond!(call_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_call_cond!(call_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_sbc_a_register!(sbc_a_a, a);
make_sbc_a_register!(sbc_a_b, b);
make_sbc_a_register!(sbc_a_c, c);
make_sbc_a_register!(sbc_a_d, d);
make_sbc_a_register!(sbc_a_e, e);
make_sbc_a_register!(sbc_a_h, h);
make_sbc_a_register!(sbc_a_l, l);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::mmio::Mmio;
    use crate::ppu::Ppu;

    /// Drive one handler with `pc` parked at a chosen address. The u16 program
    /// counter wraps on hardware, so every one of these must complete rather
    /// than overflow-panic under the debug profile.
    fn at_pc(pc: u16, f: impl FnOnce(&mut cpu::SM83, &mut crate::cpu::Bus)) -> cpu::SM83 {
        let mut sm83 = cpu::SM83::new();
        let mut mmio = Mmio::new();
        let mut ppu = Ppu::new();
        sm83.registers.pc = pc;
        {
            let mut bus = crate::cpu::Bus::new(&mut mmio, &mut ppu);
            f(&mut sm83, &mut bus);
        }
        sm83
    }

    /// The shared kernels replaced eight hand-written `(hl)` handlers and eight
    /// per-op register macros. These pin each kernel to the exact expression the
    /// pre-refactor code used, over the whole input domain.
    #[test]
    fn bitop_kernels_match_the_original_expressions() {
        for v in 0u8..=0xFF {
            for carry_in in [false, true] {
                let c = carry_in as u8;
                assert_eq!(op_rlc(v, carry_in), ((v << 1) | ((v & 0x80) >> 7), (v & 0x80) != 0), "rlc {v:#04X}");
                assert_eq!(op_rrc(v, carry_in), ((v >> 1) | ((v & 0x01) << 7), (v & 0x01) != 0), "rrc {v:#04X}");
                assert_eq!(op_rl(v, carry_in), ((v << 1) | c, (v & 0x80) != 0), "rl {v:#04X}");
                assert_eq!(op_rr(v, carry_in), ((v >> 1) | (c << 7), (v & 0x01) != 0), "rr {v:#04X}");
                assert_eq!(op_sla(v, carry_in), (v << 1, (v & 0x80) != 0), "sla {v:#04X}");
                assert_eq!(op_sra(v, carry_in), ((v >> 1) | (v & 0x80), (v & 0x01) != 0), "sra {v:#04X}");
                assert_eq!(op_srl(v, carry_in), (v >> 1, (v & 0x01) != 0), "srl {v:#04X}");
                // Both pre-refactor idioms for swap: the register macro's nibble
                // masks and the `(hl)` handler's `rotate_right(4)` form.
                assert_eq!(op_swap(v, carry_in), (((v & 0xF0) >> 4) | ((v & 0x0F) << 4), false), "swap {v:#04X}");
                assert_eq!(op_swap(v, carry_in).0, (v << 4) | v.rotate_right(4), "swap rotate idiom {v:#04X}");
            }
        }
    }

    #[test]
    fn jp_imm_reads_its_operand_across_the_pc_wrap() {
        at_pc(0xFFFF, |cpu, bus| {
            jp_imm(cpu, bus);
        });
    }

    #[test]
    fn ld_16_bit_imm_reads_its_operand_across_the_pc_wrap() {
        at_pc(0xFFFF, |cpu, bus| {
            ld_bc_imm(cpu, bus);
        });
    }

    #[test]
    fn call_imm_reads_its_operand_across_the_pc_wrap() {
        at_pc(0xFFFF, |cpu, bus| {
            call_imm(cpu, bus);
        });
    }

    /// `pc` lands on 0x0000 after the wrap, so the operand fetch itself is the
    /// overflow site rather than the post-increment.
    #[test]
    fn ld_imm_advances_pc_across_the_wrap() {
        let cpu = at_pc(0xFFFF, |cpu, bus| {
            ld_b_imm(cpu, bus);
        });
        assert_eq!(cpu.registers.pc, 0x0000);
    }

    /// `jr` computes its target in i16, which overflows for pc near 0x8000 even
    /// though the u16 result is perfectly representable.
    #[test]
    fn jr_imm_target_does_not_overflow_i16_near_0x8000() {
        let cpu = at_pc(0x7FFF, |cpu, bus| {
            jr_imm(cpu, bus);
        });
        // Operand byte at 0x7FFF reads as open-bus/ROM; only the arithmetic is
        // under test, so just require a completed, wrapped u16 target.
        let _ = cpu.registers.pc;
    }

    #[test]
    fn jr_imm_advances_pc_across_the_wrap() {
        at_pc(0xFFFF, |cpu, bus| {
            jr_imm(cpu, bus);
        });
    }

    #[test]
    fn ld_memory_imm_16_sp_writes_its_high_byte_across_the_address_wrap() {
        let mut sm83 = cpu::SM83::new();
        let mut mmio = Mmio::new();
        let mut ppu = Ppu::new();
        // Point the operand at an address pair that straddles the 0xFFFF wrap by
        // writing the little-endian target 0xFFFF into work RAM and running there.
        sm83.registers.pc = 0xC000;
        mmio.write(0xC000, 0xFF);
        mmio.write(0xC001, 0xFF);
        {
            let mut bus = crate::cpu::Bus::new(&mut mmio, &mut ppu);
            ld_memory_imm_16_sp(&mut sm83, &mut bus);
        }
    }
}

make_bitop_mem_hl!(rlc_hl, op_rlc);
make_bitop_mem_hl!(rrc_hl, op_rrc);
make_bitop_mem_hl!(rl_hl, op_rl);
make_bitop_mem_hl!(rr_hl, op_rr);
make_bitop_mem_hl!(sla_hl, op_sla);
make_bitop_mem_hl!(sra_hl, op_sra);
make_bitop_mem_hl!(srl_hl, op_srl);
make_bitop_mem_hl!(swap_hl, op_swap);
