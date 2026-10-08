//! Sigma/Foveon X3F decoder — ported from dcraw.c (public domain) by Dave
//! Coffin. Handles the FOVb container, TRUE II huffman (Merrill/DPxM/SD1M,
//! pent=30) and the older SD packed/huffman formats (pent=5/6), plus the
//! CAMF metadata block (type-2 LCG-decrypted, type-4 huffman) and the full
//! foveon_interpolate colour pipeline.
use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};

use crate::decode::{CameraInfo, Decoded};

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn g1(&mut self) -> u32 {
        let b = *self.d.get(self.pos).unwrap_or(&0);
        self.pos += 1;
        b as u32
    }
    fn g2le(&mut self) -> u32 {
        let a = self.g1();
        let b = self.g1();
        a | (b << 8)
    }
    fn g4le(&mut self) -> u32 {
        let a = self.g2le();
        let b = self.g2le();
        a | (b << 16)
    }
    fn at(&self, off: usize) -> Reader<'a> {
        Reader { d: self.d, pos: off }
    }
    fn utf16(&self, off: usize, max: usize) -> String {
        let mut s = String::new();
        let mut p = off;
        for _ in 0..max {
            let lo = self.d.get(p).copied().unwrap_or(0) as u16;
            let hi = self.d.get(p + 1).copied().unwrap_or(0) as u16;
            let c = lo | (hi << 8);
            if c == 0 {
                break;
            }
            s.push(char::from_u32(c as u32).unwrap_or(' '));
            p += 2;
        }
        s.trim().to_string()
    }
}

/// big-endian u32 inside CAMF meta_data (dcraw sget4)
fn sget4(d: &[u8], off: usize) -> u32 {
    let g = |i: usize| d.get(off + i).copied().unwrap_or(0) as u32;
    (g(0) << 24) | (g(1) << 16) | (g(2) << 8) | g(3)
}

/// MSB-first bit reader mirroring dcraw's getbithuff (X3F streams never hit
/// the zero_after_ff path).
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    bitbuf: u64,
    vbits: i32,
}
impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Bits { d, pos, bitbuf: 0, vbits: 0 }
    }
    fn get(&mut self, nbits: i32) -> u32 {
        if nbits <= 0 {
            return 0;
        }
        while self.vbits < nbits {
            let b = match self.d.get(self.pos) {
                Some(&b) => b as u64,
                None => break,
            };
            self.pos += 1;
            self.bitbuf = (self.bitbuf << 8) + b;
            self.vbits += 8;
        }
        let c = (self.bitbuf << (64 - self.vbits) >> (64 - nbits)) as u32;
        self.vbits -= nbits;
        if self.vbits < 0 {
            self.vbits = 0;
        }
        c
    }
    /// gethuff: huff[0] = window bits; entry huff[1 + window] = (len<<8)|sym
    fn gethuff(&mut self, huff: &[u16]) -> u32 {
        let nb = huff[0] as i32;
        while self.vbits < nb {
            let b = match self.d.get(self.pos) {
                Some(&b) => b as u64,
                None => break,
            };
            self.pos += 1;
            self.bitbuf = (self.bitbuf << 8) + b;
            self.vbits += 8;
        }
        let c = (self.bitbuf << (64 - self.vbits) >> (64 - nb)) as usize;
        let e = *huff.get(1 + c).unwrap_or(&0);
        self.vbits -= (e >> 8) as i32;
        if self.vbits < 0 {
            self.vbits = 0;
        }
        (e & 0xff) as u32
    }
}

fn ljpeg_diff(bits: &mut Bits, huff: &[u16]) -> i32 {
    let len = bits.gethuff(huff) as i32;
    if len == 16 {
        return -32768;
    }
    let mut diff = bits.get(len) as i32;
    if len > 0 && (diff & (1 << (len - 1))) == 0 {
        diff -= (1 << len) - 1;
    }
    diff
}

struct X3f {
    width: usize,
    height: usize,
    flip: i32,
    data_offset: usize,
    image_type: u32,
    props: HashMap<String, String>,
    meta_offset: usize,
    meta_length: usize,
    jpeg_thumb: Option<(usize, usize)>,
}

fn parse_container(d: &[u8]) -> Result<X3f> {
    if d.len() < 64 || &d[0..4] != b"FOVb" {
        bail!("not a Foveon X3F");
    }
    let r = Reader { d, pos: 0 };
    let mut x = X3f {
        width: 0,
        height: 0,
        flip: 0,
        data_offset: 0,
        image_type: 0,
        props: HashMap::new(),
        meta_offset: 0,
        meta_length: 0,
        jpeg_thumb: None,
    };
    let mut rh = r.at(36);
    x.flip = rh.g4le() as i32;
    let dir_off = r.at(d.len() - 4).g4le() as usize;
    let mut dr = r.at(dir_off);
    if dr.g4le() != 0x6443_4553 {
        bail!("x3f: no SECd directory");
    }
    let _ = dr.g4le();
    let entries = dr.g4le();
    for _ in 0..entries.min(64) {
        let off = dr.g4le() as usize;
        let len = dr.g4le() as usize;
        let tag = dr.g4le();
        let mut sr = r.at(off);
        if sr.g4le() != (0x2043_4553 | (tag << 24)) {
            continue;
        }
        match tag {
            0x4741_4d49 | 0x3241_4d49 => {
                // 'IMAG' / 'IMA2'
                let mut ir = r.at(off + 8);
                let pent = ir.g4le();
                let wide = ir.g4le() as usize;
                let high = ir.g4le() as usize;
                if wide > x.width && high > x.height {
                    x.image_type = pent;
                    x.width = wide;
                    x.height = high;
                    x.data_offset = off + 28;
                }
                if d.get(off + 28).copied() == Some(0xff)
                    && d.get(off + 29).copied() == Some(0xd8)
                {
                    let jl = len.saturating_sub(28);
                    if x.jpeg_thumb.map_or(true, |(_, l)| jl > l) {
                        x.jpeg_thumb = Some((off + 28, jl));
                    }
                }
            }
            0x464d_4143 => {
                // 'CAMF'
                x.meta_offset = off + 8;
                x.meta_length = len.saturating_sub(28);
            }
            0x504f_5250 => {
                // 'PROP'
                let mut pr = r.at(off);
                let _ = pr.g4le();
                let pent = pr.g4le() as usize;
                pr.pos += 12;
                let base = off + pent * 8 + 24;
                let n = pent.min(256);
                let mut offs = Vec::with_capacity(n * 2);
                for _ in 0..n * 2 {
                    offs.push(base + (pr.g4le() as usize) * 2);
                }
                for i in 0..n {
                    let name = r.utf16(offs[i * 2], 128);
                    let value = r.utf16(offs[i * 2 + 1], 128);
                    if !name.is_empty() {
                        x.props.insert(name, value);
                    }
                }
            }
            _ => {}
        }
    }
    if x.width == 0 {
        bail!("x3f: no IMAG section");
    }
    Ok(x)
}

