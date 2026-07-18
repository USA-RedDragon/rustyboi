//! Native ROM patching: apply IPS / UPS / BPS patches to a loaded ROM so users
//! can run romhacks and translations without external tools.
//!
//! Pure byte ops — no I/O, no deps — so it builds identically on desktop, web
//! (wasm32), and Android. The entry point is [`apply_patch`], which auto-detects
//! the format by magic and returns the patched ROM bytes.

/// Apply an IPS / UPS / BPS `patch` to `rom`, auto-detecting the format by its
/// magic bytes. Returns the patched output on success, or a human-readable error.
pub fn apply_patch(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
    if patch.starts_with(b"PATCH") {
        apply_ips(rom, patch)
    } else if patch.starts_with(b"UPS1") {
        apply_ups(rom, patch)
    } else if patch.starts_with(b"BPS1") {
        apply_bps(rom, patch)
    } else {
        Err("unrecognized patch format (expected IPS, UPS, or BPS)".into())
    }
}

// --- CRC32 (IEEE, reflected) -----------------------------------------------

pub(crate) use rustyboi_core_lib::checksum::crc32;

// --- IPS --------------------------------------------------------------------

fn apply_ips(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
    // 5-byte "PATCH" magic already matched by the caller.
    let mut out = rom.to_vec();
    let mut p = 5usize;

    let read = |p: &mut usize, n: usize| -> Result<usize, String> {
        if *p + n > patch.len() {
            return Err("IPS record truncated".into());
        }
        let mut v = 0usize;
        for _ in 0..n {
            v = (v << 8) | patch[*p] as usize;
            *p += 1;
        }
        Ok(v)
    };

    loop {
        if p + 3 > patch.len() {
            return Err("IPS stream ended without EOF marker".into());
        }
        if &patch[p..p + 3] == b"EOF" {
            p += 3;
            // Optional 3-byte truncation extension.
            if p + 3 <= patch.len() {
                let trunc = read(&mut p, 3)?;
                out.truncate(trunc);
            }
            break;
        }
        let offset = read(&mut p, 3)?;
        let size = read(&mut p, 2)?;
        if size == 0 {
            // RLE run: 2-byte run length + 1 repeated byte.
            let run = read(&mut p, 2)?;
            let byte = read(&mut p, 1)? as u8;
            let end = offset + run;
            if end > out.len() {
                out.resize(end, 0);
            }
            for b in &mut out[offset..end] {
                *b = byte;
            }
        } else {
            if p + size > patch.len() {
                return Err("IPS data record truncated".into());
            }
            let end = offset + size;
            if end > out.len() {
                out.resize(end, 0);
            }
            out[offset..end].copy_from_slice(&patch[p..p + size]);
            p += size;
        }
    }
    Ok(out)
}

// --- UPS / BPS variable-length integers ------------------------------------

/// Decode a UPS/BPS variable-width integer at `*p` (7 payload bits per byte,
/// high bit = continue; the "+1" bias per continuation byte per the spec).
fn read_vuint(data: &[u8], p: &mut usize) -> Result<u64, String> {
    let mut value: u64 = 0;
    let mut shift: u64 = 1;
    loop {
        let b = *data.get(*p).ok_or("varint ran past end of patch")?;
        *p += 1;
        value += ((b & 0x7F) as u64).wrapping_mul(shift);
        if b & 0x80 != 0 {
            break;
        }
        shift <<= 7;
        value += shift;
    }
    Ok(value)
}

