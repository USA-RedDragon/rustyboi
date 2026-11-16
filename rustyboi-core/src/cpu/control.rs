//! The instructions that act on the CPU's own state rather than on data:
//! STOP, HALT, DI, EI and the unused opcodes that lock the CPU. They are
//! apart from the interpreter (`opcodes`) because a CPU that runs the other
//! instructions outside rustyboi (`GB::ext_control`) still runs these here,
//! next to the instruction boundary they hand over to.

use crate::{cpu, cpu::registers, memory::Addressable as _};

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
