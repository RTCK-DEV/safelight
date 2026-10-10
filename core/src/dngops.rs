//! DNG embedded lens-correction opcodes (OpcodeList1/2/3).
//! Drone/phone DNGs (DJI, etc.) carry a GainMap (vignette/illumination)
//! plus WarpRectilinear — Lightroom applies them automatically; we read
//! and apply them the same way.
//!
//! Opcode stream layout (big-endian inside the tag regardless of TIFF order):
//!   u32 opcodeCount; per opcode: u32 id, u32 dngVersion, u32 flags,
//!   u32 paramBytes, params.
//!   id 1 = WarpRectilinear: u32 planes; f64 kr[planes][6]; f64 cx, cy
//!   id 9 = GainMap: u32 top,left,bottom,right,planes,rowG,colG,
//!          mapPtsV,mapPtsH; f64 mapSpacingV, mapSpacingH; u32 rowPad;
//!          f32 mapG[planes][mapPtsV][mapPtsH]
//!   id 19 = TrimBounds (skipped)

use std::path::Path;

#[derive(Clone, Debug)]
pub struct GainMap {
    /// destination plane range this map applies to
    pub plane0: usize,
    pub nplanes: usize,
    /// map grid dimensions (v, h)
    pub rows: usize,
    pub cols: usize,
    /// grid point spacing, normalized image coords (v, h)
    pub spacing: [f64; 2],
    /// first grid point position, normalized image coords (v, h)
    pub origin: [f64; 2],
    /// stored plane count (a map may store 1 plane for a 3-plane op)
    pub planes: usize,
    /// gain factors, plane-major then row-major
    pub map: Vec<f32>,
}

#[derive(Clone, Debug)]
pub struct WarpRect {
    /// per-channel radial coefficients k0..k5 (f = Σk_i r^2i)
    pub kr: [[f32; 6]; 3],
    /// distortion centre, image fractions (0..1)
    pub center: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct DngOps {
    pub warp: Option<WarpRect>,
    pub gain: Option<GainMap>,
}

impl GainMap {
    /// interpolated gain at image pixel (x, y) in an image of (img_w, img_h)
    /// for channel `ch` — dng_gain_map_interpolator semantics: map coords are
    /// normalized over the image bounds, indexed at pixel centres.
    pub fn at(&self, x: f64, y: f64, ch: usize, img_w: usize, img_h: usize) -> f64 {
        let (rows, cols) = (self.rows, self.cols);
        if rows == 0 || cols == 0 || self.map.is_empty() {
            return 1.0;
        }
        if ch < self.plane0 || ch >= self.plane0 + self.nplanes {
            return 1.0;
        }
        let v = (y + 0.5) / img_h.max(1) as f64;
        let u = (x + 0.5) / img_w.max(1) as f64;
        let gy = ((v - self.origin[0]) / self.spacing[0].max(1e-9))
            .clamp(0.0, (rows - 1) as f64);
        let gx = ((u - self.origin[1]) / self.spacing[1].max(1e-9))
            .clamp(0.0, (cols - 1) as f64);
        let (y0, x0) = (gy.floor() as usize, gx.floor() as usize);
        let (y1, x1) = ((y0 + 1).min(rows - 1), (x0 + 1).min(cols - 1));
        let (ty, tx) = ((gy - y0 as f64) as f32, (gx - x0 as f64) as f32);
        // interleaved per-pixel planes: map[(r*cols + c)*planes + p]
        let plane = ch.min(self.planes.max(1) - 1);
        let g = |r: usize, c: usize| self.map[(r * cols + c) * self.planes + plane];
        let a = g(y0, x0) + tx * (g(y0, x1) - g(y0, x0));
        let b = g(y1, x0) + tx * (g(y1, x1) - g(y1, x0));
        (a + ty * (b - a)) as f64
    }
}

// ---- minimal TIFF walk for opcode tags ------------------------------------

struct Rd<'a> {
    d: &'a [u8],
    le: bool,
}
impl<'a> Rd<'a> {
    fn u16(&self, o: usize) -> u16 {
        let b = &self.d[o..o + 2];
        if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    }
    fn u32(&self, o: usize) -> u32 {
        let b = &self.d[o..o + 4];
        if self.le {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        }
    }
    fn tag(&self, ifd: usize, want: u16) -> Option<(u16, u32, usize)> {
        let n = self.u16(ifd) as usize;
        for i in 0..n {
            let o = ifd + 2 + i * 12;
            if self.u16(o) == want {
                let (typ, cnt) = (self.u16(o + 2), self.u32(o + 4));
                let sz = typ_sz(typ) * cnt as usize;
                let voff = if sz <= 4 { o + 8 } else { self.u32(o + 8) as usize };
                return Some((typ, cnt, voff));
            }
        }
        None
    }
    /// every IFD offset reachable via 0x14A SubIFDs + next-IFD chain
    fn ifds(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![self.u32(4) as usize];
        while let Some(ifd) = stack.pop() {
            if ifd + 2 > self.d.len() {
                continue;
            }
            out.push(ifd);
            if let Some((_, cnt, off)) = self.tag(ifd, 0x14A) {
                for i in 0..cnt as usize {
                    stack.push(self.u32(off + i * 4) as usize);
                }
            }
            let n = self.u16(ifd) as usize;
            let nxt = self.u32(ifd + 2 + n * 12) as usize;
            if nxt != 0 {
                stack.push(nxt);
            }
        }
        out
    }
}
fn typ_sz(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => 0,
    }
}