fn read_huff(r: &mut Reader) -> Vec<u16> {
    let mut huff = vec![0u16; 512];
    huff[0] = 8;
    for i in 0..13u32 {
        let clen = r.g1();
        let code = r.g1();
        let span = 256u32 >> clen;
        for j in 1..=span {
            let idx = (code + j) as usize;
            if idx < huff.len() {
                huff[idx] = ((clen << 8) | i) as u16;
            }
        }
    }
    let _ = r.g2le();
    huff
}

/// pent==30 (TRUE II): three full-res planes, delta+huffman each.
fn dp_load_raw(d: &[u8], x: &X3f) -> Vec<[i16; 3]> {
    let w = x.width;
    let h = x.height;
    let mut image = vec![[0i16; 3]; w * h];
    let mut r = Reader { d, pos: x.data_offset + 8 };
    let huff = read_huff(&mut r);
    let mut roff = [0usize; 4];
    roff[0] = 48;
    for c in 0..3 {
        let n = r.g4le() as usize;
        roff[c + 1] = (roff[c] + n + 15) & !15;
    }
    for c in 0..3 {
        let mut bits = Bits::new(d, x.data_offset + roff[c]);
        let mut vpred = [[512u16; 2]; 2];
        let mut hpred = [0u16; 2];
        for row in 0..h {
            for col in 0..w {
                let diff = ljpeg_diff(&mut bits, &huff);
                let px;
                if col < 2 {
                    vpred[row & 1][col] = vpred[row & 1][col].wrapping_add(diff as u16);
                    hpred[col] = vpred[row & 1][col];
                    px = hpred[col];
                } else {
                    hpred[col & 1] = hpred[col & 1].wrapping_add(diff as u16);
                    px = hpred[col & 1];
                }
                image[row * w + col][c] = px as i16;
            }
        }
    }
    image
}

struct DNode {
    leaf: u32,
    branch: [usize; 2],
}

/// dcraw foveon_decoder: build a binary decode tree from a 1024-entry table.
fn foveon_decoder(r: &mut Reader, size: usize) -> Vec<DNode> {
    let mut huff = Vec::with_capacity(size);
    for _ in 0..size {
        huff.push(r.g4le());
    }
    let mut out = vec![DNode { leaf: 0, branch: [0; 2] }];
    let mut stack = vec![(0usize, 0u32)];
    while let Some((cur, code)) = stack.pop() {
        if code != 0 {
            if let Some(i) = huff.iter().position(|&h| h == code) {
                out[cur].leaf = i as u32;
                continue;
            }
        }
        let len = code >> 27;
        if len > 26 {
            continue;
        }
        let ncode = ((len + 1) << 27) | ((code & 0x3ff_ffff) << 1);
        let b0 = out.len();
        out.push(DNode { leaf: 0, branch: [0; 2] });
        let b1 = out.len();
        out.push(DNode { leaf: 0, branch: [0; 2] });
        out[cur].branch = [b0, b1];
        stack.push((b0, ncode));
        stack.push((b1, ncode + 1));
    }
    out
}

/// pent==5 (packed 3x10-bit diffs) and pent==6 (huffman tree).
fn sd_load_raw(d: &[u8], x: &X3f, packed: bool, model_num: i32) -> Vec<[i16; 3]> {
    let w = x.width;
    let h = x.height;
    let mut image = vec![[0i16; 3]; w * h];
    let mut r = Reader { d, pos: x.data_offset };
    let mut diff = [0i16; 1024];
    for e in diff.iter_mut() {
        *e = r.g2le() as u16 as i16;
    }
    let nodes = if packed {
        Vec::new()
    } else {
        foveon_decoder(&mut r, 1024)
    };
    let mut bitbuf = 0u32;
    for row in 0..h {
        let mut pred = [0i32; 3];
        let mut bit: i32 = -1;
        if !packed && model_num < 14 {
            let _ = r.g4le();
        }
        for col in 0..w {
            if packed {
                bitbuf = r.g4le();
                for c in 0..3 {
                    let idx = ((bitbuf >> (c * 10)) & 0x3ff) as usize;
                    pred[2 - c] = pred[2 - c].wrapping_add(diff[idx] as i32);
                }
            } else {
                for c in 0..3 {
                    let mut di = 0usize;
                    while nodes.get(di).map_or(false, |n| n.branch[0] != 0) {
                        bit = (bit - 1) & 31;
                        if bit == 31 {
                            for _ in 0..4 {
                                bitbuf = (bitbuf << 8) + r.g1();
                            }
                        }
                        let b = ((bitbuf >> bit) & 1) as usize;
                        di = nodes[di].branch[b];
                    }
                    let leaf = nodes.get(di).map(|n| n.leaf).unwrap_or(0) as usize;
                    pred[c] =
                        pred[c].wrapping_add(diff[leaf.min(1023)] as i32);
                }
            }
            for c in 0..3 {
                image[row * w + col][c] = pred[c] as u16 as i16;
            }
        }
    }
    image
}

