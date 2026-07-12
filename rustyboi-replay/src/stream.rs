//! Shared codec plumbing: LEB128 varints, the brotli entropy layer, and the
//! streaming decompression `Source` both container formats decode through.

use std::io::Read;
use std::sync::Arc;

#[cfg(feature = "encode")]
pub(crate) fn write_varint(out: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// The entropy layer: brotli q10/lgwin22 — q10 is ~2-4x faster than q11 for
/// nearly identical size on these streams, and a 4MB window keeps the measured
/// cross-frame win (>=1MB suffices).
#[cfg(feature = "encode")]
pub(crate) fn brotli_compress_into(stream: &[u8], out: &mut Vec<u8>) {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 10,
        lgwin: 22,
        ..Default::default()
    };
    let mut reader = stream;
    brotli::BrotliCompress(&mut reader, out, &params)
        .unwrap_or_else(|e| unreachable!("in-memory brotli cannot fail: {e}"));
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    BadMagic,
    Truncated,
    Malformed,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DecodeError::BadMagic => "not an RBR1 recording",
            DecodeError::Truncated => "recording truncated",
            DecodeError::Malformed => "recording malformed",
        };
        f.write_str(s)
    }
}

impl std::error::Error for DecodeError {}

/// Streaming brotli window over the compressed frame stream, so memory stays
/// small even when the decompressed stream is tens of MB. The compressed tail
/// is shared via `Arc` so `reset()` (loop restart) is allocation-cheap.
pub(crate) struct Source {
    dec: brotli_decompressor::Decompressor<std::io::Cursor<Arc<[u8]>>>,
    buf: Vec<u8>, // decompressed-but-unconsumed window
    start: usize, // consumed prefix of `buf`
    done: bool,
}

impl Source {
    pub(crate) fn new(compressed: Arc<[u8]>) -> Self {
        Source {
            dec: brotli_decompressor::Decompressor::new(std::io::Cursor::new(compressed), 16 << 10),
            buf: Vec::new(),
            start: 0,
            done: false,
        }
    }
}

/// Pull exactly `n` contiguous frame-stream bytes, decompressing more input on
/// demand and compacting the consumed prefix so the window never grows past the
/// largest single read (one raw frame block at most).
pub(crate) fn src_take(src: &mut Source, n: usize) -> Result<&[u8], DecodeError> {
    if src.start > (64 << 10) {
        src.buf.drain(..src.start);
        src.start = 0;
    }
    while src.buf.len() - src.start < n {
        if src.done {
            return Err(DecodeError::Truncated);
        }
        let mut chunk = [0u8; 32 << 10];
        match src.dec.read(&mut chunk) {
            Ok(0) => src.done = true,
            Ok(got) => src.buf.extend_from_slice(&chunk[..got]),
            Err(_) => return Err(DecodeError::Malformed),
        }
    }
    let s = &src.buf[src.start..src.start + n];
    src.start += n;
    Ok(s)
}

pub(crate) fn src_byte(src: &mut Source) -> Result<u8, DecodeError> {
    Ok(src_take(src, 1)?[0])
}

pub(crate) fn src_varint(src: &mut Source) -> Result<u32, DecodeError> {
    let mut v: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = src_byte(src)?;
        v |= u32::from(byte & 0x7f).checked_shl(shift).ok_or(DecodeError::Malformed)?;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift >= 32 {
            return Err(DecodeError::Malformed);
        }
    }
}

/// Read `(skip, lit)*` ops and XOR the literal runs onto `target`, covering
/// exactly `target.len()` bytes. Shared by the delta and motion decoders.
pub(crate) fn apply_sparse_src(src: &mut Source, target: &mut [u8]) -> Result<(), DecodeError> {
    let n = target.len();
    let mut i = 0usize;
    while i < n {
        let skip = src_varint(src)? as usize;
        i = i.checked_add(skip).ok_or(DecodeError::Malformed)?;
        let lit = src_varint(src)? as usize;
        let stop = i.checked_add(lit).ok_or(DecodeError::Malformed)?;
        if stop > n {
            return Err(DecodeError::Malformed);
        }
        let payload = src_take(src, lit)?;
        for (c, d) in target[i..stop].iter_mut().zip(payload) {
            *c ^= d;
        }
        i = stop;
    }
    if i != n {
        return Err(DecodeError::Malformed);
    }
    Ok(())
}