fn read_u32_le(data: &[u8], at: usize) -> Result<u32, String> {
    let s = data.get(at..at + 4).ok_or("missing trailing CRC32")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

// --- UPS --------------------------------------------------------------------

fn apply_ups(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
    if patch.len() < 4 + 12 {
        return Err("UPS patch too short".into());
    }
    // Trailing CRCs: source, target, patch (each little-endian u32).
    let src_crc = read_u32_le(patch, patch.len() - 12)?;
    let tgt_crc = read_u32_le(patch, patch.len() - 8)?;
    let body_end = patch.len() - 12;

    if crc32(rom) != src_crc {
        return Err("UPS source CRC mismatch (patch is for a different ROM)".into());
    }

    let mut p = 4usize; // past "UPS1"
    let in_size = read_vuint(patch, &mut p)? as usize;
    let out_size = read_vuint(patch, &mut p)? as usize;
    let _ = in_size;

    let mut out = vec![0u8; out_size];
    let copy = rom.len().min(out_size);
    out[..copy].copy_from_slice(&rom[..copy]);

    let mut out_pos = 0usize;
    while p < body_end {
        let skip = read_vuint(patch, &mut p)? as usize;
        out_pos = out_pos
            .checked_add(skip)
            .ok_or("UPS relative offset overflow")?;
        loop {
            let x = *patch.get(p).ok_or("UPS XOR stream truncated")?;
            p += 1;
            if out_pos < out.len() {
                out[out_pos] ^= x;
            }
            out_pos += 1;
            if x == 0 {
                break;
            }
        }
    }

    if crc32(&out) != tgt_crc {
        return Err("UPS target CRC mismatch (patched output is corrupt)".into());
    }
    Ok(out)
}

// --- BPS --------------------------------------------------------------------

const BPS_SOURCE_READ: u64 = 0;
const BPS_TARGET_READ: u64 = 1;
const BPS_SOURCE_COPY: u64 = 2;
const BPS_TARGET_COPY: u64 = 3;

fn apply_bps(rom: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
    if patch.len() < 4 + 12 {
        return Err("BPS patch too short".into());
    }
    let src_crc = read_u32_le(patch, patch.len() - 12)?;
    let tgt_crc = read_u32_le(patch, patch.len() - 8)?;
    let body_end = patch.len() - 12;

    if crc32(rom) != src_crc {
        return Err("BPS source CRC mismatch (patch is for a different ROM)".into());
    }

    let mut p = 4usize; // past "BPS1"
    let src_size = read_vuint(patch, &mut p)? as usize;
    let out_size = read_vuint(patch, &mut p)? as usize;
    let meta_len = read_vuint(patch, &mut p)? as usize;
    if src_size != rom.len() {
        return Err(format!(
            "BPS source size mismatch (expected {src_size}, got {})",
            rom.len()
        ));
    }
    p = p.checked_add(meta_len).ok_or("BPS metadata overflow")?;
    if p > body_end {
        return Err("BPS metadata runs past patch body".into());
    }

    let mut out = vec![0u8; out_size];
    let mut out_pos = 0usize;
    let mut src_rel = 0usize;
    let mut tgt_rel = 0usize;

    while p < body_end {
        let cmd = read_vuint(patch, &mut p)?;
        let action = cmd & 3;
        let length = (cmd >> 2) as usize + 1;
        match action {
            BPS_SOURCE_READ => {
                for _ in 0..length {
                    let b = *rom.get(out_pos).ok_or("BPS SourceRead past source end")?;
                    *out.get_mut(out_pos).ok_or("BPS SourceRead past target end")? = b;
                    out_pos += 1;
                }
            }
            BPS_TARGET_READ => {
                for _ in 0..length {
                    let b = *patch.get(p).ok_or("BPS TargetRead past patch end")?;
                    p += 1;
                    *out.get_mut(out_pos).ok_or("BPS TargetRead past target end")? = b;
                    out_pos += 1;
                }
            }
            BPS_SOURCE_COPY => {
                let raw = read_vuint(patch, &mut p)?;
                let neg = raw & 1 != 0;
                let off = (raw >> 1) as usize;
                if neg {
                    src_rel = src_rel.checked_sub(off).ok_or("BPS SourceCopy offset underflow")?;
                } else {
                    src_rel += off;
                }
                for _ in 0..length {
                    let b = *rom.get(src_rel).ok_or("BPS SourceCopy past source end")?;
                    *out.get_mut(out_pos).ok_or("BPS SourceCopy past target end")? = b;
                    src_rel += 1;
                    out_pos += 1;
                }
            }
            BPS_TARGET_COPY => {
                let raw = read_vuint(patch, &mut p)?;
                let neg = raw & 1 != 0;
                let off = (raw >> 1) as usize;
                if neg {
                    tgt_rel = tgt_rel.checked_sub(off).ok_or("BPS TargetCopy offset underflow")?;
                } else {
                    tgt_rel += off;
                }
                // Byte-by-byte (source and dest may overlap; run-fills are valid).
                for _ in 0..length {
                    let b = *out.get(tgt_rel).ok_or("BPS TargetCopy past written target")?;
                    *out.get_mut(out_pos).ok_or("BPS TargetCopy past target end")? = b;
                    tgt_rel += 1;
                    out_pos += 1;
                }
            }
            _ => unreachable!("action is masked to 2 bits"),
        }
    }

    if crc32(&out) != tgt_crc {
        return Err("BPS target CRC mismatch (patched output is corrupt)".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be3(v: usize) -> [u8; 3] {
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    }
    fn be2(v: usize) -> [u8; 2] {
        [(v >> 8) as u8, v as u8]
    }

    #[test]
    fn ips_data_and_rle_and_truncate() {
        let rom = vec![0u8; 16];
        let mut patch = b"PATCH".to_vec();
        // Data record: offset 2, size 3, bytes AA BB CC.
        patch.extend_from_slice(&be3(2));
        patch.extend_from_slice(&be2(3));
        patch.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        // RLE record: offset 8, size 0, run 4, byte 0x77.
        patch.extend_from_slice(&be3(8));
        patch.extend_from_slice(&be2(0));
        patch.extend_from_slice(&be2(4));
        patch.push(0x77);
        // EOF + truncate to 14.
        patch.extend_from_slice(b"EOF");
        patch.extend_from_slice(&be3(14));

        let out = apply_ips(&rom, &patch).unwrap();
        assert_eq!(out.len(), 14);
        assert_eq!(&out[2..5], &[0xAA, 0xBB, 0xCC]);
        assert_eq!(&out[8..12], &[0x77, 0x77, 0x77, 0x77]);
        assert_eq!(out[0], 0);
        // Auto-detect path agrees.
        assert_eq!(apply_patch(&rom, &patch).unwrap(), out);
    }

    /// Build a UPS patch that transforms `src` into `tgt` (same length assumed
    /// for simplicity of the fixture; the applier handles differing sizes).
    fn build_ups(src: &[u8], tgt: &[u8]) -> Vec<u8> {
        let mut body = b"UPS1".to_vec();
        // in/out sizes as varints.
        push_vuint(&mut body, src.len() as u64);
        push_vuint(&mut body, tgt.len() as u64);
        let n = src.len().max(tgt.len());
        let mut i = 0usize;
        // `out_pos` in the applier after the previous chunk (it advances one past
        // the terminating 0 byte); the skip is relative to that.
        let mut cursor = 0usize;
        while i < n {
            let sb = src.get(i).copied().unwrap_or(0);
            let tb = tgt.get(i).copied().unwrap_or(0);
            if sb == tb {
                i += 1;
                continue;
            }
            push_vuint(&mut body, (i - cursor) as u64);
            // XOR bytes until they realign (and always emit a terminating 0).
            while i < n {
                let sb = src.get(i).copied().unwrap_or(0);
                let tb = tgt.get(i).copied().unwrap_or(0);
                body.push(sb ^ tb);
                i += 1;
                if sb ^ tb == 0 {
                    break;
                }
            }
            cursor = i;
        }
        body.extend_from_slice(&crc32(src).to_le_bytes());
        body.extend_from_slice(&crc32(tgt).to_le_bytes());
        let pcrc = crc32(&body);
        body.extend_from_slice(&pcrc.to_le_bytes());
        body
    }

    fn push_vuint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let b = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(b | 0x80);
                break;
            }
            out.push(b);
            value -= 1;
        }
    }

    #[test]
    fn ups_roundtrip() {
        let src: Vec<u8> = (0..64u8).collect();
        let mut tgt = src.clone();
        tgt[10] = 0xFF;
        tgt[11] = 0xEE;
        tgt[40] = 0x01;
        let patch = build_ups(&src, &tgt);
        let out = apply_patch(&src, &patch).unwrap();
        assert_eq!(out, tgt);
    }

    #[test]
    fn ups_bad_source_errors() {
        let src: Vec<u8> = (0..64u8).collect();
        let mut tgt = src.clone();
        tgt[0] = 0x99;
        let patch = build_ups(&src, &tgt);
        let wrong: Vec<u8> = (1..65u8).collect();
        assert!(apply_patch(&wrong, &patch).is_err());
    }

    /// Build a minimal BPS patch using only TargetRead over the whole target
    /// (a valid, if unoptimized, BPS encoding), plus correct trailing CRCs.
    fn build_bps_targetread(src: &[u8], tgt: &[u8]) -> Vec<u8> {
        let mut body = b"BPS1".to_vec();
        push_vuint(&mut body, src.len() as u64);
        push_vuint(&mut body, tgt.len() as u64);
        push_vuint(&mut body, 0); // no metadata
        // Single TargetRead action covering the entire target.
        let cmd = ((tgt.len() as u64 - 1) << 2) | BPS_TARGET_READ;
        push_vuint(&mut body, cmd);
        body.extend_from_slice(tgt);
        body.extend_from_slice(&crc32(src).to_le_bytes());
        body.extend_from_slice(&crc32(tgt).to_le_bytes());
        let pcrc = crc32(&body);
        body.extend_from_slice(&pcrc.to_le_bytes());
        body
    }

    /// Build a BPS that SourceCopies the whole source (identity), exercising the
    /// SourceRead/SourceCopy path.
    fn build_bps_sourceread(src: &[u8]) -> Vec<u8> {
        let mut body = b"BPS1".to_vec();
        push_vuint(&mut body, src.len() as u64);
        push_vuint(&mut body, src.len() as u64);
        push_vuint(&mut body, 0);
        let cmd = ((src.len() as u64 - 1) << 2) | BPS_SOURCE_READ;
        push_vuint(&mut body, cmd);
        body.extend_from_slice(&crc32(src).to_le_bytes());
        body.extend_from_slice(&crc32(src).to_le_bytes());
        let pcrc = crc32(&body);
        body.extend_from_slice(&pcrc.to_le_bytes());
        body
    }

    #[test]
    fn bps_targetread_roundtrip() {
        let src: Vec<u8> = (0..100u8).collect();
        let tgt: Vec<u8> = (0..100u8).rev().collect();
        let patch = build_bps_targetread(&src, &tgt);
        let out = apply_patch(&src, &patch).unwrap();
        assert_eq!(out, tgt);
    }

    #[test]
    fn bps_sourceread_identity() {
        let src: Vec<u8> = (0..80u8).collect();
        let patch = build_bps_sourceread(&src);
        let out = apply_patch(&src, &patch).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn bps_bad_target_crc_errors() {
        let src: Vec<u8> = (0..100u8).collect();
        let tgt: Vec<u8> = (0..100u8).rev().collect();
        let mut patch = build_bps_targetread(&src, &tgt);
        // Corrupt the stored target CRC (bytes at len-8..len-4).
        let n = patch.len();
        patch[n - 8] ^= 0xFF;
        let err = apply_patch(&src, &patch).unwrap_err();
        assert!(err.contains("target CRC"), "unexpected error: {err}");
    }

    #[test]
    fn unknown_format_errors() {
        assert!(apply_patch(&[0u8; 4], b"NOPE").is_err());
    }
}