fn load_camf(d: &[u8], x: &X3f) -> Option<Vec<u8>> {
    if x.meta_offset == 0 || x.meta_length == 0 {
        return None;
    }
    let mut r = Reader { d, pos: x.meta_offset };
    let typ = r.g4le();
    let _ = r.g4le();
    let _ = r.g4le();
    let mut wide = r.g4le();
    let mut high = r.g4le();
    match typ {
        2 => {
            let start = r.pos;
            let n = x.meta_length.min(d.len().saturating_sub(start));
            let mut meta = d[start..start + n].to_vec();
            for b in meta.iter_mut() {
                high = (high.wrapping_mul(1597).wrapping_add(51749)) % 244944;
                wide = ((high as u64).wrapping_mul(301593171) >> 24) as u32;
                let k = ((((high << 8).wrapping_sub(wide)) >> 1).wrapping_add(wide)) >> 17;
                *b ^= (k & 0xff) as u8;
            }
            Some(meta)
        }
        4 => {
            let size = (wide as usize) * (high as usize) * 3 / 2;
            let mut meta = vec![0u8; size];
            let huff = read_huff(&mut r);
            let _ = r.g4le();
            let mut bits = Bits::new(d, r.pos);
            let mut vpred = [[512u16; 2]; 2];
            let mut hpred = [0u16; 2];
            let mut j = 0usize;
            for row in 0..high as usize {
                for col in 0..wide as usize {
                    let diff = ljpeg_diff(&mut bits, &huff);
                    if col < 2 {
                        hpred[col] = vpred[row & 1][col].wrapping_add(diff as u16);
                        vpred[row & 1][col] = hpred[col];
                    } else {
                        hpred[col & 1] = hpred[col & 1].wrapping_add(diff as u16);
                    }
                    if col & 1 == 1 {
                        if j + 2 < size {
                            meta[j] = (hpred[0] >> 4) as u8;
                            meta[j + 1] = ((hpred[0] << 4) | (hpred[1] >> 8)) as u8;
                            meta[j + 2] = hpred[1] as u8;
                        }
                        j += 3;
                    }
                }
            }
            Some(meta)
        }
        _ => {
            let start = r.pos;
            let n = x.meta_length.min(d.len().saturating_sub(start));
            Some(d[start..start + n].to_vec())
        }
    }
}

struct Camf<'a> {
    d: &'a [u8],
}
impl<'a> Camf<'a> {
    fn cstr(&self, off: usize) -> String {
        let mut s = String::new();
        let mut p = off;
        while p < self.d.len() && s.len() < 256 {
            let b = self.d[p];
            if b == 0 {
                break;
            }
            s.push(b as char);
            p += 1;
        }
        s.trim().to_string()
    }
    fn param(&self, block: &str, param: &str) -> Option<String> {
        let mut idx = 0usize;
        while idx + 20 < self.d.len() {
            if &self.d[idx..idx + 3] != b"CMb" {
                break;
            }
            let next = sget4(self.d, idx + 8) as usize;
            if self.d[idx + 3] == b'P' {
                let name = self.cstr(idx + sget4(self.d, idx + 12) as usize);
                if name == block {
                    let mut cp = idx + sget4(self.d, idx + 16) as usize;
                    let num = sget4(self.d, cp) as usize;
                    let dp = idx + sget4(self.d, cp + 4) as usize;
                    for _ in 0..num {
                        cp += 8;
                        let pn = self.cstr(dp + sget4(self.d, cp) as usize);
                        if pn == param {
                            return Some(self.cstr(dp + sget4(self.d, cp + 4) as usize));
                        }
                    }
                }
            }
            if next == 0 {
                break;
            }
            idx += next;
        }
        None
    }
    fn matrix(&self, name: &str) -> Option<([usize; 3], Vec<u32>)> {
        let mut idx = 0usize;
        while idx + 20 < self.d.len() {
            if &self.d[idx..idx + 3] != b"CMb" {
                break;
            }
            let next = sget4(self.d, idx + 8) as usize;
            if self.d[idx + 3] == b'M' {
                let nm = self.cstr(idx + sget4(self.d, idx + 12) as usize);
                if nm == name {
                    let mut cp = idx + sget4(self.d, idx + 16) as usize;
                    let typ = sget4(self.d, cp);
                    let ndim = sget4(self.d, cp + 4) as usize;
                    if ndim > 3 {
                        return None;
                    }
                    let dp = idx + sget4(self.d, cp + 8) as usize;
                    let mut dim = [1usize; 3];
                    for i in (0..ndim).rev() {
                        cp += 12;
                        dim[i] = sget4(self.d, cp) as usize;
                    }
                    let size = dim[0] * dim[1] * dim[2];
                    if size == 0 || size > self.d.len() / 2 {
                        return None;
                    }
                    let mut mat = Vec::with_capacity(size);
                    if typ != 0 && typ != 6 {
                        for i in 0..size {
                            mat.push(sget4(self.d, dp + i * 4));
                        }
                    } else {
                        for i in 0..size {
                            let o = dp + i * 2;
                            let hi = self.d.get(o).copied().unwrap_or(0) as u32;
                            let lo = self.d.get(o + 1).copied().unwrap_or(0) as u32;
                            mat.push((hi << 8) | lo);
                        }
                    }
                    return Some((dim, mat));
                }
            }
            if next == 0 {
                break;
            }
            idx += next;
        }
        None
    }
    fn fixed_f32(&self, out: &mut [f32], name: &str) -> bool {
        match self.matrix(name) {
            Some((_dim, mat)) => {
                for (o, m) in out.iter_mut().zip(mat.iter()) {
                    *o = f32::from_bits(*m);
                }
                true
            }
            None => false,
        }
    }
    fn fixed_i32(&self, out: &mut [i32], name: &str) -> bool {
        match self.matrix(name) {
            Some((_dim, mat)) => {
                for (o, m) in out.iter_mut().zip(mat.iter()) {
                    *o = *m as i32;
                }
                true
            }
            None => false,
        }
    }
}

fn foveon_make_curve(max: f64, mul: f64, filt: f64) -> Vec<i16> {
    let filt = if filt == 0.0 { 0.8 } else { filt };
    let size = (4.0 * std::f64::consts::PI * max / filt) as usize;
    let mut curve = vec![0i16; size + 1];
    curve[0] = size as i16;
    for i in 0..size {
        let x = i as f64 * filt / max / 4.0;
        curve[i + 1] =
            (((x.cos() + 1.0) / 2.0) * ((i as f64 * filt / mul).tanh()) * mul + 0.5) as i16;
    }
    curve
}
fn apply_curve(curve: &[i16], i: i32) -> i32 {
    if curve.is_empty() || (i.unsigned_abs() as usize) >= curve[0] as usize {
        return 0;
    }
    if i < 0 {
        -(curve[(1 - i) as usize] as i32)
    } else {
        curve[(1 + i) as usize] as i32
    }
}
fn foveon_avg(row: &[[i16; 3]], c: usize, range: [i32; 2], cfilt: f32) -> f32 {
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    let mut sum = 0.0f32;
    for i in range[0]..=range[1] {
        if i < 0 {
            continue;
        }
        let cur = row.get(i as usize).map(|p| p[c] as f32).unwrap_or(0.0);
        let prev = row
            .get((i - 1) as usize)
            .map(|p| p[c] as f32)
            .unwrap_or(0.0);
        let val = cur + (cur - prev) * cfilt;
        sum += val;
        if min > val {
            min = val;
        }
        if max < val {
            max = val;
        }
    }
    if range[1] - range[0] == 1 {
        return sum / 2.0;
    }
    (sum - min - max) / ((range[1] - range[0] - 1).max(1)) as f32
}