/// raw bytes of an arbitrary TIFF tag across the IFD chain/SubIFDs.
/// (tag 0x2BC = embedded XMP packet; used for DNG develop settings)
pub fn tag_bytes(path: &Path, want: u16) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 16 {
        return None;
    }
    let le = match &data[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r = Rd { d: &data, le };
    for ifd in r.ifds() {
        if let Some((_, cnt, off)) = r.tag(ifd, want) {
            let end = (off + cnt as usize).min(data.len());
            if off < end {
                return Some(data[off..end].to_vec());
            }
        }
    }
    None
}

/// SAFELIGHT_DNG=no_warp|no_gain disables one op class (A/B debugging)
pub fn env_filter(ops: &mut Option<DngOps>) {
    let off = std::env::var("SAFELIGHT_DNG").unwrap_or_default();
    if let Some(o) = ops {
        if off == "no_warp" {
            o.warp = None;
        }
        if off == "no_gain" {
            o.gain = None;
        }
        if o.warp.is_none() && o.gain.is_none() {
            *ops = None;
        }
    }
}

// ---- opcode stream (always big-endian) -------------------------------------

struct Be<'a>(&'a [u8]);
impl<'a> Be<'a> {
    fn u32(&self, o: usize) -> u32 {
        u32::from_be_bytes(self.0[o..o + 4].try_into().unwrap())
    }
    fn f64(&self, o: usize) -> f64 {
        f64::from_bits(u64::from_be_bytes(
            self.0[o..o + 8].try_into().unwrap(),
        ))
    }
    fn f32(&self, o: usize) -> f32 {
        f32::from_bits(u32::from_be_bytes(
            self.0[o..o + 4].try_into().unwrap(),
        ))
    }
}

fn parse_opcodes(d: &[u8]) -> DngOps {
    let b = Be(d);
    let mut ops = DngOps {
        warp: None,
        gain: None,
    };
    if d.len() < 4 {
        return ops;
    }
    let n = b.u32(0) as usize;
    let mut p = 4usize;
    for _ in 0..n.min(32) {
        if p + 16 > d.len() {
            break;
        }
        let (id, _ver, _flags, sz) = (b.u32(p), b.u32(p + 4), b.u32(p + 8), b.u32(p + 12));
        p += 16;
        let q = &d[p..(p + sz as usize).min(d.len())];
        match id {
            1 => {
                // WarpRectilinear
                let pb = Be(q);
                if q.len() >= 4 {
                    let planes = pb.u32(0).min(3) as usize;
                    let mut kr = [[0.0f32; 6]; 3];
                    let mut o = 4usize;
                    for pl in 0..planes {
                        for i in 0..6 {
                            if o + 8 <= q.len() {
                                kr[pl][i] = pb.f64(o) as f32;
                            }
                            o += 8;
                        }
                    }
                    let (cx, cy) = if o + 16 <= q.len() {
                        (pb.f64(o) as f32, pb.f64(o + 8) as f32)
                    } else {
                        (0.5, 0.5)
                    };
                    ops.warp = Some(WarpRect {
                        kr,
                        center: [cx, cy],
                    });
                }
            }
            9 => {
                // GainMap (dng_sdk: areaspec then dng_gain_map stream)
                let pb = Be(q);
                if q.len() >= 76 {
                    // dng_area_spec: t,l,b,r, plane, planes, rowPitch, colPitch
                    let plane0 = pb.u32(16) as usize;
                    let nplanes = pb.u32(20).max(1) as usize;
                    // dng_gain_map stream: mapPoints v/h, spacing v/h (f64),
                    // origin v/h (f64), planes, then mapG planes*rows*cols f32
                    let rows = pb.u32(32) as usize;
                    let cols = pb.u32(36) as usize;
                    let mut spacing = [pb.f64(40), pb.f64(48)];
                    let mut origin = [pb.f64(56), pb.f64(64)];
                    // spec: a 1-point axis ignores spacing/origin
                    if rows == 1 {
                        spacing[0] = 1.0;
                        origin[0] = 0.0;
                    }
                    if cols == 1 {
                        spacing[1] = 1.0;
                        origin[1] = 0.0;
                    }
                    let planes = pb.u32(72).max(1) as usize;
                    let mut o = 76usize;
                    let n = planes * rows * cols;
                    let mut map = Vec::with_capacity(n);
                    for _ in 0..n {
                        if o + 4 <= q.len() {
                            map.push(pb.f32(o));
                        } else {
                            map.push(1.0);
                        }
                        o += 4;
                    }
                    ops.gain = Some(GainMap {
                        plane0,
                        nplanes,
                        rows,
                        cols,
                        spacing,
                        origin,
                        planes,
                        map,
                    });
                }
            }
            _ => {}
        }
        p += sz as usize;
    }
    ops
}

