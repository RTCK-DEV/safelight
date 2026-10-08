//! Nikon HE / HE* ("TicoRAW", JPEG-XS-family) Bayer sensor decode,
//! implemented from the wire format: precinct stream of wavelet sub-bands →
//! entropy coding (significance + GCLI + bit-plane magnitudes + signs) →
//! 5/3 horizontal IDWT → precinct-spanning vertical lift → 2D Bayer merge
//! with a fixed tone LUT. Written against the codestream itself.
//!
//! Nikon profile (from stream inspection): 4 CFA components, 26 sub-bands
//! in 8 line-blocks per precinct, 18 precincts per 64-row tile with a
//! 2-precinct overlap between tiles. Output is the raw Bayer mosaic
//! (14-bit) which enters the normal develop pipeline.

use anyhow::{bail, Result};
use std::path::Path;

const SUBBANDS: usize = 26;
const LBS: usize = 8;
const DPB: usize = 28;
const PRECINCTS_PER_TILE: usize = 18;
const STRIPES_PER_TILE: usize = 32;
const LUT_SIZE: usize = 81792;
const LUT_SHIFT: i32 = 2;
const CLIP_MAX: i32 = 16383;

// ---------- NEF (little-endian TIFF) container ----------

/// Find the JPEG-XS codestream: the SubIFD with Compression=0x8799 whose
/// strip begins with the SOC marker.
fn find_jxs_strip(d: &[u8]) -> Option<(usize, usize)> {
    if d.len() < 8 || &d[..2] != b"II" {
        return None;
    }
    let u16 = |o: usize| -> usize {
        d.get(o..o + 2)
            .map(|b| b[0] as usize | ((b[1] as usize) << 8))
            .unwrap_or(0)
    };
    let u32 = |o: usize| -> usize {
        d.get(o..o + 4)
            .map(|b| {
                b[0] as usize
                    | ((b[1] as usize) << 8)
                    | ((b[2] as usize) << 16)
                    | ((b[3] as usize) << 24)
            })
            .unwrap_or(0)
    };
    let mut stack = vec![u32(4)];
    for _ in 0..64 {
        let Some(ifd) = stack.pop() else { break };
        if ifd + 2 > d.len() {
            continue;
        }
        let n = u16(ifd);
        let (mut comp, mut off, mut size) = (0usize, 0usize, 0usize);
        let (mut subptr, mut subcnt) = (0usize, 0usize);
        for i in 0..n {
            let e = ifd + 2 + i * 12;
            if e + 12 > d.len() {
                break;
            }
            let (tag, cnt, val) = (u16(e), u32(e + 4), u32(e + 8));
            match tag {
                0x103 => comp = val & 0xffff,
                0x111 => off = if cnt == 1 { val } else { u32(val) },
                0x117 => size = if cnt == 1 { val } else { u32(val) },
                0x14a => {
                    subptr = val;
                    subcnt = cnt;
                }
                _ => {}
            }
        }
        if comp == 0x8799 && off > 0 && size > 64 && off + size <= d.len() {
            if d.get(off) == Some(&0xff) && d.get(off + 1) == Some(&0x10) {
                return Some((off, size));
            }
        }
        for j in 0..subcnt.min(16) {
            let p = subptr + j * 4;
            if p + 4 <= d.len() {
                stack.push(u32(p));
            }
        }
        let next = ifd + 2 + n * 12;
        if next + 4 <= d.len() && u32(next) != 0 {
            stack.push(u32(next));
        }
    }
    None
}

// ---------- codestream header ----------

fn be16(d: &[u8], o: usize) -> usize {
    ((d[o] as usize) << 8) | d[o + 1] as usize
}
fn be24(d: &[u8], o: usize) -> usize {
    ((d[o] as usize) << 16) | ((d[o + 1] as usize) << 8) | d[o + 2] as usize
}
fn be32(d: &[u8], o: usize) -> usize {
    ((d[o] as usize) << 24)
        | ((d[o + 1] as usize) << 16)
        | ((d[o + 2] as usize) << 8)
        | d[o + 3] as usize
}

struct PicHeader {
    w: usize,
    h: usize,
    nbands: usize,
    gain: [u8; 64],
    priority: [u8; 64],
    precinct_off: usize,
}

fn parse_pic_header(strip: &[u8]) -> Result<PicHeader> {
    if strip.len() < 8 || be16(strip, 0) != 0xff10 {
        bail!("no SOC");
    }
    let mut ph = PicHeader {
        w: 0,
        h: 0,
        nbands: 0,
        gain: [0; 64],
        priority: [0; 64],
        precinct_off: 0,
    };
    let (mut cap, mut pih, mut cdt, mut wgt) = (false, false, false, false);
    let mut i = 2usize;
    while i + 4 <= strip.len() {
        let m = be16(strip, i);
        if (m & 0xff00) != 0xff00 || m == 0xff11 {
            break;
        }
        let lseg = be16(strip, i + 2);
        if lseg < 2 || i + 2 + lseg > strip.len() {
            bail!("bad marker len");
        }
        let body = &strip[i + 4..i + 2 + lseg];
        match m {
            0xff50 => {
                if cap || body.len() < 16 || &body[..16] != b"CONTACT_INTOPIX_" {
                    bail!("bad CAP");
                }
                cap = true;
            }
            0xff12 => {
                if pih || body.len() < 20 {
                    bail!("bad PIH");
                }
                let lcod = be32(body, 0);
                ph.w = be16(body, 8);
                ph.h = be16(body, 10);
                let (ppw, hsl) = (be16(body, 12), be16(body, 14));
                let (nc, cg, sg, bw) = (body[16], body[17], body[18], body[19]);
                if lcod != strip.len()
                    || ph.w == 0
                    || ph.w & 1 != 0
                    || ph.h == 0
                    || ppw != 0
                    || hsl != 16
                    || nc != 4
                    || cg != 4
                    || sg != 8
                    || bw != 18
                {
                    bail!("unsupported PIH profile");
                }
                pih = true;
            }
            0xff13 => {
                if cdt || body.is_empty() {
                    bail!("bad CDT");
                }
                cdt = true;
            }
            0xff14 => {
                if wgt || body.len() % 2 != 0 {
                    bail!("bad WGT");
                }
                ph.nbands = body.len() / 2;
                if ph.nbands == 0 || ph.nbands > 64 {
                    bail!("bad WGT count");
                }
                for b in 0..ph.nbands {
                    ph.gain[b] = body[2 * b];
                    ph.priority[b] = body[2 * b + 1];
                }
                wgt = true;
            }
            0xff16 => bail!("NLT unsupported"),
            0xff20 => {
                if !(cap && pih && cdt && wgt) {
                    bail!("missing markers");
                }
                ph.precinct_off = i + 2 + lseg;
                return Ok(ph);
            }
            _ => {}
        }
        i += 2 + lseg;
    }
    bail!("no SLH marker");
}