#[allow(clippy::too_many_lines)]
fn foveon_interpolate(
    image: &mut Vec<[i16; 3]>,
    width: usize,
    height: usize,
    camf: &Camf,
    model2: &str,
    rgb_cam: [[f32; 3]; 3],
) -> (usize, usize) {
    let w = width;
    let h = height;
    let mut dscr = [0i32; 4];
    let mut ppm = [0f32; 27];
    let mut satlev = [0i32; 3];
    let mut keep = [0i32; 4];
    let mut active = [0i32; 4];
    let mut chroma_dq = [0f32; 3];
    let mut color_dq = [0f32; 3];
    let mut cfilt = 0f32;

    camf.fixed_i32(&mut dscr, "DarkShieldColRange");
    camf.fixed_f32(&mut ppm, "PostPolyMatrix");
    camf.fixed_i32(&mut satlev, "SaturationLevel");
    camf.fixed_i32(&mut keep, "KeepImageArea");
    camf.fixed_i32(&mut active, "ActiveImageArea");
    camf.fixed_f32(&mut chroma_dq, "ChromaDQ");
    let cdq_name = if camf.param("IncludeBlocks", "ColorDQ").is_some() {
        "ColorDQ"
    } else {
        "ColorDQCamRGB"
    };
    camf.fixed_f32(&mut color_dq, cdq_name);
    if camf.param("IncludeBlocks", "ColumnFilter").is_some() {
        let mut cf = [0f32; 1];
        camf.fixed_f32(&mut cf, "ColumnFilter");
        cfilt = cf[0];
    }

    let mut ddft = [[[0f32; 2]; 3]; 3];
    let mut have_drift = false;
    if camf.param("IncludeBlocks", "DarkDrift").is_some() {
        let mut dd = [0f32; 12];
        if camf.fixed_f32(&mut dd, "DarkDrift") {
            ddft[1] = [[dd[0], dd[1]], [dd[2], dd[3]], [dd[4], dd[5]]];
            ddft[2] = [[dd[6], dd[7]], [dd[8], dd[9]], [dd[10], dd[11]]];
            have_drift = true;
        }
    }
    if !have_drift {
        for i in 0..2 {
            let name = if i == 0 { "DarkShieldTop" } else { "DarkShieldBottom" };
            let mut dstb = [0i32; 4];
            if camf.fixed_i32(&mut dstb, name) {
                let mut acc = [0f64; 3];
                let mut n = 0usize;
                for row in dstb[1]..=dstb[3] {
                    for col in dstb[0]..=dstb[2] {
                        if row >= 0 && col >= 0 && (row as usize) < h && (col as usize) < w {
                            for c in 0..3 {
                                acc[c] += image[row as usize * w + col as usize][c] as f64;
                            }
                            n += 1;
                        }
                    }
                }
                for c in 0..3 {
                    ddft[i + 1][c][1] = (acc[c] / n.max(1) as f64) as f32;
                }
            }
        }
    }

    let illum = camf.param("WhiteBalanceIlluminants", model2);
    let mut cam_xyz = [[0f32; 3]; 3];
    if let Some(ill) = &illum {
        let mut flat = [0f32; 9];
        if camf.fixed_f32(&mut flat, ill) {
            for i in 0..3 {
                for j in 0..3 {
                    cam_xyz[i][j] = flat[i * 3 + j];
                }
            }
        }
    }
    let mut correct = [[0f32; 3]; 3];
    if let Some(ill) = camf.param("WhiteBalanceCorrections", model2) {
        let mut flat = [0f32; 9];
        if camf.fixed_f32(&mut flat, &ill) {
            for i in 0..3 {
                for j in 0..3 {
                    correct[i][j] = flat[i * 3 + j];
                }
            }
        }
    }
    let mut last = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for c in 0..3 {
                last[i][j] += correct[i][c] * cam_xyz[c][j];
            }
        }
    }
    let mut diag = [[0f32; 3]; 3];
    for i in 0..3 {
        for c in 0..3 {
            let l = |x: usize, y: usize| last[(i + x) % 3][(c + y) % 3];
            diag[c][i] = l(1, 1) * l(2, 2) - l(1, 2) * l(2, 1);
        }
    }
    let mut div = [0f32; 3];
    for c in 0..3 {
        div[c] = diag[c][0] * 0.3127 + diag[c][1] * 0.329 + diag[c][2] * 0.3583;
    }
    let rgb_neutral = format!("{model2}RGBNeutral");
    if camf.param("IncludeBlocks", &rgb_neutral).is_some() {
        camf.fixed_f32(&mut div, &rgb_neutral);
    }
    let mut num = 0f32;
    for c in 0..3 {
        if num < div[c] {
            num = div[c];
        }
    }
    for c in 0..3 {
        div[c] /= num.max(1e-6);
    }

    let mut trans = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for c in 0..3 {
                trans[i][j] += rgb_cam[i][c] * last[c][j] * div[j];
            }
        }
    }
    let mut trsum = [0f64; 3];
    for c in 0..3 {
        trsum[c] = (trans[c][0] + trans[c][1] + trans[c][2]) as f64;
    }
    let mut dsum = (6.0 * trsum[0] + 11.0 * trsum[1] + 3.0 * trsum[2]) / 20.0;
    for i in 0..3 {
        for c in 0..3 {
            let denom = if trsum[i].abs() > 1e-9 { trsum[i] } else { 1.0 };
            last[i][c] = (trans[i][c] as f64 * dsum / denom) as f32;
        }
    }
    let mut trans2 = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for c in 0..3 {
                let k = if i == c { 32f32 } else { -1f32 };
                trans2[i][j] += k * last[c][j] / 30.0;
            }
        }
    }

    let mut mul = [0f64; 3];
    let mut max = 0f64;
    for c in 0..3 {
        mul[c] = (color_dq[c] / div[c].max(1e-6)) as f64;
        if max < mul[c] {
            max = mul[c];
        }
    }
    let mut curve: Vec<Vec<i16>> = Vec::with_capacity(8);
    for c in 0..3 {
        curve.push(foveon_make_curve(max.max(1.0), mul[c].max(1e-6), cfilt as f64));
    }
    let mut mul3 = [0f64; 3];
    let mut max3 = 0f64;
    for c in 0..3 {
        chroma_dq[c] /= 3.0;
        mul3[c] = (chroma_dq[c] / div[c].max(1e-6)) as f64;
        if max3 < mul3[c] {
            max3 = mul3[c];
        }
    }
    for c in 0..3 {
        curve.push(foveon_make_curve(max3.max(1.0), mul3[c].max(1e-6), cfilt as f64));
    }
    for c in 0..3 {
        dsum += (chroma_dq[c] / div[c].max(1e-6)) as f64;
    }
    curve.push(foveon_make_curve(dsum.max(1.0), dsum.max(1.0), cfilt as f64));
    curve.push(foveon_make_curve(
        (dsum * 2.0).max(1.0),
        (dsum * 2.0).max(1.0),
        cfilt as f64,
    ));

    let (sgdim, sgain) = match camf.matrix("SpatialGain") {
        Some(v) => v,
        None => {
            // no colour metadata — still emit the image; matrix-only fallback
            for p in image.iter_mut() {
                let mut o = [0f32; 3];
                for c in 0..3 {
                    for k in 0..3 {
                        o[c] += rgb_cam[c][k] * (p[k] as f32 / 8.0);
                    }
                }
                for c in 0..3 {
                    p[c] = o[c].clamp(0.0, 24000.0) as i16;
                }
            }
            return (w, h);
        }
    };
    let mut sgrow = vec![[0f32; 3]; sgdim[1].max(1)];
    let sgx = ((w + sgdim[1] - 2) / (sgdim[1].saturating_sub(1).max(1))).max(1);

    let mut black = vec![[0f32; 3]; h];
    for row in 0..h {
        let mut dd0 = [[0f32; 2]; 3];
        for c in 0..3 {
            for k in 0..2 {
                let t = row as f32 / (h.saturating_sub(1).max(1)) as f32;
                dd0[c][k] = ddft[1][c][k] * (1.0 - t) + ddft[2][c][k] * t;
            }
        }
        let rowpix = &image[row * w..(row * w + w).min(image.len())];
        for c in 0..3 {
            let a = foveon_avg(rowpix, c, [dscr[0], dscr[1]], cfilt);
            let b = foveon_avg(rowpix, c, [dscr[2], dscr[3]], cfilt);
            black[row][c] = (a + b * 3.0 - dd0[c][0]) / 4.0 - dd0[c][1];
        }
    }
    if h >= 8 {
        let src = black[8.min(h - 1)];
        for row in black.iter_mut().take(8) {
            *row = src;
        }
    }
    if h >= 22 {
        for k in 0..11 {
            black[h - 11 + k] = black[h - 22 + k];
        }
    }
    // 3-tap despeckle on raw copy, then exp smooth forward + backward
    let orig = black.clone();
    for row in 1..h.saturating_sub(1) {
        for c in 0..3 {
            let a = orig[row - 1][c];
            let b = orig[row][c];
            let d = orig[row + 1][c];
            if b > a {
                if b > d {
                    black[row][c] = a.max(d);
                }
            } else if b < d {
                black[row][c] = a.min(d);
            }
        }
    }
    if h > 0 {
        for c in 0..3 {
            black[h - 1][c] = (orig[h - 2][c] + orig[h - 1][c]) / 2.0;
            black[0][c] = (black[1.min(h - 1)][c] + black[3.min(h - 1)][c]) / 2.0;
        }
    }
    let val = 1.0 - (-1.0f64 / 24.0).exp() as f32;
    let mut fsum = if h > 0 { black[0] } else { [0f32; 3] };
    for row in 1..h {
        for c in 0..3 {
            black[row][c] = (black[row][c] - black[row - 1][c]) * val + black[row - 1][c];
            fsum[c] += black[row][c];
        }
    }
    for c in 0..3 {
        fsum[c] /= h.max(1) as f32;
    }
    let mut prevblk = if h > 0 { black[h - 1] } else { [0f32; 3] };
    for row in (0..h).rev() {
        for c in 0..3 {
            black[row][c] = (black[row][c] - fsum[c] - prevblk[c]) * val + prevblk[c];
            prevblk[c] = black[row][c];
        }
    }
    let mut total = [0i64; 4];
    for row in (2..h).step_by(4) {
        for col in (2..w).step_by(4) {
            if let Some(p) = image.get(row * w + col) {
                for c in 0..3 {
                    total[c] += p[c] as i64;
                }
                total[3] += 1;
            }
        }
    }
    for row in 0..h {
        for c in 0..3 {
            black[row][c] += fsum[c] / 2.0 + total[c] as f32 / (total[3].max(1) as f32 * 100.0);
        }
    }

    for row in 0..h {
        let mut dd0 = [[0f32; 2]; 3];
        for c in 0..3 {
            for k in 0..2 {
                let t = row as f32 / (h.saturating_sub(1).max(1)) as f32;
                dd0[c][k] = ddft[1][c][k] * (1.0 - t) + ddft[2][c][k] * t;
            }
        }
        let base = row * w;
        let mut prev = image[base];
        let frow = row as f32 / (h.saturating_sub(1).max(1)) as f32
            * (sgdim[2].saturating_sub(1)) as f32;
        let irow = (frow as usize).min(sgdim[2].saturating_sub(2));
        let fr = frow - irow as f32;
        for i in 0..sgdim[1] {
            for c in 0..3 {
                let a = sgain
                    .get(irow * sgdim[1] + i)
                    .map(|&v| f32::from_bits(v))
                    .unwrap_or(1.0);
                let b = sgain
                    .get((irow + 1) * sgdim[1] + i)
                    .map(|&v| f32::from_bits(v))
                    .unwrap_or(1.0);
                sgrow[i][c] = a * (1.0 - fr) + b * fr;
            }
        }
        for col in 0..w {
            let idx = base + col;
            let mut ipix = [0i32; 3];
            let mut work = [[0i32; 3]; 3];
            for c in 0..3 {
                let diff = image[idx][c] as i32 - prev[c] as i32;
                prev[c] = image[idx][c];
                let t = diff + ((diff * diff) >> 14);
                ipix[c] = image[idx][c] as i32
                    + (t as f32 * cfilt
                        - dd0[c][1]
                        - dd0[c][0] * (col as f32 / w as f32 - 0.5)
                        - black[row][c])
                        .floor() as i32;
            }
            for c in 0..3 {
                work[0][c] = ipix[c] * ipix[c] >> 14;
                work[2][c] = ipix[c] * work[0][c] >> 14;
                work[1][2 - c] = ipix[(c + 1) % 3] * ipix[(c + 2) % 3] >> 14;
            }
            for c in 0..3 {
                let mut v = 0f32;
                for i in 0..3 {
                    for j in 0..3 {
                        v += ppm[c * 9 + i * 3 + j] * work[i][j] as f32;
                    }
                }
                let gi = (col / sgx).min(sgrow.len().saturating_sub(2));
                let sg0 = sgrow[gi][c];
                let sg1 = sgrow[(gi + 1).min(sgrow.len() - 1)][c];
                let rem = (col % sgx) as f32;
                let gain = (sg0 * (sgx as f32 - rem) + sg1 * rem) / sgx as f32;
                let o = ((ipix[c] as f32 + v.floor()) * gain / div[c].max(1e-6)).floor()
                    as i32;
                image[idx][c] = (o.min(32000) as u16) as i16;
            }
        }
    }

    if let Some((bpdim, badpix)) = camf.matrix("BadPixels") {
        let hood = [-1i32, -1, -1, 0, -1, 1, 0, -1, 0, 1, 1, -1, 1, 0, 1, 1];
        for i in 0..bpdim[0] {
            let bp = badpix.get(i).copied().unwrap_or(0);
            let col = (bp >> 8 & 0xfff) as i64 - keep[0] as i64;
            let row = (bp >> 20) as i64 - keep[1] as i64;
            if row < 1 || col < 1 || row > h as i64 - 3 || col > w as i64 - 3 {
                continue;
            }
            let mut sum = 0i32;
            let mut fs = [0i32; 3];
            for j in 0..8 {
                if bp & (1 << j) != 0 {
                    let rr = (row + hood[j * 2] as i64) as usize;
                    let cc = (col + hood[j * 2 + 1] as i64) as usize;
                    if rr < h && cc < w {
                        for c in 0..3 {
                            fs[c] += image[rr * w + cc][c] as i32;
                        }
                        sum += 1;
                    }
                }
            }
            if sum > 0 {
                for c in 0..3 {
                    image[row as usize * w + col as usize][c] = (fs[c] / sum) as i16;
                }
            }
        }
    }

    // --- ring-buffer smoothing helpers (dcraw pointer rotation) ---
    // red sharpening against a 5x5 horizontal gaussian
    let mut ring: Vec<Vec<i32>> = (0..6).map(|_| vec![0; w]).collect();
    let mut smlast: i64 = -1;
    for row in 2..h.saturating_sub(2) {
        while smlast < (row + 2) as i64 {
            ring.rotate_left(1);
            smlast += 1;
            let r = smlast as usize;
            if r >= h {
                break;
            }
            for col in 2..w.saturating_sub(2) {
                let idx = r * w + col;
                let p = |k: i64| image[(idx as i64 + k) as usize][0] as i32;
                ring[4][col] = (p(0) * 6 + (p(-1) + p(1)) * 4 + p(-2) + p(2) + 8) >> 4;
            }
        }
        if smlast < (row + 2) as i64 {
            break;
        }
        let mut smred_p = 0i32;
        for col in 2..w.saturating_sub(2) {
            let smred = (6 * ring[2][col] + 4 * (ring[1][col] + ring[3][col])
                + ring[0][col]
                + ring[4][col]
                + 8)
                >> 4;
            if col == 2 {
                smred_p = smred;
            }
            let idx = row * w + col;
            let red = image[idx][0] as i32;
            image[idx][0] = (red + ((red - ((smred * 7 + smred_p) >> 3)) >> 3)).min(32000) as i16;
            smred_p = smred;
        }
    }

    // highlight linearity
    let mut min_sat = i32::MAX;
    for c in 0..3 {
        let s = if div[c].abs() > 1e-6 {
            (satlev[c] as f32 / div[c]) as i32
        } else {
            satlev[c]
        };
        if min_sat > s {
            min_sat = s;
        }
    }
    if min_sat <= 0 || min_sat == i32::MAX {
        min_sat = 1 << 30;
    }
    let limit = min_sat * 9 >> 4;
    for p in image.iter_mut() {
        if p[0] as i32 <= limit || p[1] as i32 <= limit || p[2] as i32 <= limit {
            continue;
        }
        let mut mn = p[0] as i32;
        let mut mx = p[0] as i32;
        for c in 1..3 {
            if mn > p[c] as i32 {
                mn = p[c] as i32;
            }
            if mx < p[c] as i32 {
                mx = p[c] as i32;
            }
        }
        if mn >= limit * 2 {
            p[0] = mx as i16;
            p[1] = mx as i16;
            p[2] = mx as i16;
        } else if limit > 0 {
            let mut i = 0x4000i32 - ((mn - limit) << 14) / limit;
            i = 0x4000 - (i * i >> 14);
            i = i * i >> 14;
            for c in 0..3 {
                let np = p[c] as i32 + ((mx - p[c] as i32) * i >> 14);
                p[c] = np.clamp(0, 32000) as i16;
            }
        }
    }

    // hue smoothing pass 1 (dev curve)
    let mut ring2: Vec<Vec<[i32; 3]>> = (0..6).map(|_| vec![[0; 3]; w]).collect();
    let mut smlast2: i64 = -1;
    for row in 2..h.saturating_sub(2) {
        while smlast2 < (row + 2) as i64 {
            ring2.rotate_left(1);
            smlast2 += 1;
            let r = smlast2 as usize;
            if r >= h {
                break;
            }
            for col in 2..w.saturating_sub(2) {
                for c in 0..3 {
                    let idx = r * w + col;
                    let a = image.get(idx - 1).map(|p| p[c] as i32).unwrap_or(0);
                    let b0 = image[idx][c] as i32;
                    let c1 = image.get(idx + 1).map(|p| p[c] as i32).unwrap_or(0);
                    ring2[4][col][c] = (a + 2 * b0 + c1 + 2) >> 2;
                }
            }
        }
        if smlast2 < (row + 2) as i64 {
            break;
        }
        for col in 2..w.saturating_sub(2) {
            let idx = row * w + col;
            let mut dev = [0i32; 3];
            for c in 0..3 {
                let sm = (ring2[1][col][c] + 2 * ring2[2][col][c] + ring2[3][col][c]) >> 2;
                dev[c] = -apply_curve(&curve[7], image[idx][c] as i32 - sm);
            }
            let sum = (dev[0] + dev[1] + dev[2]) >> 3;
            for c in 0..3 {
                image[idx][c] = (image[idx][c] as i32 + dev[c] - sum) as i16;
            }
        }
    }

    // hue smoothing pass 2 (luma-proportional chroma)
    let mut ring3: Vec<Vec<[i32; 3]>> = (0..6).map(|_| vec![[0; 3]; w]).collect();
    let mut smlast3: i64 = -1;
    for row in 2..h.saturating_sub(2) {
        while smlast3 < (row + 2) as i64 {
            ring3.rotate_left(1);
            smlast3 += 1;
            let r = smlast3 as usize;
            if r >= h {
                break;
            }
            for col in 2..w.saturating_sub(2) {
                for c in 0..3 {
                    let idx = r * w + col;
                    let v = image.get(idx - 2).map(|p| p[c] as i32).unwrap_or(0)
                        + image.get(idx - 1).map(|p| p[c] as i32).unwrap_or(0)
                        + image[idx][c] as i32
                        + image.get(idx + 1).map(|p| p[c] as i32).unwrap_or(0)
                        + image.get(idx + 2).map(|p| p[c] as i32).unwrap_or(0);
                    ring3[4][col][c] = (v + 2) >> 2;
                }
            }
        }
        if smlast3 < (row + 2) as i64 {
            break;
        }
        for col in 2..w.saturating_sub(2) {
            let idx = row * w + col;
            let mut total3 = 375i64;
            let mut sumpix = 60i64;
            let mut totalc = [0i64; 3];
            for c in 0..3 {
                for i in 0..5 {
                    totalc[c] += ring3[i][col][c] as i64;
                }
                total3 += totalc[c];
                sumpix += image[idx][c] as i64;
            }
            if sumpix < 0 {
                sumpix = 0;
            }
            let j = if total3 > 375 {
                (sumpix << 16) / total3
            } else {
                sumpix * 174
            };
            for c in 0..3 {
                let tgt = ((j * totalc[c] + 0x8000) >> 16) as i32;
                let nv =
                    image[idx][c] as i32 + apply_curve(&curve[6], tgt - image[idx][c] as i32);
                image[idx][c] = nv.clamp(0, i16::MAX as i32) as i16;
            }
        }
    }

    // colorspace transform
    for p in image.iter_mut() {
        let mut pixv = [0i32; 3];
        for c in 0..3 {
            pixv[c] = p[c] as i32 - apply_curve(&curve[c], p[c] as i32);
        }
        let sum = (pixv[0] + pixv[1] + pixv[1] + pixv[2]) >> 2;
        for c in 0..3 {
            pixv[c] -= apply_curve(&curve[c], pixv[c] - sum);
        }
        for c in 0..3 {
            let mut dv = 0f64;
            for i in 0..3 {
                dv += trans2[c][i] as f64 * pixv[i] as f64;
            }
            p[c] = dv.clamp(0.0, 24000.0) as i16;
        }
    }

    // 1/4-scale smoothing, then pull chroma toward smooth values
    let qw = w / 4;
    let qh = h / 4;
    let mut shrink = vec![[0i32; 3]; qw * qh];
    for row in (0..qh).rev() {
        for col in 0..qw {
            let mut ip = [0i32; 3];
            for i in 0..4 {
                for j in 0..4 {
                    let idx = (row * 4 + i) * w + col * 4 + j;
                    if idx < image.len() {
                        for c in 0..3 {
                            ip[c] += image[idx][c] as i32;
                        }
                    }
                }
            }
            for c in 0..3 {
                shrink[row * qw + col][c] = if row + 2 > qh {
                    ip[c] >> 4
                } else {
                    let pv = shrink[(row + 1) * qw + col][c];
                    (pv * 1840 + ip[c] * 141 + 2048) >> 12
                };
            }
        }
    }
    let mut sm0 = vec![[0i32; 3]; w & !3];
    let mut sm1 = vec![[0i32; 3]; w & !3];
    let mut sm2 = vec![[0i32; 3]; w & !3];
    for row in 0..(h & !3) {
        let mut acc = [0i32; 3];
        if row & 3 == 0 {
            for col in (0..(w & !3)).rev() {
                for c in 0..3 {
                    acc[c] =
                        (shrink[(row / 4) * qw + col / 4][c] * 1485 + acc[c] * 6707 + 4096) >> 13;
                    sm0[col][c] = acc[c];
                }
            }
        }
        acc = [0i32; 3];
        for col in 0..(w & !3) {
            for c in 0..3 {
                acc[c] = (sm0[col][c] * 1485 + acc[c] * 6707 + 4096) >> 13;
                sm1[col][c] = acc[c];
            }
        }
        if row == 0 {
            sm2.copy_from_slice(&sm1);
        } else {
            for col in 0..(w & !3) {
                for c in 0..3 {
                    sm2[col][c] = (sm2[col][c] * 6707 + sm1[col][c] * 1485 + 4096) >> 13;
                }
            }
        }
        for col in 0..(w & !3) {
            let idx = row * w + col;
            if idx >= image.len() {
                break;
            }
            let mut isum = 30i32;
            let mut jsum = 30i64;
            for c in 0..3 {
                isum += sm2[col][c];
                jsum += image[idx][c] as i64;
            }
            let jj = if isum != 0 { ((jsum << 16) / isum as i64) as i32 } else { 0 };
            let mut sum = 0i32;
            let mut ip = [0i32; 3];
            for c in 0..3 {
                ip[c] = apply_curve(
                    &curve[c + 3],
                    ((sm2[col][c] * jj + 0x8000) >> 16) - image[idx][c] as i32,
                );
                sum += ip[c];
            }
            sum >>= 3;
            for c in 0..3 {
                let nv = image[idx][c] as i32 + ip[c] - sum;
                image[idx][c] = nv.clamp(0, i16::MAX as i32) as i16;
            }
        }
    }

    // trim to active area
    if active.iter().any(|&v| v != 0) {
        let a1 = active[1].saturating_sub(keep[1]).max(0) as usize;
        let a0 = active[0].max(0) as usize;
        let a3 = (active[3] - 2).max(0) as usize;
        let neww = (active[2] - active[0]).max(1) as usize;
        let newh = a3.saturating_sub(a1).max(1).min(h.saturating_sub(a1));
        let neww = neww.min(w.saturating_sub(a0));
        let mut out = vec![[0i16; 3]; neww * newh];
        for row in 0..newh {
            for col in 0..neww {
                out[row * neww + col] = image[(row + a1) * w + col + a0];
            }
        }
        *image = out;
        return (neww, newh);
    }
    (w, h)
}