/// read the lens-correction opcode lists (OpcodeList1 0xC740,
/// OpcodeList2 0xC741, OpcodeList3 0xC74E — merged in that order).
/// Returns None for non-DNG files.
pub fn parse(path: &Path) -> Option<DngOps> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 16 {
        return None;
    }
    let le = match &data[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r = Rd { d: &data, le };
    // must be a DNG
    let mut is_dng = false;
    for ifd in r.ifds() {
        if r.tag(ifd, 0xC612).is_some() {
            is_dng = true;
            break;
        }
    }
    if !is_dng {
        return None;
    }
    let mut merged = DngOps {
        warp: None,
        gain: None,
    };
    for ifd in r.ifds() {
        for tag in [0xC740u16, 0xC741, 0xC74E] {
            if let Some((_, cnt, off)) = r.tag(ifd, tag) {
                let end = (off + cnt as usize).min(data.len());
                let ops = parse_opcodes(&data[off..end]);
                if ops.warp.is_some() {
                    merged.warp = ops.warp;
                }
                if ops.gain.is_some() {
                    merged.gain = ops.gain;
                }
            }
        }
    }
    if merged.warp.is_none() && merged.gain.is_none() {
        None
    } else {
        Some(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dji_dng_ops() {
        let p = std::path::Path::new(r"G:\カメラ\DJI_action4\DJI_20240806091613_0023_D.DNG");
        if !p.exists() {
            return;
        }
        let ops = parse(p).expect("dji dng has opcodes");
        let w = ops.warp.expect("warp rectilinear");
        eprintln!("warp center={:?}", w.center);
        for ch in 0..3 {
            eprintln!("  kr[{ch}]={:?}", w.kr[ch]);
        }
        let g = ops.gain.expect("gainmap");
        eprintln!(
            "gain {}x{} planes={} spacing={:?} origin={:?} center_gain={:.3} corner_gain={:.3}",
            g.rows,
            g.cols,
            g.planes,
            g.spacing,
            g.origin,
            g.at(1824.0, 1368.0, 1, 3648, 2736),
            g.at(10.0, 10.0, 1, 3648, 2736)
        );
        // print first entries of each plane — should be smooth flat-field data
        for pl in 0..g.planes {
            for r in [0, g.rows / 2].iter().copied() {
                let row: Vec<String> = (0..g.cols.min(8))
                    .map(|c| format!("{:.3}", g.map[(r * g.cols + c) * g.planes + pl]))
                    .collect();
                eprintln!("  pl{pl} r{r}: {}", row.join(" "));
            }
        }
        // and channel spreads at a few positions
        for &(x, y) in &[(10.0, 10.0), (1824.0, 1368.0), (3600.0, 2700.0)] {
            let ch: Vec<f64> = (0..3).map(|c| g.at(x, y, c, 3648, 2736)).collect();
            eprintln!("  at({x},{y}): {:.3} {:.3} {:.3}", ch[0], ch[1], ch[2]);
        }
    }
}
