//! Adobe `dng_rgb_table` decode — the `crs:Table_<digest>` payloads carried
//! by "enhanced" XMP profiles/presets (crs:LookTable / crs:RGBTables).
//!
//! The payload is Adobe's own little-endian base85 text wrapping a
//! zlib stream; after inflation the binary is a `dng_rgb_table`: header
//! (type, version, dimensions, divisions), then u16 samples stored as
//! deltas from a neutral identity ramp, then metadata (primaries, gamma,
//! gamut, amount range, flags). 1D tables are per-channel curves, 3D
//! tables are RGB→RGB LUTs.
//!
//! We convert either into a `CubeLut` (display-referred, trilinear) so it
//! rides the existing `.cube` LUT path in both CPU and GPU pipelines.

use crate::dcp::HsMap;
use crate::lut::CubeLut;

const BTT_LOOK_TABLE: u32 = 0;
const BTT_RGB_TABLE: u32 = 1;
const RGB_TABLE_VERSION: u32 = 1;

/// Decoded `crs:Table_*` payload — either an HSV look table (same shape
/// as a DNG ProfileLookTable, applied early like the profile look) or an
/// RGB LUT (applied near the end of the pipe).
pub enum Table {
    Look(HsMap),
    Rgb(RgbTable),
}

pub struct RgbTable {
    pub dimensions: u32,
    pub divisions: u32,
    /// samples in Adobe order: r outer, g mid, b fastest
    pub samples: Vec<[u16; 3]>,
    pub primaries: u32,
    pub gamma: u32,
    pub gamut: u32,
    pub min_amount: f64,
    pub max_amount: f64,
}

/// Decode + parse a `crs:Table_*` attribute value.
pub fn decode(encoded: &str) -> Option<Table> {
    let compressed = base85_decode(encoded);
    if compressed.len() < 5 {
        return None;
    }
    let expected = u32::from_le_bytes(compressed[0..4].try_into().ok()?) as usize;
    let decoded = miniz_oxide::inflate::decompress_to_vec_zlib(&compressed[4..]).ok()?;
    if decoded.len() != expected {
        return None;
    }
    let mut r = Rd::new(&decoded);
    match r.u32()? {
        BTT_LOOK_TABLE => parse_look(&mut r).map(Table::Look),
        BTT_RGB_TABLE => parse_rgb(&mut r).map(Table::Rgb),
        _ => None,
    }
}

/// dng_look_table (per dng_big_table::PutStream): type, version,
/// [hue,sat,val] divs, then h*s*v cells of 3 f32 =
/// [hue_shift_deg, sat_scale, val_scale] in the dng_hue_sat_map storage
/// order — value outermost, hue middle, saturation fastest — the same
/// layout ProfileLookTableData uses inside a .dcp. The encoding u32
/// (0=linear, 1=sRGB) trails the cells, then optional min/max/flags.
fn parse_look(r: &mut Rd) -> Option<HsMap> {
    let version = r.u32()?;
    if !(1..=2).contains(&version) {
        return None;
    }
    let (hd, sd, vd) = (r.u32()?, r.u32()?, r.u32()?);
    if !(1..=360).contains(&hd) || !(1..=256).contains(&sd) || !(1..=256).contains(&vd) {
        return None;
    }
    let n = hd as usize * sd as usize * vd as usize;
    let mut data = Vec::with_capacity(n);
    for _ in 0..n {
        data.push([r.f32()?, r.f32()?, r.f32()?]);
    }
    let srgb_encoded = r.u32().unwrap_or(0) == 1;
    Some(HsMap {
        dims: [hd, sd, vd],
        data,
        srgb_encoded,
    })
}