/// camera→linear-sRGB for the detected Sigma model (adobe_coeff-derived).
fn sigma_rgb_cam(model: &str) -> [[f32; 3]; 3] {
    let cam_xyz: [f32; 9] = if model.contains("Quattro") {
        [11648.0, -4868.0, -1108.0, -3779.0, 12520.0, 2734.0, 105.0, -1227.0, 10993.0]
    } else if model.contains("Merrill") || model.contains("SD1") {
        [5133.0, -1895.0, -353.0, 4978.0, 744.0, 144.0, 3837.0, 3069.0, 2777.0]
    } else if model.contains("SD15") || model.contains("SD14") {
        [11748.0, -3767.0, -1033.0, -2770.0, 10770.0, 2075.0, -3266.0, -544.0, 5968.0]
    } else {
        [11850.0, -4184.0, -1257.0, -3693.0, 11551.0, 2065.0, -3246.0, -383.0, 8200.0]
    };
    let mut m = [[0f32; 3]; 3];
    for i in 0..3 {
        let s = (cam_xyz[i * 3] + cam_xyz[i * 3 + 1] + cam_xyz[i * 3 + 2]).max(1e-6);
        for j in 0..3 {
            m[i][j] = cam_xyz[i * 3 + j] / s;
        }
    }
    let inv = invert3(m).unwrap_or(m);
    const XYZ2SRGB: [[f32; 3]; 3] = [
        [3.240_454_2, -1.537_138_5, -0.498_531_4],
        [-0.969_266_0, 1.876_010_8, 0.041_556_0],
        [0.055_643_4, -0.204_025_9, 1.057_225_2],
    ];
    let mut out = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                out[i][j] += XYZ2SRGB[i][k] * inv[k][j];
            }
        }
    }
    out
}