/// Truncation level per sub-band from WGT weights:
// Captured (Bp, Br) -> 26-band GTLI ground truth (transcribed from the
// reference decoder; the weight formula remains as fallback for
// (Bp, Br) combos not in this table).
const GTLI_TABLE: &[(i32, i32, [u8; 26])] = &[
    (
        4,
        0,
        [
            1, 1, 2, 2, 3, 3, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        1,
        [
            1, 1, 2, 2, 3, 3, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        2,
        [
            1, 1, 2, 2, 3, 3, 2, 3, 3, 3, 4, 4, 4, 2, 3, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        3,
        [
            0, 1, 2, 2, 3, 3, 2, 3, 3, 3, 4, 4, 4, 2, 3, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        4,
        [
            0, 1, 2, 2, 3, 3, 2, 2, 3, 3, 4, 4, 4, 2, 3, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        5,
        [
            0, 1, 2, 2, 3, 3, 2, 2, 3, 3, 4, 4, 4, 2, 2, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        6,
        [
            0, 1, 2, 2, 3, 3, 1, 2, 3, 3, 4, 4, 4, 2, 2, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        7,
        [
            0, 1, 2, 2, 3, 3, 1, 2, 3, 3, 4, 4, 4, 1, 2, 3, 3, 4, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        11,
        [
            0, 1, 1, 2, 2, 3, 1, 2, 3, 3, 3, 4, 4, 1, 2, 3, 3, 3, 4, 3, 4, 4, 4, 4, 4, 4,
        ],
    ),
    (
        4,
        12,
        [
            0, 1, 1, 2, 2, 3, 1, 2, 3, 3, 3, 4, 3, 1, 2, 3, 3, 3, 4, 3, 4, 4, 4, 3, 4, 4,
        ],
    ),
    (
        5,
        12,
        [
            1, 2, 2, 3, 3, 4, 2, 3, 4, 4, 4, 5, 4, 2, 3, 4, 4, 4, 5, 4, 5, 5, 5, 4, 5, 5,
        ],
    ),
    (
        5,
        13,
        [
            1, 2, 2, 3, 3, 4, 2, 3, 4, 4, 4, 5, 4, 2, 3, 4, 4, 4, 5, 4, 5, 5, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        14,
        [
            1, 2, 2, 3, 3, 4, 2, 3, 4, 4, 4, 4, 4, 2, 3, 4, 4, 4, 5, 4, 5, 5, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        15,
        [
            1, 2, 2, 3, 3, 4, 2, 3, 4, 4, 4, 4, 4, 2, 3, 4, 4, 4, 4, 4, 5, 5, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        16,
        [
            1, 2, 2, 3, 3, 4, 2, 3, 4, 4, 4, 4, 4, 2, 3, 4, 4, 4, 4, 4, 5, 4, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        20,
        [
            1, 1, 2, 3, 3, 4, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 4, 4, 4, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        21,
        [
            1, 1, 2, 3, 3, 4, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 3, 4, 4, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        22,
        [
            1, 1, 2, 3, 3, 3, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 3, 4, 4, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        23,
        [
            1, 1, 2, 2, 3, 3, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 3, 4, 4, 5, 4, 4, 5,
        ],
    ),
    (
        5,
        24,
        [
            1, 1, 2, 2, 3, 3, 2, 3, 3, 4, 4, 4, 4, 2, 3, 3, 4, 4, 4, 3, 4, 4, 4, 4, 4, 5,
        ],
    ),
    (
        1,
        0,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        ],
    ),
    (
        1,
        1,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        ],
    ),
    (
        1,
        7,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        ],
    ),
    (
        1,
        8,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 1, 0, 1, 1, 1, 1, 1, 1,
        ],
    ),
    (
        1,
        11,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 1, 0, 1, 1, 1, 1, 1, 1,
        ],
    ),
    (
        2,
        0,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        1,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 1, 2, 2, 2, 2, 0, 0, 1, 1, 2, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        3,
        [
            0, 0, 0, 0, 1, 1, 0, 1, 1, 2, 2, 2, 2, 0, 0, 1, 1, 2, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        4,
        [
            0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 2, 0, 0, 1, 1, 2, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        7,
        [
            0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 2, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        8,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 2, 2, 2, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        10,
        [
            0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 1, 2, 2, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        11,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 2, 2, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 2, 2, 2,
        ],
    ),
    (
        2,
        12,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 2, 1, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 1, 2, 2,
        ],
    ),
    (
        2,
        13,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 2, 1, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        14,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1, 2, 1, 2, 2, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        15,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        16,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 1, 2, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        17,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 2, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        18,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 2, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        20,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 1, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        21,
        [
            0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        23,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 2, 1, 1, 2,
        ],
    ),
    (
        2,
        24,
        [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1, 2,
        ],
    ),
    (
        3,
        8,
        [
            0, 0, 1, 1, 2, 2, 0, 1, 2, 2, 2, 3, 3, 0, 1, 2, 2, 3, 3, 2, 3, 3, 3, 3, 3, 3,
        ],
    ),
    (
        3,
        11,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 3, 3, 0, 1, 2, 2, 2, 3, 2, 3, 3, 3, 3, 3, 3,
        ],
    ),
    (
        3,
        12,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 3, 2, 0, 1, 2, 2, 2, 3, 2, 3, 3, 3, 2, 3, 3,
        ],
    ),
    (
        3,
        13,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 3, 2, 0, 1, 2, 2, 2, 3, 2, 3, 3, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        14,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 2, 2, 0, 1, 2, 2, 2, 3, 2, 3, 3, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        15,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 2, 2, 0, 1, 2, 2, 2, 2, 2, 3, 3, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        16,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 2, 2, 0, 1, 2, 2, 2, 2, 2, 3, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        17,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 2, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 2, 3, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        18,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 2, 3, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        20,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 2, 2, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        21,
        [
            0, 0, 0, 1, 1, 2, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 1, 2, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        22,
        [
            0, 0, 0, 1, 1, 1, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 1, 2, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        23,
        [
            0, 0, 0, 0, 1, 1, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 1, 2, 2, 3, 2, 2, 3,
        ],
    ),
    (
        3,
        24,
        [
            0, 0, 0, 0, 1, 1, 0, 1, 1, 2, 2, 2, 2, 0, 1, 1, 2, 2, 2, 1, 2, 2, 2, 2, 2, 3,
        ],
    ),
];

/// T = clamp(Bp - gain[b] - (priority[b] < Br), 0, 15); band 23 (pass-B LL)
/// shares band 12's WGT entry.
fn gtli(ph: &PicHeader, sb: usize, bp: i32, br: i32) -> i32 {
    // Captured table wins over the weight formula when the combo is known
    // — the formula only approximates the encoder's actual quantization
    // (e.g. (2,7) gives sb17=1 in the table but 2 by formula).
    for &(tbp, tbr, ref vals) in GTLI_TABLE {
        if tbp == bp && tbr == br {
            return vals[sb] as i32;
        }
    }
    let w = if sb < 23 {
        sb
    } else if sb == 23 {
        12
    } else {
        sb - 1
    };
    if w >= ph.nbands {
        return 0;
    }
    (bp - ph.gain[w] as i32 - ((ph.priority[w] as i32) < br) as i32).clamp(0, 15)
}

// ---------- MSB-first bit reader ----------

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    reg: u64,
    avail: i32,
}
impl<'a> Bits<'a> {
    fn new(d: &'a [u8]) -> Self {
        Self {
            d,
            pos: 0,
            reg: 0,
            avail: 0,
        }
    }
    fn refill(&mut self) {
        let rem = self.d.len() - self.pos;
        if rem == 0 {
            return;
        }
        let take = rem.min(4);
        let mut w = 0u32;
        for i in 0..take {
            w = (w << 8) | self.d[self.pos + i] as u32;
        }
        w <<= (4 - take) * 8;
        self.pos += take;
        self.reg |= (w as u64) << (32 - self.avail);
        self.avail += (take * 8) as i32;
    }
    fn get(&mut self, n: i32) -> u32 {
        while self.avail < n {
            if self.pos >= self.d.len() {
                let r = (self.reg >> (64 - n)) as u32;
                self.reg = 0;
                self.avail = 0;
                return r;
            }
            self.refill();
        }
        let r = (self.reg >> (64 - n)) as u32;
        self.reg <<= n;
        self.avail -= n;
        r
    }
    fn unary(&mut self) -> u32 {
        let mut n = 0u32;
        loop {
            if self.avail <= 0 {
                if self.pos >= self.d.len() {
                    return n;
                }
                self.refill();
            }
            let one = (self.reg >> 63) & 1 != 0;
            self.reg <<= 1;
            self.avail -= 1;
            if !one {
                return n;
            }
            n += 1;
        }
    }
}

// ---------- sub-band layout ----------

fn round_up(x: usize, m: usize) -> usize {
    x.div_ceil(m) * m
}

#[derive(Clone, Copy, Default)]
struct Sb {
    ng: usize,
    x24: usize,
    buf: usize,
}

struct Layout {
    ng_max: usize,
    ng_ll: usize,
    ng_lift: [usize; 6],
    memcpy_lb: usize,
    lift_lb: usize,
    sb: [Sb; SUBBANDS],
}

fn compute_layout(half_w: usize) -> Layout {
    let ng_max = half_w.div_ceil(8);
    let ng_ll = half_w / 4;
    let n4 = ng_max.div_ceil(2);
    let n5 = n4.div_ceil(2);
    let ng5 = [
        n5.div_ceil(4),
        (n4 - n5).div_ceil(4),
        ng_max.div_ceil(8),
        ng_max.div_ceil(4),
        ng_max.div_ceil(2),
        ng_max,
    ];
    let memcpy_lb = round_up(ng_ll, 32);
    let lift_lb = memcpy_lb + round_up(ng_max.div_ceil(16), 32);

    // LB per sb: 0-5→0, 6-11→1, 12→2, 13-18→3, 19-20→4, 21-22→5, 23→6, 24-25→7
    const LB_OF: [usize; SUBBANDS] = [
        0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 2, 3, 3, 3, 3, 3, 3, 4, 4, 5, 5, 6, 7, 7,
    ];
    let mut sb = [Sb::default(); SUBBANDS];
    let mut lb_cursor = [0usize; LBS];
    for s in 0..SUBBANDS {
        let lbi = LB_OF[s];
        // 5-level lift bands carry ng5[0..6] indexed from their LB start
        // (LB0→0, LB1→6, LB3→13); `s % 6` only happens to work for LB 0/1.
        let ng = match lbi {
            0 | 1 | 3 => ng5[s - [0usize, 6, 0, 13][lbi]],
            2 | 6 => ng_ll,
            _ => ng_max,
        };
        let base = [0usize, lift_lb, 2 * lift_lb, 2 * lift_lb + memcpy_lb][lbi % 4];
        sb[s] = Sb {
            ng,
            x24: base + lb_cursor[lbi],
            buf: (lbi >= 4) as usize,
        };
        lb_cursor[lbi] += round_up(ng, 8);
    }
    Layout {
        ng_max,
        ng_ll,
        ng_lift: ng5,
        memcpy_lb,
        lift_lb,
        sb,
    }
}

// ---------- GCLI prediction ----------

/// predict(gtli, prev, unary): baseline max(prev, gtli), then zigzag
/// ±1,±2,… for u ≤ 2·(m_top − gtli), else one-sided escape.
fn predict_gcli(gtli: i32, prev: i32, u: u32) -> i32 {
    let m_top = gtli.max(prev);
    let thr = m_top - gtli;
    let max_stored = 15 + thr;
    let delta = if u == 0 {
        0
    } else if (u as i32) <= 2 * thr {
        if u & 1 == 1 {
            -((u as i32 + 1) / 2)
        } else {
            (u / 2) as i32
        }
    } else {
        u as i32 - thr
    };
    let g = m_top + delta;
    if g > max_stored {
        0xff
    } else {
        g
    }
}

// ---------- entropy decode ----------

const MIDPOINT_SCALE: [i32; 16] = [
    87381, 74898, 69905, 67650, 66576, 66052, 65793, 65664, 65600, 65568, 65552, 65544, 65540,
    65538, 65537, 0,
];
const W4: i32 = 4;

/// LB → sub-band range.
const LB_SB: [(usize, usize); LBS] = [
    (0, 6),
    (6, 12),
    (12, 13),
    (13, 19),
    (19, 21),
    (21, 23),
    (23, 24),
    (24, 26),
];

/// Cross-precinct GCLI prediction context: sb 12 (pass-A LL) predicts from
/// the previous precinct's sb 23; sb 23 (pass-B LL) predicts from the
/// current precinct's sb 12. Every other band predicts from its own
/// previous-precinct GCLI history.
struct Pred {
    index: usize,
    store: Vec<Vec<u8>>, // per-sub-band previous GCLIs
    cur12: Vec<u8>,      // this precinct's sb 12 (consumed by sb 23)
}

impl Pred {
    fn new(lay: &Layout) -> Self {
        Self {
            index: 0,
            store: lay.sb.iter().map(|s| vec![0u8; s.ng]).collect(),
            cur12: vec![0u8; lay.sb[12].ng],
        }
    }
    /// The GCLI history a mode-0x73 band predicts against.
    fn prev(&self, sb: usize) -> &[u8] {
        match sb {
            12 => &self.store[23],
            23 => &self.cur12,
            s => &self.store[s],
        }
    }
    /// Persist a decoded band's GCLIs for future prediction.
    fn save(&mut self, sb: usize, gcli: &[u8]) {
        match sb {
            12 => self.cur12.copy_from_slice(gcli),
            s => self.store[s].copy_from_slice(gcli),
        }
    }
    fn reset(&mut self) {
        for s in &mut self.store {
            s.fill(0);
        }
        self.cur12.fill(0);
    }
}

fn decode_precinct(
    d: &[u8],
    img_w: usize,
    lay: &Layout,
    ph: &PicHeader,
    pred: &mut Pred,
    buf_a: &mut [i32],
    buf_b: &mut [i32],
) -> Result<()> {
    if d.len() < 19 {
        bail!("short precinct");
    }
    let (bp, br) = (d[3] as i32, d[4] as i32);
    let mut dpb = [0u8; DPB];
    for i in 0..7 {
        let b = d[5 + i];
        for j in 0..4 {
            if i * 4 + j < DPB {
                dpb[i * 4 + j] = (b >> (6 - 2 * j)) & 3;
            }
        }
    }
    let f20 = (img_w / 2).div_ceil(256);
    // 8 LB blocks interleaved with their substreams:
    //   7-byte header: [1b f20_sign | 20b data_len | 20b gcli_len | 15b sign_len]
    //   then sig(f20 bytes) + gcli + data + sign
    let mut lb_seg = [(0usize, 0usize, 0usize, 0usize); LBS];
    let mut cur = 12usize;
    for lb in 0..LBS {
        if cur + 7 > d.len() {
            bail!("short LB header");
        }
        let mut v = 0u64;
        for i in 0..7 {
            v = (v << 8) | d[cur + i] as u64;
        }
        let data_len = ((v >> 35) & 0xF_FFFF) as usize;
        let gcli_len = ((v >> 15) & 0xF_FFFF) as usize;
        let sign_len = (v & 0x7FFF) as usize;
        cur += 7;
        let end = cur + f20 + gcli_len + data_len + sign_len;
        if end > d.len() {
            bail!("LB overrun");
        }
        lb_seg[lb] = (cur, gcli_len, data_len, sign_len);
        cur = end;
    }
    // precinct 16 (0-based) resets the GCLI prediction context
    if pred.index == 16 {
        pred.reset();
    }
    for lb in 0..LBS {
        let (seg, gcli_len, data_len, sign_len) = lb_seg[lb];
        let mut sig = Bits::new(&d[seg..seg + f20]);
        let mut gcli_r = Bits::new(&d[seg + f20..seg + f20 + gcli_len]);
        let data_off = seg + f20 + gcli_len;
        let mut data_r = Bits::new(&d[data_off..data_off + data_len]);
        let sign_off = data_off + data_len;
        let mut sign_r = Bits::new(&d[sign_off..sign_off + sign_len]);

        let (sb_lo, sb_hi) = LB_SB[lb];
        for s in sb_lo..sb_hi {
            let ng = lay.sb[s].ng;
            if ng == 0 {
                continue;
            }
            let mode = 0x70 | dpb[s] as i32; // 0x71 zero-pred, 0x73 predecessor
            let gtli = gtli(ph, s, bp, br);

            // -- GCLI per coefficient group (4 coeffs per group) --
            let mut gcli = vec![0u8; ng];
            for blk in 0..ng.div_ceil(8) {
                let base = blk * 8;
                let n = (ng - base).min(8);
                if sig.get(1) == 1 {
                    for i in 0..n {
                        gcli[base + i] = if mode == 0x71 {
                            gtli as u8
                        } else {
                            pred.prev(s)
                                .get(base + i)
                                .copied()
                                .unwrap_or(0)
                                .max(gtli as u8)
                        };
                    }
                } else {
                    for i in 0..n {
                        let u = gcli_r.unary();
                        gcli[base + i] = if mode == 0x71 {
                            (gtli + u as i32).clamp(0, 255) as u8
                        } else {
                            let prev = pred.prev(s).get(base + i).copied().unwrap_or(0) as i32;
                            predict_gcli(gtli, prev, u).clamp(0, 255) as u8
                        };
                    }
                }
            }

            // -- magnitudes: one nibble per bit-plane, MSB first --
            let mut coef = vec![0i32; ng * 4];
            for g in 0..ng {
                let planes = gcli[g] as i32 - gtli;
                if planes <= 0 {
                    continue;
                }
                let mut m = [0i32; 4];
                for _ in 0..planes {
                    let nib = data_r.get(4) as i32;
                    for c in 0..4 {
                        m[c] = (m[c] << 1) | ((nib >> (3 - c)) & 1);
                    }
                }
                for c in 0..4 {
                    coef[g * 4 + c] = m[c] << gtli;
                }
            }
            // -- signs: one bit per non-zero coefficient --
            for c in coef.iter_mut() {
                if *c != 0 && sign_r.get(1) == 1 {
                    *c = -*c;
                }
            }
            // -- dequantize: deadzone-midpoint scale, << W4 --
            for g in 0..ng {
                let gc = gcli[g] as i32;
                let bpc = gc - gtli;
                for c in 0..4 {
                    let i = g * 4 + c;
                    let v = coef[i];
                    if v == 0 || gc <= gtli || !(1..=15).contains(&bpc) {
                        coef[i] = 0;
                        continue;
                    }
                    let mag = (v.unsigned_abs() >> gtli) as u64;
                    let scaled = (((mag * MIDPOINT_SCALE[(bpc - 1) as usize] as u64) >> (16 - gtli))
                        as i32)
                        << W4;
                    coef[i] = if v < 0 { -scaled } else { scaled };
                }
            }
            pred.save(s, &gcli);
            let dst = if lay.sb[s].buf == 0 {
                &mut *buf_a
            } else {
                &mut *buf_b
            };
            let off = lay.sb[s].x24 * 4;
            dst[off..off + ng * 4].copy_from_slice(&coef);
        }
    }
    pred.index += 1;
    pred.cur12.fill(0); // sb23 consumes this precinct's sb12 exactly once
    Ok(())
}

// ---------- horizontal 5/3 IDWT ----------

fn idwt_level(l: &[i32], h: &[i32], out: &mut [i32]) {
    let n = l.len() + h.len();
    if n == 0 {
        return;
    }
    for (i, &v) in l.iter().enumerate() {
        if 2 * i < n {
            out[2 * i] = v;
        }
    }
    for (i, &v) in h.iter().enumerate() {
        if 2 * i + 1 < n {
            out[2 * i + 1] = v;
        }
    }
    for i in (0..n).step_by(2) {
        let left = if i > 0 {
            out[i - 1]
        } else if n > 1 {
            out[1]
        } else {
            0
        };
        let right = if i + 1 < n {
            out[i + 1]
        } else if i > 0 {
            out[i - 1]
        } else {
            0
        };
        out[i] -= (left + right + 2) >> 2;
    }
    for i in (1..n).step_by(2) {
        let right = if i + 1 < n { out[i + 1] } else { out[i - 1] };
        out[i] += (out[i - 1] + right) >> 1;
    }
}

/// Multi-level lift for one LB region; `hl[k]` = int offset of the level-k
/// high-pass band inside `src` (hl[0] is a sentinel, never read).
fn idwt_lb(src: &[i32], n: usize, levels: usize, hl: &[usize], out: &mut [i32], work: &mut [i32]) {
    if levels == 0 || n == 0 {
        return;
    }
    let mut ns = vec![0usize; levels + 1];
    ns[0] = n;
    for k in 1..=levels {
        ns[k] = ns[k - 1].div_ceil(2);
    }
    let mut in_out = levels % 2 == 0;
    let ll = ns[levels];
    if in_out {
        out[..ll].copy_from_slice(&src[..ll]);
    } else {
        work[..ll].copy_from_slice(&src[..ll]);
    }
    for k in (1..=levels).rev() {
        let (n_l, n_h) = (ns[k], ns[k - 1] - ns[k]);
        let h = &src[hl[k]..hl[k] + n_h];
        if in_out {
            idwt_level(&out[..n_l], h, work);
        } else {
            idwt_level(&work[..n_l], h, out);
        }
        in_out = !in_out;
    }
    if !in_out {
        out[..n].copy_from_slice(&work[..n]);
    }
}

/// Horizontal IDWT for one precinct pass. Out layout (ints):
/// [0..lift) lift-LB a, [lift..2lift) lift-LB b, [2lift..2lift+mc) memcpy
/// LL, [2lift+mc..) lift-LB c.
fn idwt_pass(buf: &[i32], lay: &Layout, pass_a: bool, out: &mut [i32], work: &mut [i32]) {
    let lift_st = lay.lift_lb * 4;
    let memcpy_st = lay.memcpy_lb * 4;
    let n = lay.ng_ll * 4;
    let levels = if pass_a { 5 } else { 1 };
    // band offsets: cum[k] = groups used by bands 0..k (exclusive)
    let mut hl = [0usize; 6];
    if pass_a {
        let mut cum = [0usize; 8];
        for i in 0..=5 {
            cum[i + 1] = cum[i] + round_up(lay.ng_lift[i], 8);
        }
        for k in 0..=5 {
            hl[k] = 4 * cum[6 - k];
        }
    } else {
        hl[0] = 8 * round_up(lay.ng_max, 8);
        hl[1] = 4 * round_up(lay.ng_max, 8);
    }
    for slot in [0usize, 1, 3] {
        let off = if slot == 3 {
            2 * lift_st + memcpy_st
        } else {
            slot * lift_st
        };
        idwt_lb(&buf[off..], n, levels, &hl, &mut out[off..off + n], work);
    }
    let ll_sb = if pass_a { 12 } else { 23 };
    let src = lay.sb[ll_sb].x24 * 4;
    out[2 * lift_st..2 * lift_st + memcpy_st].copy_from_slice(&buf[src..src + memcpy_st]);
}

// ---------- vertical lift (cross-precinct state machine) ----------

/// Per-LB-column lift state; tile 0 enters at state 2, later tiles at 0.
/// States ≥ 7 emit a stripe; the tail flushes 7→9→11.
struct VlState {
    st: i32,
    x2: Vec<i32>,
    x3: Vec<i32>,
}

impl VlState {
    fn new(n: usize, first: bool) -> Self {
        Self {
            st: if first { 2 } else { 0 },
            x2: vec![0; n],
            x3: vec![0; n],
        }
    }
    /// n=0 ticks the state with no I/O (the memcpy LB column).
    fn step(&mut self, x0: Option<&[i32]>, x1: &mut [i32], n: usize) {
        if n == 0 {
            self.st = match self.st {
                0 => 1,
                1 => 4,
                2 => 5,
                4 | 5 => 7,
                7 => {
                    if x0.is_none() {
                        9
                    } else {
                        8
                    }
                }
                8 => 7,
                9 => 11,
                s => s,
            };
            return;
        }
        match self.st {
            0 => {
                self.x2[..n].copy_from_slice(&x0.unwrap()[..n]);
                self.st = 1;
            }
            1 => {
                for i in 0..n {
                    self.x2[i] = (x0.unwrap()[i] << 2) - self.x2[i];
                }
                self.st = 4;
            }
            2 => {
                for i in 0..n {
                    self.x2[i] = x0.unwrap()[i] << 2;
                }
                self.st = 5;
            }
            4 => {
                for i in 0..n {
                    let w = self.x2[i] - x0.unwrap()[i];
                    self.x3[i] = (w + 1) >> 2;
                    self.x2[i] = x0.unwrap()[i];
                }
                self.st = 7;
            }
            5 => {
                for i in 0..n {
                    let w = self.x2[i] - 2 * x0.unwrap()[i];
                    self.x3[i] = (w + 1) >> 2;
                    self.x2[i] = x0.unwrap()[i];
                }
                self.st = 7;
            }
            7 => {
                if let Some(x0) = x0 {
                    for i in 0..n {
                        let t = self.x3[i];
                        x1[i] = t;
                        self.x3[i] = t + 2 * self.x2[i];
                        self.x2[i] = (x0[i] << 2) - self.x2[i];
                    }
                    self.st = 8;
                } else {
                    x1[..n].copy_from_slice(&self.x3[..n]);
                    self.st = 9;
                }
            }
            8 => {
                let x0 = x0.unwrap();
                for i in 0..n {
                    let w = (self.x2[i] - x0[i] + 1) >> 2;
                    x1[i] = (w + self.x3[i]) >> 1;
                    self.x2[i] = x0[i];
                    self.x3[i] = w;
                }
                self.st = 7;
            }
            9 => {
                for i in 0..n {
                    x1[i] = self.x3[i] + self.x2[i];
                }
                self.st = 11;
            }
            _ => {}
        }
    }
}

// ---------- tile orchestration ----------

#[allow(clippy::too_many_arguments)]
fn decode_tile(
    precs: &[&[u8]],
    img_w: usize,
    lay: &Layout,
    ph: &PicHeader,
    tiles: &mut [i32],
    tile_idx: usize,
    stripe_ints: usize,
    overflow: &mut [i32],
    first: bool,
) -> Result<()> {
    let pass_stride = 3 * lay.lift_lb + lay.memcpy_lb; // quarter-stripe ints
    let lift_st = lay.lift_lb * 4;
    let memcpy_st = lay.memcpy_lb * 4;
    let kband = lift_st;

    let mut pred = Pred::new(lay);
    let mut ver: Vec<VlState> = (0..4).map(|_| VlState::new(lift_st, first)).collect();
    let buf_len = 4 * pass_stride;
    let (mut buf_a, mut buf_b) = (vec![0i32; buf_len], vec![0i32; buf_len]);
    let mut h_out = vec![0i32; 4 * lift_st];
    let mut h_work = vec![0i32; 4 * lift_st + 64];
    let mut tile_buf = vec![0i32; 40 * stripe_ints];
    if !first {
        tile_buf[..2 * stripe_ints].copy_from_slice(overflow);
    }
    let mut x1_off = if first { 0usize } else { 8 }; // quarter-stripe units
    let mut memcpy_cur = 0usize; // ints; advances stripe_ints per pass

    for p in 0..precs.len() {
        buf_a.fill(0);
        buf_b.fill(0);
        decode_precinct(precs[p], img_w, lay, ph, &mut pred, &mut buf_a, &mut buf_b)?;

        // pass A: horizontal IDWT → memcpy LL → vertical lift (skipped for
        // precinct 0 of non-first tiles — shared precinct already produced it)
        h_out.fill(0);
        h_work.fill(0);
        idwt_pass(&buf_a, lay, true, &mut h_out, &mut h_work);
        tile_buf[memcpy_cur + 3 * kband..memcpy_cur + 3 * kband + memcpy_st]
            .copy_from_slice(&h_out[2 * lift_st..2 * lift_st + memcpy_st]);
        memcpy_cur += stripe_ints;
        if !(p == 0 && !first) {
            let pre = ver[0].st;
            let base = x1_off * pass_stride;
            {
                ver[0].step(
                    Some(&h_out[0..lift_st]),
                    &mut tile_buf[base..base + lift_st],
                    lift_st,
                );
                ver[1].step(
                    Some(&h_out[lift_st..2 * lift_st]),
                    &mut tile_buf[base + lift_st..base + 2 * lift_st],
                    lift_st,
                );
                ver[2].step(
                    None,
                    &mut tile_buf[base + 2 * lift_st..base + 2 * lift_st],
                    0,
                );
                ver[3].step(
                    Some(&h_out[2 * lift_st + memcpy_st..2 * lift_st + memcpy_st + lift_st]),
                    &mut tile_buf[base + 2 * lift_st..base + 3 * lift_st],
                    lift_st,
                );
            }
            if pre > 5 {
                x1_off += 4;
            }
        }

        // pass B
        h_out.fill(0);
        h_work.fill(0);
        idwt_pass(&buf_b, lay, false, &mut h_out, &mut h_work);
        tile_buf[memcpy_cur + 3 * kband..memcpy_cur + 3 * kband + memcpy_st]
            .copy_from_slice(&h_out[2 * lift_st..2 * lift_st + memcpy_st]);
        memcpy_cur += stripe_ints;
        {
            let pre = ver[0].st;
            let base = x1_off * pass_stride;
            ver[0].step(
                Some(&h_out[0..lift_st]),
                &mut tile_buf[base..base + lift_st],
                lift_st,
            );
            ver[1].step(
                Some(&h_out[lift_st..2 * lift_st]),
                &mut tile_buf[base + lift_st..base + 2 * lift_st],
                lift_st,
            );
            ver[2].step(
                None,
                &mut tile_buf[base + 2 * lift_st..base + 2 * lift_st],
                0,
            );
            ver[3].step(
                Some(&h_out[2 * lift_st + memcpy_st..2 * lift_st + memcpy_st + lift_st]),
                &mut tile_buf[base + 2 * lift_st..base + 3 * lift_st],
                lift_st,
            );
            if pre > 5 {
                x1_off += 4;
            }
        }
    }
    // tail flush: two x0=None ticks drain states 7→9→11
    for _ in 0..2 {
        let pre = ver[0].st;
        let base = x1_off * pass_stride;
        ver[0].step(None, &mut tile_buf[base..base + lift_st], lift_st);
        ver[1].step(
            None,
            &mut tile_buf[base + lift_st..base + 2 * lift_st],
            lift_st,
        );
        ver[2].step(
            None,
            &mut tile_buf[base + 2 * lift_st..base + 2 * lift_st],
            0,
        );
        ver[3].step(
            None,
            &mut tile_buf[base + 2 * lift_st..base + 3 * lift_st],
            lift_st,
        );
        if pre > 5 {
            x1_off += 4;
        }
    }

    let dst = &mut tiles[tile_idx * STRIPES_PER_TILE * stripe_ints
        ..(tile_idx + 1) * STRIPES_PER_TILE * stripe_ints];
    dst.copy_from_slice(&tile_buf[..STRIPES_PER_TILE * stripe_ints]);
    overflow.copy_from_slice(
        &tile_buf[STRIPES_PER_TILE * stripe_ints..(STRIPES_PER_TILE + 2) * stripe_ints],
    );
    Ok(())
}

// ---------- Bayer merge ----------

/// Merge the 4 stripe planes (p1=LL, p2=LH, p3=HH, p4=HL) into L and H via
/// the 2D inverse lift. All four planes index `tiles` at
/// `band_base + r*stride`; at tile edges the previous/next row belongs to
/// the neighboring tile's stripes (the flat scratch makes that address
/// arithmetic work — clamped only at the image borders).
#[allow(clippy::too_many_arguments)]
fn step1(
    tiles: &[i32],
    p1_base: isize,
    p2_base: isize,
    p3_base: isize,
    p4_base: isize,
    rows: usize,
    cols: usize,
    stride: usize,
    scratch: &mut [i32],
    l_off: usize,
    h_off: usize,
    first: bool,
    last: bool,
) {
    if rows == 0 || cols == 0 {
        return;
    }
    let st = stride as isize;
    let at = |b: isize, r: isize, c: usize| -> i32 { tiles[(b + r * st + c as isize) as usize] };
    for r in 0..rows as isize {
        let prev_r = if r == 0 && first { 0 } else { r - 1 };
        let next_r = if r == rows as isize - 1 && last {
            r
        } else {
            r + 1
        };
        let i = r * st;
        // c = 0 boundary
        {
            let c = 0usize;
            let cp = if cols == 1 { 0 } else { 1 };
            let hh_sum = at(p3_base, r, c) + at(p3_base, r, cp);
            let lh_p = at(p2_base, r, c)
                - ((hh_sum + at(p3_base, prev_r, c) + at(p3_base, prev_r, cp)) >> 3);
            let lh_np = at(p2_base, next_r, c)
                - ((hh_sum + at(p3_base, next_r, c) + at(p3_base, next_r, cp)) >> 3);
            let ll_hl = at(p1_base, r, c) + at(p4_base, r, c);
            scratch[l_off + (i + c as isize) as usize] =
                lh_p - ((ll_hl + at(p1_base, r, cp) + at(p4_base, prev_r, c)) >> 3);
            let pred2 = (lh_p + lh_np) << 1;
            let hh_p =
                at(p3_base, r, c) - ((ll_hl + at(p4_base, r, c) + at(p1_base, next_r, c)) >> 3);
            scratch[h_off + (i + c as isize) as usize] = hh_p + (pred2 >> 2);
        }
        let (mut carry_pred, mut carry_next) = {
            let c = 0usize;
            let cp = if cols == 1 { 0 } else { 1 };
            let hh_sum = at(p3_base, r, c) + at(p3_base, r, cp);
            (
                at(p2_base, r, c)
                    - ((hh_sum + at(p3_base, prev_r, c) + at(p3_base, prev_r, cp)) >> 3),
                at(p2_base, next_r, c)
                    - ((hh_sum + at(p3_base, next_r, c) + at(p3_base, next_r, cp)) >> 3),
            )
        };
        for c in 1..cols {
            let cp = if c == cols - 1 { c } else { c + 1 };
            let hh_sum = at(p3_base, r, c) + at(p3_base, r, cp);
            let cur_p = at(p2_base, r, c)
                - ((hh_sum + at(p3_base, prev_r, cp) + at(p3_base, prev_r, c)) >> 3);
            let nxt_p = at(p2_base, next_r, c)
                - ((hh_sum + at(p3_base, next_r, c) + at(p3_base, next_r, cp)) >> 3);
            let ll_hl = at(p1_base, r, c) + at(p4_base, r, c);
            scratch[l_off + (i + c as isize) as usize] =
                cur_p - ((ll_hl + at(p1_base, r, cp) + at(p4_base, prev_r, c)) >> 3);
            let psum = carry_pred + cur_p + nxt_p + carry_next;
            let hh_p =
                at(p3_base, r, c) - ((ll_hl + at(p4_base, r, c - 1) + at(p1_base, next_r, c)) >> 3);
            scratch[h_off + (i + c as isize) as usize] = hh_p + (psum >> 2);
            carry_pred = cur_p;
            carry_next = nxt_p;
        }
        let _ = (carry_pred, carry_next);
    }
}

/// Final merge per stripe → 2 Bayer rows through the tone LUT (midpoint
/// bias, 14-bit clamp). p1/p4 index `tiles`, p2/p3 index `step1_out` —
/// each at `base + r*stride` with neighbor-tile rows at the edges.
#[allow(clippy::too_many_arguments)]
fn step2(
    tiles: &[i32],
    step1_out: &[i32],
    p1_base: isize,
    p2_base: isize,
    p3_base: isize,
    p4_base: isize,
    rows: usize,
    cols: usize,
    stride: usize,
    lut: &[i32],
    out: &mut [u16],
    img_w: usize,
    row_start: usize,
    first: bool,
    last: bool,
) {
    let mid = 1i32 << (LUT_SHIFT + 14 - 1);
    let look = |v: i32| -> u16 {
        let idx = v.clamp(0, LUT_SIZE as i32 - 1) as usize;
        ((lut[idx] + (1 << (LUT_SHIFT - 1))) >> LUT_SHIFT).clamp(0, CLIP_MAX) as u16
    };
    let st = stride as isize;
    let at13 = |b: isize, r: isize, c: usize| -> i32 { tiles[(b + r * st + c as isize) as usize] };
    let at24 =
        |b: isize, r: isize, c: usize| -> i32 { step1_out[(b + r * st + c as isize) as usize] };
    for r in 0..rows as isize {
        let prev_r = if r == 0 && first { 0 } else { r - 1 };
        let next_r = if r == rows as isize - 1 && last {
            r
        } else {
            r + 1
        };
        let (bt, bb) = (row_start + 2 * r as usize, row_start + 2 * r as usize + 1);
        if bb * img_w + 2 * cols > out.len() {
            continue;
        }
        // c = 0 boundary
        {
            let c = 0usize;
            let cp = if cols == 1 { 0 } else { 1 };
            let hh_c = at24(p3_base, r, c);
            let hh_cp = at24(p3_base, r, cp);
            let lh_c = at24(p2_base, r, c);
            let lh_next = at24(p2_base, next_r, c);
            let hl_c = at13(p4_base, r, c);
            let s0 =
                ((at24(p3_base, prev_r, c) + lh_c + hh_c + lh_c) >> 2) + mid + at13(p1_base, r, c);
            out[bt * img_w + 2 * c] = look(s0);
            out[bt * img_w + 2 * c + 1] = look(lh_c + mid);
            out[bb * img_w + 2 * c] = look(hh_c + mid);
            out[bb * img_w + 2 * c + 1] = look(hl_c + mid + ((hh_cp + hh_c + lh_c + lh_next) >> 2));
        }
        for c in 1..cols {
            let cp = if c == cols - 1 { c } else { c + 1 };
            let hh_c = at24(p3_base, r, c);
            let hh_cp = at24(p3_base, r, cp);
            let hh_plus_lh = hh_c + at24(p2_base, r, c);
            let lh_c = at24(p2_base, r, c);
            let lh_next = at24(p2_base, next_r, c);
            let hl_c = at13(p4_base, r, c);
            let s0 = ((at24(p2_base, r, c - 1) + at24(p3_base, prev_r, c) + hh_plus_lh) >> 2)
                + mid
                + at13(p1_base, r, c);
            out[bt * img_w + 2 * c] = look(s0);
            out[bt * img_w + 2 * c + 1] = look(lh_c + mid);
            out[bb * img_w + 2 * c] = look(hh_c + mid);
            out[bb * img_w + 2 * c + 1] = look(hl_c + mid + ((hh_cp + hh_plus_lh + lh_next) >> 2));
        }
    }
}

// ---------- tone LUT (256 PWL breakpoints → 81792-entry table) ----------

const IQX_IQP_BP: [[i32; 2]; 256] = [
    [0, 0],
    [349, 172],
    [845, 409],
    [1301, 620],
    [1672, 787],
    [1983, 924],
    [2369, 1090],
    [2834, 1284],
    [3334, 1485],
    [3750, 1646],
    [4206, 1817],
    [4660, 1981],
    [5157, 2153],
    [5601, 2300],
    [5976, 2420],
    [6396, 2549],
    [6837, 2679],
    [7287, 2805],
    [7703, 2916],
    [8023, 2998],
    [8459, 3105],
    [8936, 3215],
    [9447, 3325],
    [9845, 3405],
    [10266, 3485],
    [10613, 3546],
    [10934, 3600],
    [11349, 3665],
    [11769, 3725],
    [12151, 3775],
    [12562, 3824],
    [12938, 3864],
    [13259, 3895],
    [13632, 3927],
    [14064, 3959],
    [14498, 3985],
    [14901, 4004],
    [15293, 4018],
    [15643, 4026],
    [15980, 4031],
    [16626, 4035],
    [17132, 4051],
    [17667, 4082],
    [18186, 4126],
    [18709, 4184],
    [19245, 4258],
    [19756, 4342],
    [20300, 4446],
    [20840, 4564],
    [21346, 4688],
    [21884, 4834],
    [22420, 4994],
    [22951, 5167],
    [23486, 5356],
    [23993, 5548],
    [24557, 5777],
    [25087, 6007],
    [25584, 6236],
    [26118, 6495],
    [26628, 6757],
    [27101, 7011],
    [27559, 7268],
    [28035, 7546],
    [28545, 7857],
    [29005, 8149],
    [29479, 8460],
    [30010, 8823],
    [30507, 9176],
    [30964, 9511],
    [31418, 9854],
    [31931, 10255],
    [32442, 10667],
    [32447, 10672],
    [32894, 11043],
    [33406, 11481],
    [33898, 11915],
    [34402, 12371],
    [34417, 12386],
    [34831, 12771],
    [35334, 13250],
    [35370, 13286],
    [35753, 13660],
    [36247, 14153],
    [36739, 14658],
    [37177, 15117],
    [37700, 15678],
    [38193, 16220],
    [38734, 16828],
    [38752, 16850],
    [39116, 17267],
    [39446, 17653],
    [39716, 17971],
    [40201, 18553],
    [40697, 19161],
    [41176, 19760],
    [41658, 20374],
    [42156, 21021],
    [42538, 21526],
    [43000, 22146],
    [43006, 22155],
    [43155, 22357],
    [43611, 22983],
    [44207, 23818],
    [44720, 24551],
    [44730, 24566],
    [45133, 25151],
    [45539, 25749],
    [45553, 25770],
    [46028, 26480],
    [46347, 26964],
    [46670, 27459],
    [46943, 27882],
    [47308, 28452],
    [47577, 28878],
    [47866, 29338],
    [48105, 29722],
    [48432, 30252],
    [48605, 30535],
    [48935, 31078],
    [49031, 31238],
    [49306, 31696],
    [49714, 32383],
    [50159, 33142],
    [50660, 34008],
    [50665, 34018],
    [50979, 34567],
    [51460, 35419],
    [51480, 35455],
    [51803, 36034],
    [51966, 36328],
    [52285, 36908],
    [52435, 37182],
    [52816, 37884],
    [53272, 38733],
    [53277, 38743],
    [53385, 38946],
    [53735, 39607],
    [54160, 40418],
    [54165, 40428],
    [54519, 41111],
    [54544, 41159],
    [54863, 41780],
    [54889, 41832],
    [55103, 42251],
    [55132, 42309],
    [55236, 42514],
    [55495, 43026],
    [55601, 43238],
    [55932, 43899],
    [55947, 43931],
    [56023, 44082],
    [56030, 44098],
    [56046, 44129],
    [56125, 44289],
    [56230, 44501],
    [56549, 45149],
    [56568, 45189],
    [56581, 45214],
    [56585, 45224],
    [56612, 45277],
    [56613, 45281],
    [56626, 45306],
    [56634, 45324],
    [56647, 45349],
    [56658, 45373],
    [56732, 45524],
    [56856, 45779],
    [56887, 45843],
    [56897, 45862],
    [56908, 45886],
    [57027, 46131],
    [57243, 46578],
    [57363, 46828],
    [57426, 46959],
    [57728, 47591],
    [57789, 47720],
    [57795, 47731],
    [57808, 47759],
    [57947, 48052],
    [58151, 48484],
    [58520, 49270],
    [58542, 49318],
    [58769, 49805],
    [59050, 50412],
    [59408, 51191],
    [59451, 51286],
    [59477, 51342],
    [59531, 51461],
    [59883, 52235],
    [60267, 53087],
    [60269, 53093],
    [60295, 53150],
    [60636, 53913],
    [60642, 53927],
    [60815, 54317],
    [61153, 55082],
    [61664, 56251],
    [61839, 56654],
    [61862, 56708],
    [61895, 56783],
    [61896, 56787],
    [61933, 56871],
    [61934, 56875],
    [61975, 56968],
    [61976, 56972],
    [62013, 57056],
    [62014, 57060],
    [62044, 57128],
    [62045, 57132],
    [62079, 57209],
    [62080, 57213],
    [62397, 57950],
    [62400, 57958],
    [62436, 58041],
    [62437, 58045],
    [62474, 58130],
    [62475, 58134],
    [62513, 58221],
    [62514, 58225],
    [62530, 58261],
    [62531, 58265],
    [62578, 58374],
    [62587, 58396],
    [62813, 58927],
    [62815, 58933],
    [62832, 58971],
    [62833, 58975],
    [62870, 59061],
    [62875, 59074],
    [63029, 59437],
    [63031, 59443],
    [63064, 59520],
    [63065, 59524],
    [63078, 59553],
    [63080, 59559],
    [63302, 60086],
    [63597, 60791],
    [63603, 60806],
    [63767, 61200],
    [63804, 61289],
    [64102, 62009],
    [64118, 62047],
    [64124, 62062],
    [64193, 62230],
    [64224, 62305],
    [64429, 62804],
    [64707, 63484],
    [65110, 64477],
    [65130, 64527],
    [65400, 65197],
    [65456, 65337],
    [65493, 65429],
    [65518, 65491],
    [65530, 65521],
    [65535, 65534],
    [81791, 65534],
];

fn build_lut() -> Vec<i32> {
    let mut t = vec![0i32; LUT_SIZE];
    let mut k = 0usize;
    for (i, e) in t.iter_mut().enumerate() {
        while k < 255 && (IQX_IQP_BP[k + 1][0] as usize) <= i {
            k += 1;
        }
        let (xa, ya) = (IQX_IQP_BP[k][0], IQX_IQP_BP[k][1]);
        let (xb, yb) = (
            IQX_IQP_BP[(k + 1).min(255)][0],
            IQX_IQP_BP[(k + 1).min(255)][1],
        );
        *e = if xb == xa {
            ya
        } else {
            ya + (yb - ya) * (i as i32 - xa) / (xb - xa)
        };
    }
    t
}

// ---------- top level ----------

/// Decode a Nikon HE/HE* NEF to the raw Bayer mosaic (u16, 14-bit).
/// Returns (bayer, width, height); errors on non-HE files or unsupported
/// profiles, letting callers fall through to other paths.
pub fn decode_nef_he(path: &Path) -> Result<(Vec<u16>, usize, usize)> {
    let d = std::fs::read(path)?;
    let (off, size) = find_jxs_strip(&d).ok_or_else(|| anyhow::anyhow!("no JXS strip"))?;
    let strip = &d[off..off + size];
    let ph = parse_pic_header(strip)?;
    let (img_w, img_h) = (ph.w, ph.h);

    // file precincts: [24-bit payload size][12-byte header+payload],
    // 6-byte alignment pad after every 16th (index ≡ 15 mod 16)
    let mut precs: Vec<&[u8]> = Vec::new();
    let mut cur = ph.precinct_off;
    loop {
        if cur + 3 > strip.len() {
            break;
        }
        let sz = be24(strip, cur);
        if sz == 0 || cur + sz + 12 > strip.len() {
            break;
        }
        precs.push(&strip[cur..cur + sz + 12]);
        cur += sz + 12;
        if precs.len() % 16 == 0 && cur + 6 <= strip.len() {
            cur += 6;
        }
    }
    let n_tiles = img_h.div_ceil(64);
    // last tile carries only the precincts its remaining rows need
    // (e.g. 5520 → 86 full tiles + a 4-precinct tail)
    if precs.len() <= (n_tiles - 1) * 16 {
        bail!(
            "precinct stream short: {} for {} tiles",
            precs.len(),
            n_tiles
        );
    }

    let lay = compute_layout(img_w / 2);
    let pass_stride = 3 * lay.lift_lb + lay.memcpy_lb;
    let stripe_ints = 4 * pass_stride;
    let kband = lay.lift_lb * 4;

    let mut tiles = vec![0i32; n_tiles * STRIPES_PER_TILE * stripe_ints];
    let mut step1_buf = vec![0i32; n_tiles * STRIPES_PER_TILE * stripe_ints];
    let mut overflow = vec![0i32; 2 * stripe_ints];

    // pass 1: entropy + horizontal IDWT + vertical lift per tile
    for t in 0..n_tiles {
        let base = t * 16;
        let n = PRECINCTS_PER_TILE.min(precs.len() - base);
        decode_tile(
            &precs[base..base + n],
            img_w,
            &lay,
            &ph,
            &mut tiles,
            t,
            stripe_ints,
            &mut overflow,
            t == 0,
        )?;
    }

    // pass 2 (step1 over ALL tiles) must finish before any step2:
    // step2 reads neighboring tiles' step1 rows at tile boundaries.
    let lut = build_lut();
    let mut bayer = vec![0u16; img_w * img_h];
    for t in 0..n_tiles {
        let last = t == n_tiles - 1;
        let rows = if last && img_h % 64 != 0 {
            (img_h % 64) / 2
        } else {
            STRIPES_PER_TILE
        };
        let cols = img_w / 2;
        let tb = (t * STRIPES_PER_TILE * stripe_ints) as isize;
        step1(
            &tiles,
            tb + kband as isize,
            tb,
            tb + 3 * kband as isize,
            tb + 2 * kband as isize,
            rows,
            cols,
            stripe_ints,
            &mut step1_buf,
            (t * STRIPES_PER_TILE * stripe_ints) as usize,
            (t * STRIPES_PER_TILE * stripe_ints + kband) as usize,
            t == 0,
            last,
        );
    }
    for t in 0..n_tiles {
        let last = t == n_tiles - 1;
        let rows = if last && img_h % 64 != 0 {
            (img_h % 64) / 2
        } else {
            STRIPES_PER_TILE
        };
        let cols = img_w / 2;
        let tb = (t * STRIPES_PER_TILE * stripe_ints) as isize;
        step2(
            &tiles,
            &step1_buf,
            tb + kband as isize,
            tb,
            tb + kband as isize,
            tb + 2 * kband as isize,
            rows,
            cols,
            stripe_ints,
            &lut,
            &mut bayer,
            img_w,
            t * 64,
            t == 0,
            last,
        );
    }
    Ok((bayer, img_w, img_h))
}

/// Cheap probe: does this file contain a supported HE/HE* codestream?
pub fn is_nef_he(path: &Path) -> bool {
    let Ok(d) = std::fs::read(path) else {
        return false;
    };
    let Some((off, size)) = find_jxs_strip(&d) else {
        return false;
    };
    parse_pic_header(&d[off..off + size]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_z9() {
        let f = std::env::var("HE_FILE")
            .unwrap_or_else(|_| "/Users/devin/real-raws/net/nikon_z9_he_star.nef".into());
        let p = Path::new(&f);
        assert!(is_nef_he(p), "probe failed");
        let (b, w, h) = decode_nef_he(p).unwrap();
        eprintln!("decoded {w}x{h}");
        let mut mn = u16::MAX;
        let mut mx = 0u16;
        let mut sum = 0u64;
        for &v in &b {
            mn = mn.min(v);
            mx = mx.max(v);
            sum += v as u64;
        }
        eprintln!("min={mn} max={mx} mean={}", sum / b.len() as u64);
        // zone check: every 256x256 block must have nonzero signal
        let mut empty = 0;
        for by in (0..h - 256).step_by(256) {
            for bx in (0..w - 256).step_by(256) {
                let mut s = 0u64;
                for y in by..by + 256 {
                    for x in bx..bx + 256 {
                        s += b[y * w + x] as u64;
                    }
                }
                if s == 0 {
                    empty += 1;
                }
            }
        }
        eprintln!("empty 256x256 zones: {empty}");
        assert!(mx > 1000, "no signal");
        assert_eq!(empty, 0, "dead zones present");
    }
}

// quick check: does decode_nef_he succeed on these files directly?
#[test]
fn check_he_files() {
    for f in [
        "/Users/devin/real-raws/net/nikon_z9_he.nef",
        "/Users/devin/real-raws/net/nikon_z8_he_high.nef",
        "/Users/devin/real-raws/net/nikon_z9_he_star.nef",
    ] {
        if !std::path::Path::new(f).exists() {
            continue;
        }
        let r = decode_nef_he(std::path::Path::new(f));
        match &r {
            Ok((b, w, h)) => {
                let mn = b.iter().min().copied().unwrap_or(0);
                let mx = b.iter().max().copied().unwrap_or(0);
                let mean: u64 = b.iter().map(|&v| v as u64).sum::<u64>() / b.len() as u64;
                eprintln!("{f}: OK {w}x{h} min={mn} max={mx} mean={mean}");
            }
            Err(e) => eprintln!("{f}: ERR {e}"),
        }
    }
}

#[cfg(test)]
mod dump {
    use super::*;

    #[test]
    fn dump_mosaic() {
        let f = std::env::var("HE_FILE")
            .unwrap_or_else(|_| "/Users/devin/real-raws/net/nikon_z9_he_star.nef".into());
        let (b, w, h) = decode_nef_he(Path::new(&f)).unwrap();
        let mut out = String::new();
        out.push_str(&format!("P5\n{w} {h}\n65535\n"));
        let mut raw = Vec::with_capacity(w * h * 2);
        for &v in &b {
            raw.extend_from_slice(&(v << 2).to_be_bytes());
        }
        std::fs::write("/tmp/he_mosaic.pgm", out.as_bytes()).unwrap();
        std::fs::write(
            "/tmp/he_mosaic.pgm",
            [out.as_bytes().to_vec(), raw].concat(),
        )
        .unwrap();
        // row profile stats per 64-row band to spot tile-boundary breaks
        for t in 0..4 {
            let y0 = t * 64;
            let mut s = 0u64;
            for y in y0..y0 + 64 {
                for x in 0..w {
                    s += b[y * w + x] as u64;
                }
            }
            eprintln!("tile {t} mean {}", s / (64 * w) as u64);
        }
        // per-band column profile: mean per 8px column in rows 60..70
        for y in [62usize, 63, 64, 65] {
            let mut s = 0u64;
            for x in 0..w {
                s += b[y * w + x] as u64;
            }
            eprintln!("row {y} mean {}", s / w as u64);
        }
    }
}