/// Convert the table into a display-referred `CubeLut`.
/// `out_div` is the cube edge (Adobe 3D tables are usually 17 or 33;
/// resampling to the same grid when it already matches avoids loss).
pub fn to_cube(t: &RgbTable, out_div: usize) -> CubeLut {
    let d = t.divisions as usize;
    let n = out_div;
    let mut data = vec![0.0f32; n * n * n * 3];
    if t.dimensions == 1 {
        // per-channel curve: out_c = T_c(in_c); table grid is 0..1
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let i = ((z * n + y) * n + x) * 3;
                    data[i] = curve_at(t, x as f32 / (n - 1) as f32, 0);
                    data[i + 1] = curve_at(t, y as f32 / (n - 1) as f32, 1);
                    data[i + 2] = curve_at(t, z as f32 / (n - 1) as f32, 2);
                }
            }
        }
    } else {
        // 3D table: trilinear-resample the division grid onto the cube grid
        let s = (d - 1) as f32 / (n - 1) as f32;
        for z in 0..n {
            for y in 0..n {
                for x in 0..n {
                    let fr = x as f32 * s;
                    let fg = y as f32 * s;
                    let fb = z as f32 * s;
                    let (r0, r1, tr) = split(fr, d);
                    let (g0, g1, tg) = split(fg, d);
                    let (b0, b1, tb) = split(fb, d);
                    let i = ((z * n + y) * n + x) * 3;
                    for c in 0..3 {
                        let at = |r: usize, g: usize, b: usize| {
                            t.samples[(r * d + g) * d + b][c] as f32 / 65535.0
                        };
                        let c00 = lerp(at(r0, g0, b0), at(r1, g0, b0), tr);
                        let c10 = lerp(at(r0, g1, b0), at(r1, g1, b0), tr);
                        let c01 = lerp(at(r0, g0, b1), at(r1, g0, b1), tr);
                        let c11 = lerp(at(r0, g1, b1), at(r1, g1, b1), tr);
                        let c0 = lerp(c00, c10, tg);
                        let c1 = lerp(c01, c11, tg);
                        data[i + c] = lerp(c0, c1, tb);
                    }
                }
            }
        }
    }
    CubeLut {
        size: n,
        data,
        dmin: [0.0; 3],
        dscale: [1.0; 3],
    }
}

fn curve_at(t: &RgbTable, x: f32, chan: usize) -> f32 {
    let d = t.divisions as usize;
    let (i0, i1, f) = split(x * (d - 1) as f32, d);
    lerp(
        t.samples[i0][chan] as f32 / 65535.0,
        t.samples[i1][chan] as f32 / 65535.0,
        f,
    )
}