fn invert3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-8 {
        return None;
    }
    let id = 1.0 / det;
    let mut inv = [[0f32; 3]; 3];
    inv[0][0] = (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * id;
    inv[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * id;
    inv[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * id;
    inv[1][0] = (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * id;
    inv[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * id;
    inv[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * id;
    inv[2][0] = (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * id;
    inv[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * id;
    inv[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * id;
    Some(inv)
}

pub fn is_x3f(path: &Path) -> bool {
    std::fs::File::open(path)
        .map(|mut f| {
            use std::io::Read;
            let mut b = [0u8; 4];
            f.read_exact(&mut b).is_ok() && &b == b"FOVb"
        })
        .unwrap_or(false)
}

pub fn probe_x3f(path: &Path) -> Option<CameraInfo> {
    let d = std::fs::read(path).ok()?;
    let x = parse_container(&d).ok()?;
    let g = |k: &str| x.props.get(k).cloned().unwrap_or_default();
    Some(CameraInfo {
        make: g("CAMMANUF"),
        model: g("CAMMODEL"),
        lens: String::new(),
        iso: g("ISO").parse().unwrap_or(0.0),
        shutter: g("EXPTIME").parse::<f32>().unwrap_or(0.0) / 1e6,
        aperture: g("APERTURE").parse().unwrap_or(0.0),
        focal: g("FLENGTH").parse().unwrap_or(0.0),
        timestamp: g("TIME").parse().unwrap_or(0),
        flip: x.flip,
    })
}

/// decode an X3F to an sRGB-encoded rgba16 raster (Foveon pixels carry all
/// three channels — no CFA/demosaic involved).
pub fn decode_x3f(path: &Path) -> Result<Decoded> {
    let d = std::fs::read(path)?;
    let x = parse_container(&d)?;
    let model = x.props.get("CAMMODEL").cloned().unwrap_or_default();
    // atoi(model+2) equivalent — digits after the 2-letter prefix
    let model_num: i32 = model
        .get(2..)
        .unwrap_or("")
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut image = match x.image_type {
        30 => dp_load_raw(&d, &x),
        5 => sd_load_raw(&d, &x, true, model_num),
        6 => sd_load_raw(&d, &x, false, model_num),
        _ => bail!("x3f: unsupported image type {}", x.image_type),
    };
    let (mut w, mut h) = (x.width, x.height);

    let rgb_cam = sigma_rgb_cam(&model);
    if let Some(meta) = load_camf(&d, &x) {
        let camf = Camf { d: &meta };
        let model2 = x.props.get("WB_DESC").cloned().unwrap_or_default();
        let (nw, nh) = foveon_interpolate(&mut image, w, h, &camf, &model2, rgb_cam);
        w = nw;
        h = nh;
    } else {
        for p in image.iter_mut() {
            let mut o = [0f32; 3];
            for c in 0..3 {
                for k in 0..3 {
                    o[c] += rgb_cam[c][k] * (p[k] as f32 / 8.0);
                }
            }
            for c in 0..3 {
                p[c] = o[c].clamp(0.0, 24000.0) as i16;
            }
        }
    }

    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in &image {
        for c in 0..3 {
            let v = (p[c].max(0) as f32) / 24000.0;
            let e = if v <= 0.003_130_8 {
                v * 12.92
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            rgba.push((e.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16);
        }
        rgba.push(u16::MAX);
    }
    let g = |k: &str| x.props.get(k).cloned().unwrap_or_default();
    Ok(Decoded::Raster {
        rgba,
        w,
        h,
        info: CameraInfo {
            make: g("CAMMANUF"),
            model,
            lens: String::new(),
            iso: g("ISO").parse().unwrap_or(0.0),
            shutter: g("EXPTIME").parse::<f32>().unwrap_or(0.0) / 1e6,
            aperture: g("APERTURE").parse().unwrap_or(0.0),
            focal: g("FLENGTH").parse().unwrap_or(0.0),
            timestamp: g("TIME").parse().unwrap_or(0),
            flip: x.flip,
        },
        flip: x.flip,
    })
}

/// byte range of the largest embedded JPEG preview (Quattro/unsupported
/// types still open via this).
pub fn x3f_embedded_jpeg(path: &Path) -> Option<(usize, usize)> {
    let d = std::fs::read(path).ok()?;
    let x = parse_container(&d).ok()?;
    x.jpeg_thumb
}