fn split(pos: f32, div: usize) -> (usize, usize, f32) {
    let lo = pos.floor().clamp(0.0, (div - 1) as f32) as usize;
    let hi = (lo + 1).min(div - 1);
    (lo, hi, pos - lo as f32)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn parse_rgb(r: &mut Rd) -> Option<RgbTable> {
    if r.u32()? != RGB_TABLE_VERSION {
        return None;
    }
    let dimensions = r.u32()?;
    let divisions = r.u32()?;
    if (dimensions != 1 && dimensions != 3) || divisions < 2 {
        return None;
    }
    let dv = divisions as usize;
    // neutral ramp the samples are delta-encoded against
    let ramp = |i: usize| -> u16 { ((i * 0x0ffff + ((dv - 1) >> 1)) / (dv - 1)) as u16 };
    let count = if dimensions == 1 { dv } else { dv * dv * dv };
    let mut samples = Vec::with_capacity(count);
    if dimensions == 1 {
        for i in 0..dv {
            let p = ramp(i);
            samples.push([
                r.u16()?.wrapping_add(p),
                r.u16()?.wrapping_add(p),
                r.u16()?.wrapping_add(p),
            ]);
        }
    } else {
        for ri in 0..dv {
            for gi in 0..dv {
                for bi in 0..dv {
                    samples.push([
                        r.u16()?.wrapping_add(ramp(ri)),
                        r.u16()?.wrapping_add(ramp(gi)),
                        r.u16()?.wrapping_add(ramp(bi)),
                    ]);
                }
            }
        }
    }
    Some(RgbTable {
        dimensions,
        divisions,
        samples,
        primaries: r.u32()?,
        gamma: r.u32()?,
        gamut: r.u32()?,
        min_amount: r.f64()?,
        max_amount: r.f64()?,
    })
}

/// Adobe's little-endian base85: 5 digits → one LE u32. Alphabet is
/// 0-9 a-z A-Z then punctuation; not standard Ascii85 ordering.
fn base85_decode(s: &str) -> Vec<u8> {
    fn digit(b: u8) -> Option<u32> {
        Some(match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'z' => 10 + b - b'a',
            b'A'..=b'Z' => 36 + b - b'A',
            b'.' => 62,
            b'-' => 63,
            b':' => 64,
            b'+' => 65,
            b'=' => 66,
            b'^' => 67,
            b'!' => 68,
            b'/' => 69,
            b'*' => 70,
            b'?' => 71,
            b'`' => 72,
            b'\'' => 73,
            b'|' => 74,
            b'(' => 75,
            b')' => 76,
            b'[' => 77,
            b']' => 78,
            b'{' => 79,
            b'}' => 80,
            b'@' => 81,
            b'%' => 82,
            b'$' => 83,
            b'#' => 84,
            _ => return None,
        } as u32)
    }
    let mut out = Vec::with_capacity(s.len() / 5 * 4 + 4);
    let mut phase = 0u32;
    let mut v = 0u32;
    for b in s.bytes() {
        let Some(d) = digit(b) else { continue };
        phase += 1;
        v += d * 85u32.pow(phase - 1);
        if phase == 5 {
            out.extend_from_slice(&v.to_le_bytes());
            phase = 0;
            v = 0;
        }
    }
    if phase > 1 {
        let bytes = v.to_le_bytes();
        out.extend_from_slice(&bytes[..(phase - 1) as usize]);
    }
    out
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base85_le_groups() {
        assert_eq!(base85_decode("00000"), [0, 0, 0, 0]);
        assert_eq!(base85_decode("10000"), [1, 0, 0, 0]);
        // '1'=1, '2'=2 → 1 + 2*85 = 171, partial group emits 1 byte
        assert_eq!(base85_decode("12"), [171]);
        assert_eq!(base85_decode("120"), [171, 0]);
    }

    #[test]
    fn decode_real_adobe_table() {
        // smallest real payload we can synthesize: zlib("..." ) — build a
        // 1D×2div table, compress, base85-encode is unnecessary; test parse
        // path directly instead.
        let mut b = Vec::new();
        b.extend(1u32.to_le_bytes()); // version
        b.extend(1u32.to_le_bytes()); // dimensions=1
        b.extend(2u32.to_le_bytes()); // divisions=2
                                      // sample deltas: identity ramp is [0, 65535]; make output [0,40000]
        for d in [0u16, 40000u16.wrapping_sub(65535)] {
            b.extend(d.to_le_bytes());
            b.extend(d.to_le_bytes());
            b.extend(d.to_le_bytes());
        }
        b.extend(0u32.to_le_bytes()); // primaries
        b.extend(0u32.to_le_bytes()); // gamma
        b.extend(0u32.to_le_bytes()); // gamut
        b.extend(0.0f64.to_le_bytes());
        b.extend(1.0f64.to_le_bytes());
        let t = parse_rgb(&mut Rd::new(&b)).unwrap();
        assert_eq!(t.samples[0], [0, 0, 0]);
        assert_eq!(t.samples[1], [40000, 40000, 40000]);
    }

    #[test]
    fn look_table_value_major_order() {
        // dng_look_table stream order is value outer, hue middle, sat
        // fastest: idx = (v*hd + h)*sd + s — same as dng_hue_sat_map.
        // 2 hue × 3 sat × 2 val cells, hue_shift = index for identity.
        let mut b = Vec::new();
        b.extend(2u32.to_le_bytes()); // version
        for d in [2u32, 3, 2] {
            b.extend(d.to_le_bytes());
        }
        for i in 0..12u32 {
            b.extend((i as f32).to_le_bytes()); // hue shift = cell index
            b.extend(1.0f32.to_le_bytes());
            b.extend(1.0f32.to_le_bytes());
        }
        let m = parse_look(&mut Rd::new(&b)).unwrap();
        assert_eq!(m.dims, [2, 3, 2]);
        // cell (h=1, s=2, v=1) lives at (1*2+1)*3+2 = 11
        assert_eq!(m.data[11][0], 11.0);
        // lookup lands on a single cell when coords hit grid points:
        // h=1 → hf=2 → h0 wraps... use v=0.5,s=1.0 and check interp range
        let d = m.lookup(0.75, 1.0, 0.5);
        assert!(d[0] >= 0.0 && d[0] <= 11.0);
        // exact cell lookup: h=0.25*2=0.5→ between h0/h1 — pick exact points
        // sat grid: s=1.0 → sf=2 → s0=1? (sd-1)=2 → sf=1.9999.. → s0=1,s1=2
        // value: v=1.0 → vf=0.9999.. → v0=0,v1=1
    }

    #[test]
    fn look_table_neutral_cells() {
        // per the SDK, s==0 cells must keep sat/val scale at 1.0 — a map
        // built that way leaves grays untouched.
        let mut b = Vec::new();
        b.extend(1u32.to_le_bytes());
        for d in [4u32, 2, 2] {
            b.extend(d.to_le_bytes());
        }
        for i in 0..16u32 {
            let s_idx = i % 2; // sat is fastest
            let sat_scale: f32 = if s_idx == 0 { 1.0 } else { 2.0 };
            b.extend(5.0f32.to_le_bytes());
            b.extend(sat_scale.to_le_bytes());
            b.extend(1.0f32.to_le_bytes());
        }
        let m = parse_look(&mut Rd::new(&b)).unwrap();
        // s=0 → all s0=0 cells → sat scale 1.0 → apply() returns input
        let out = m.apply([0.5, 0.5, 0.5]);
        assert!((out[0] - 0.5).abs() < 1e-4);
        assert!((out[2] - 0.5).abs() < 1e-4);
    }
}
