//! Development pipeline (CPU reference implementation).
//! Stages: normalize -> demosaic -> WB -> camera->sRGB matrix ->
//!         tone/color adjustments -> sRGB gamma -> orient.
//! The same math is mirrored by the wgpu compute pipeline in gpu.rs.
use crate::decode::{Decoded, Mosaic};
use crate::recipe::{Recipe, WbMode};

#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    /// rgba8
    pub data: Vec<u8>,
}

/// CIE XYZ(D65) -> linear sRGB (kept for future ICC/custom-matrix paths)
#[allow(dead_code)]
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// Shared per-image derived parameters (identical math in gpu.rs).
#[derive(Debug, Clone)]
pub struct Params {
    pub wb: [f32; 3],
    /// camera -> linear sRGB matrix incl. WB
    pub m: [[f32; 3]; 3],
    pub lut: Vec<f32>,
    pub exposure_mul: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub sharpen: f32,
    pub noise_luma: f32,
    pub rotation_deg: f32,
    pub clarity: f32,
    pub vignette: f32,
    pub grain: f32,
}

fn inv3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let [a, b, c] = m[0];
    let [d, e, f] = m[1];
    let [g, h, i] = m[2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-8 {
        return None;
    }
    let id = 1.0 / det;
    Some([
        [
            (e * i - f * h) * id,
            (c * h - b * i) * id,
            (b * f - c * e) * id,
        ],
        [
            (f * g - d * i) * id,
            (a * i - c * g) * id,
            (c * d - a * f) * id,
        ],
        [
            (d * h - e * g) * id,
            (b * g - a * h) * id,
            (a * e - b * d) * id,
        ],
    ])
}

fn mat_mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0.0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    o
}

fn norm_rows(mut m: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    for r in 0..3 {
        let s: f32 = m[r].iter().sum();
        if s.abs() > 1e-6 {
            for c in 0..3 {
                m[r][c] /= s;
            }
        }
    }
    m
}

fn catmull_lut(points: &[[f32; 2]]) -> Vec<f32> {
    // build a sorted point list incl. endpoints
    let mut pts: Vec<[f32; 2]> = Vec::new();
    pts.push([0.0, 0.0]);
    let mut mid: Vec<[f32; 2]> = points
        .iter()
        .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
        .collect();
    mid.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    mid.retain(|p| p[0] > 0.0 && p[0] < 1.0);
    pts.extend(mid);
    pts.push([1.0, 1.0]);
    let n = pts.len();
    let mut lut = Vec::with_capacity(256);
    for i in 0..256 {
        let x = i as f32 / 255.0;
        // find segment
        let mut k = 0;
        while k + 1 < n && pts[k + 1][0] < x {
            k += 1;
        }
        let p0 = pts[k.saturating_sub(1)];
        let p1 = pts[k];
        let p2 = pts[(k + 1).min(n - 1)];
        let p3 = pts[(k + 2).min(n - 1)];
        let dx = (p2[0] - p1[0]).max(1e-6);
        let t = ((x - p1[0]) / dx).clamp(0.0, 1.0);
        let t2 = t * t;
        let t3 = t2 * t;
        let y = 0.5
            * ((2.0 * p1[1])
                + (-p0[1] + p2[1]) * t
                + (2.0 * p0[1] - 5.0 * p1[1] + 4.0 * p2[1] - p3[1]) * t2
                + (-p0[1] + 3.0 * p1[1] - 3.0 * p2[1] + p3[1]) * t3);
        lut.push(y.clamp(0.0, 1.0));
    }
    lut
}

pub fn build_params(m: &Mosaic, r: &Recipe, auto_means: Option<[f32; 3]>) -> Params {
    // as-shot WB: prefer libraw's effective pre_mul (dcraw's actual channel
    // scaling), normalized to green. Fall back to cam_mul when absent.
    let mut wb = {
        let pm = m.pre_mul;
        if pm[1] > 0.0 && pm[0] > 0.0 && pm[2] > 0.0 {
            [pm[0] / pm[1], 1.0, pm[2] / pm[1]]
        } else {
            let g = if m.cam_mul[1] > 0.0 { m.cam_mul[1] } else { 1.0 };
            [m.cam_mul[0] / g, 1.0, if m.cam_mul[2] > 0.0 { m.cam_mul[2] / g } else { 1.0 }]
        }
    };
    if r.wb_mode == WbMode::Auto {
        if let Some(means) = auto_means {
            let gm = means[1].max(1e-4);
            wb = [gm / means[0].max(1e-4), 1.0, gm / means[2].max(1e-4)];
        }
    }
    // relative temperature/tint shift (v1 approximation)
    let t = r.temperature.clamp(-1.0, 1.0);
    let ti = r.tint.clamp(-1.0, 1.0);
    wb[0] *= 1.0 + t * 0.35 + ti * 0.12;
    wb[1] *= 1.0 - ti * 0.30;
    wb[2] *= 1.0 - t * 0.35 + ti * 0.12;

    // cam -> sRGB: use libraw's precomputed rgb_cam (cam->sRGB, row-normalized).
    // WB is applied separately with a post-WB clip so blown highlights stay
    // white instead of shifting magenta.
    let cam2srgb = [
        [m.rgb_cam[0][0], m.rgb_cam[0][1], m.rgb_cam[0][2]],
        [m.rgb_cam[1][0], m.rgb_cam[1][1], m.rgb_cam[1][2]],
        [m.rgb_cam[2][0], m.rgb_cam[2][1], m.rgb_cam[2][2]],
    ];

    Params {
        wb,
        m: cam2srgb,
        lut: catmull_lut(&r.curve),
        exposure_mul: (2.0f32).powf(r.exposure),
        contrast: r.contrast,
        highlights: r.highlights,
        shadows: r.shadows,
        whites: r.whites,
        blacks: r.blacks,
        saturation: r.saturation,
        vibrance: r.vibrance,
        sharpen: r.sharpen,
        noise_luma: r.noise_luma,
        rotation_deg: r.rotation_deg.clamp(-45.0, 45.0),
        clarity: r.clarity.clamp(-1.0, 1.0),
        vignette: r.vignette.clamp(-1.0, 1.0),
        grain: r.grain.clamp(0.0, 1.0),
    }
}

/// smoothstep(edge0, edge1, x) helper
fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn srgb_encode(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// apply tone+color adjustments on linear sRGB triple. Shared logic with gpu shader.
fn adjust(rgb: [f32; 3], p: &Params) -> [f32; 3] {
    let mut x = rgb;
    // exposure
    for c in &mut x {
        *c *= p.exposure_mul;
    }
    // blacks / whites remap
    for c in &mut x {
        if p.blacks > 0.0 {
            *c += p.blacks * 0.15 * (1.0 - *c);
        } else {
            *c *= 1.0 + p.blacks * 0.20;
        }
        *c *= 1.0 + p.whites * 0.20;
    }
    // contrast around 18% pivot
    for c in &mut x {
        *c = (*c - 0.18) * (1.0 + p.contrast * 0.9) + 0.18;
    }
    // shadows lift / highlights recovery in upper/lower bands
    for c in &mut x {
        if p.shadows != 0.0 {
            let w = (1.0 - sstep(0.0, 0.45, *c)) * sstep(0.0, 0.06, *c);
            *c += p.shadows * w * 0.30;
        }
        if p.highlights != 0.0 {
            let w = sstep(0.45, 1.2, *c);
            *c += -p.highlights * w * 0.5 * *c;
        }
    }
    // saturation / vibrance
    if p.saturation != 0.0 || p.vibrance != 0.0 {
        let luma = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let mx = x[0].max(x[1]).max(x[2]);
        let mn = x[0].min(x[1]).min(x[2]);
        let sat_now = if mx > 1e-5 { (mx - mn) / mx } else { 0.0 };
        let vib = p.vibrance * (1.0 - sat_now);
        let s = 1.0 + p.saturation + vib;
        for c in &mut x {
            *c = luma + (*c - luma) * s.max(0.0);
        }
    }
    // tone curve lut
    for c in &mut x {
        let idx = (c.clamp(0.0, 1.0) * 255.0) as usize;
        *c = p.lut[idx.min(255)];
    }
    x
}

/// generic same-colour-mean demosaic (works for bayer & xtrans).
/// Each virtual pixel covers a `stride`x`stride` block of sensor pixels at
/// (left + vx*stride, top + vy*stride). The CFA phase is computed on raw
/// sensor coordinates, which is what libraw's filters/xtrans refer to.
/// When a channel has no samples inside the block (small stride), the
/// window is widened until every channel is covered.
fn demosaic_pixel(
    m: &Mosaic,
    vx: usize,
    vy: usize,
    stride: usize,
    norm: &[f32; 4],
) -> [f32; 3] {
    let cfa = &m.cfa;
    let x0 = m.left + vx * stride;
    let y0 = m.top + vy * stride;
    let x_end = m.left + m.w;
    let y_end = m.top + m.h;
    let mut sums = [0.0f32; 3];
    let mut cnts = [0u32; 3];
    let mut pad = 0usize;
    for _ in 0..6 {
        sums = [0.0; 3];
        cnts = [0; 3];
        let ys = y0.saturating_sub(pad).max(m.top);
        let ye = (y0 + stride + pad).min(y_end);
        let xs = x0.saturating_sub(pad).max(m.left);
        let xe = (x0 + stride + pad).min(x_end);
        for sy in ys..ye {
            let row = sy * m.raw_w;
            for sx in xs..xe {
                let col = cfa.color_at(sx % cfa.w, sy % cfa.h) as usize;
                let cc = if col == 3 { 1 } else { col };
                let raw = m.data[row + sx] as f32;
                sums[cc] += (raw - m.black[col.min(3)]) * norm[col.min(3)];
                cnts[cc] += 1;
            }
        }
        if cnts[0] > 0 && cnts[1] > 0 && cnts[2] > 0 {
            break;
        }
        pad += 2;
    }
    let mut out = [0.0f32; 3];
    for ch in 0..3 {
        if cnts[ch] > 0 {
            out[ch] = sums[ch] / cnts[ch] as f32;
        }
    }
    out
}

/// black level + white point normalization factors per CFA colour index.
fn norm_factors(m: &Mosaic) -> [f32; 4] {
    let mut n = [0.0f32; 4];
    for c in 0..4 {
        let denom = (m.maximum as f32 - m.black[c]).max(1.0);
        n[c] = 1.0 / denom;
    }
    n
}

/// Full development: decoded -> rgba8 oriented, fit inside max_px (0 = full res).
pub fn develop_cpu(d: &Decoded, r: &Recipe, max_px: u32) -> RgbaImage {
    match d {
        Decoded::Mosaic(m) => develop_mosaic(m, r, max_px),
        Decoded::Raster {
            rgba, w, h, flip, ..
        } => develop_raster(rgba, *w, *h, *flip, r, max_px),
    }
}

fn develop_mosaic(m: &Mosaic, r: &Recipe, max_px: u32) -> RgbaImage {
    // virtual stride: subsample to roughly fit max_px, keeping CFA phase
    let period = m.cfa.h.max(1);
    let mut stride = 1usize;
    if max_px > 0 {
        let ratio = (m.w.max(m.h) as f32 / max_px as f32).max(1.0);
        stride = ((ratio / period as f32).floor() as usize).max(1) * period;
    }
    let vw = m.w / stride;
    let vh = m.h / stride;
    let norm = norm_factors(m);

    // demosaic + matrix (need scene means first when auto WB)
    let auto_means = if r.wb_mode == WbMode::Auto {
        let mut acc = [0.0f64; 3];
        let mut cnt = 0u64;
        let step = 4usize; // sparse sample
        for vy in (0..vh).step_by(step) {
            for vx in (0..vw).step_by(step) {
                let c = demosaic_pixel(m, vx, vy, stride, &norm);
                for ch in 0..3 {
                    acc[ch] += c[ch] as f64;
                }
                cnt += 1;
            }
        }
        Some([
            (acc[0] / cnt as f64) as f32,
            (acc[1] / cnt as f64) as f32,
            (acc[2] / cnt as f64) as f32,
        ])
    } else {
        None
    };
    let p = build_params(m, r, auto_means);

    let mut lin = vec![[0.0f32; 3]; vw * vh];
    for vy in 0..vh {
        for vx in 0..vw {
            let cam = demosaic_pixel(m, vx, vy, stride, &norm);
            // WB in camera space, clip at sensor white -> neutral highlights
            let cw = [
                (cam[0] * p.wb[0]).clamp(0.0, 1.0),
                (cam[1] * p.wb[1]).clamp(0.0, 1.0),
                (cam[2] * p.wb[2]).clamp(0.0, 1.0),
            ];
            let mut out = [0.0f32; 3];
            for c in 0..3 {
                out[c] = p.m[c][0] * cw[0] + p.m[c][1] * cw[1] + p.m[c][2] * cw[2];
            }
            lin[vy * vw + vx] = out;
        }
    }

    finish_linear(lin, vw, vh, &p, m.info.flip, max_px)
}

fn develop_raster(
    rgba: &[u16],
    w: usize,
    h: usize,
    flip: i32,
    r: &Recipe,
    max_px: u32,
) -> RgbaImage {
    let fake = Mosaic {
        data: Vec::new(),
        raw_w: 0,
        raw_h: 0,
        left: 0,
        top: 0,
        w: 0,
        h: 0,
        black: [0.0; 4],
        maximum: 65535,
        cam_mul: [1.0; 4],
        cam_xyz: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]],
        rgb_cam: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
        pre_mul: [1.0; 4],
        cfa: crate::decode::CfaPattern {
            w: 2,
            h: 2,
            cells: vec![0, 1, 1, 2],
        },
        info: crate::decode::CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip,
        },
    };
    let p = build_params(&fake, r, None);
    // raster is already sRGB-encoded u16: decode gamma to linear, adjust, re-encode
    let mut lin = vec![[0.0f32; 3]; w * h];
    for (i, px) in rgba.chunks_exact(4).enumerate() {
        for c in 0..3 {
            let v = px[c] as f32 / 65535.0;
            lin[i][c] = if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            };
        }
    }
    finish_linear(lin, w, h, &p, flip, max_px)
}

/// shared tail: spatial ops -> straighten/flip/fit-resize -> adjust ->
/// grain/vignette -> gamma. Same ordering as the gpu.rs finish pass.
fn finish_linear(
    mut lin: Vec<[f32; 3]>,
    w: usize,
    h: usize,
    p: &Params,
    flip: i32,
    max_px: u32,
) -> RgbaImage {
    if p.noise_luma > 0.0 {
        lin = chroma_smooth(&lin, w, h, p.noise_luma);
    }
    if p.sharpen > 0.0 || p.clarity != 0.0 {
        lin = sharpen_clarity(&lin, w, h, p.sharpen, p.clarity);
    }
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let (dw, dh) = if max_px > 0 { fit(fw as u32, fh as u32, max_px) } else { (fw as u32, fh as u32) };
    let (dw, dh) = (dw.max(1) as usize, dh.max(1) as usize);
    let cx = (fw as f32 - 1.0) * 0.5;
    let cy = (fh as f32 - 1.0) * 0.5;
    let sin = p.rotation_deg.to_radians().sin();
    let cos = p.rotation_deg.to_radians().cos();
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        for dx in 0..dw {
            // dst -> post-flip frame, undo straighten, undo flip -> src
            let mut fx = (dx as f32 + 0.5) * fw as f32 / dw as f32 - 0.5;
            let mut fy = (dy as f32 + 0.5) * fh as f32 / dh as f32 - 0.5;
            let px = fx - cx;
            let py = fy - cy;
            fx = cx + px * cos + py * sin;
            fy = cy - px * sin + py * cos;
            let (sx, sy) = match flip {
                3 => (w as f32 - 1.0 - fx, h as f32 - 1.0 - fy),
                6 => (fy, h as f32 - 1.0 - fx),
                5 => (w as f32 - 1.0 - fy, fx),
                _ => (fx, fy),
            };
            let col = bilinear(&lin, w, h, sx, sy);
            let mut adj = adjust(col, p);
            if p.grain > 0.0 {
                for (c, seed) in adj.iter_mut().zip([0.0f32, 17.0, 43.0]) {
                    *c += (hash_px(dx, dy, seed) - 0.5) * p.grain * 0.12;
                }
            }
            if p.vignette != 0.0 {
                let nx = (dx as f32 + 0.5) / dw as f32 * 2.0 - 1.0;
                let ny = (dy as f32 + 0.5) / dh as f32 * 2.0 - 1.0;
                let d = (nx * nx + ny * ny).sqrt() * 0.7071;
                let f = 1.0 - p.vignette * sstep(0.35, 1.05, d) * 0.9;
                for c in &mut adj {
                    *c *= f;
                }
            }
            let o = (dy * dw + dx) * 4;
            for c in 0..3 {
                out[o + c] = (srgb_encode(adj[c]) * 255.0 + 0.5) as u8;
            }
            out[o + 3] = 255;
        }
    }
    RgbaImage {
        width: dw as u32,
        height: dh as u32,
        data: out,
    }
}

fn bilinear(lin: &[[f32; 3]], w: usize, h: usize, sx: f32, sy: f32) -> [f32; 3] {
    let x0 = sx.floor() as i32;
    let y0 = sy.floor() as i32;
    if x0 < 0 || y0 < 0 || x0 + 1 >= w as i32 || y0 + 1 >= h as i32 {
        if x0 >= 0 && y0 >= 0 && (x0 as usize) < w && (y0 as usize) < h {
            return lin[y0 as usize * w + x0 as usize];
        }
        return [0.0; 3];
    }
    let tx = sx - x0 as f32;
    let ty = sy - y0 as f32;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let at = |x: usize, y: usize| lin[y * w + x];
    let mut o = [0.0f32; 3];
    for c in 0..3 {
        let a = at(x0, y0)[c] * (1.0 - tx) + at(x0 + 1, y0)[c] * tx;
        let b = at(x0, y0 + 1)[c] * (1.0 - tx) + at(x0 + 1, y0 + 1)[c] * tx;
        o[c] = a * (1.0 - ty) + b * ty;
    }
    o
}

fn hash_px(x: usize, y: usize, seed: f32) -> f32 {
    let v = (x as f32 * (12.9898 + seed) + y as f32 * (78.233 + seed * 1.7)).sin() * 43758.5453;
    v - v.floor()
}

/// 256-bin histogram of rendered rgba8 pixels: [R,G,B,luma] * 256.
pub fn histogram(rgba8: &[u8]) -> [u32; 1024] {
    let mut h = [0u32; 1024];
    for px in rgba8.chunks_exact(4) {
        h[px[0] as usize] += 1;
        h[256 + px[1] as usize] += 1;
        h[512 + px[2] as usize] += 1;
        let l = (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) as usize;
        h[768 + l.min(255)] += 1;
    }
    h
}

fn fit(w: u32, h: u32, max_px: u32) -> (u32, u32) {
    let s = max_px as f32 / w.max(h) as f32;
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

fn box3(chan: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let mut s = 0.0f32;
    let mut n = 0u32;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                s += chan[ny as usize * w + nx as usize];
                n += 1;
            }
        }
    }
    s / n as f32
}

/// simple chroma smoothing (colour NR): blur R-B residuals only.
fn chroma_smooth(lin: &[[f32; 3]], w: usize, h: usize, amount: f32) -> Vec<[f32; 3]> {
    let luma: Vec<f32> = lin
        .iter()
        .map(|c| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
        .collect();
    let mut cr = vec![0.0f32; lin.len()];
    let mut cb = vec![0.0f32; lin.len()];
    for (i, c) in lin.iter().enumerate() {
        cr[i] = c[0] - luma[i];
        cb[i] = c[2] - luma[i];
    }
    let mut out = Vec::with_capacity(lin.len());
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let br = box3(&cr, w, h, x, y);
            let bb = box3(&cb, w, h, x, y);
            let nr = cr[i] + (br - cr[i]) * amount;
            let nb = cb[i] + (bb - cb[i]) * amount;
            let l = luma[i];
            out.push([
                l + nr,
                l - 0.2126 / 0.7152 * nr - 0.0722 / 0.7152 * nb, // keep luma
                l + nb,
            ]);
        }
    }
    out
}

/// unsharp mask (3x3) + clarity (5x5 midtone-weighted local contrast).
fn sharpen_clarity(
    lin: &[[f32; 3]],
    w: usize,
    h: usize,
    sharpen: f32,
    clarity: f32,
) -> Vec<[f32; 3]> {
    let chans: Vec<Vec<f32>> = (0..3)
        .map(|c| lin.iter().map(|p| p[c]).collect())
        .collect();
    let boxn = |chan: &[f32], x: usize, y: usize, rad: i32| {
        let mut s = 0.0f32;
        let mut n = 0u32;
        for dy in -rad..=rad {
            for dx in -rad..=rad {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                    s += chan[ny as usize * w + nx as usize];
                    n += 1;
                }
            }
        }
        s / n as f32
    };
    let mut out = vec![[0.0f32; 3]; lin.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let lum = (0.2126 * lin[i][0] + 0.7152 * lin[i][1] + 0.0722 * lin[i][2])
                .clamp(0.0, 1.0);
            let mid = 4.0 * lum * (1.0 - lum);
            for c in 0..3 {
                let mut v = lin[i][c];
                if sharpen != 0.0 {
                    v += sharpen * 0.8 * (v - boxn(&chans[c], x, y, 1));
                }
                if clarity != 0.0 {
                    v += clarity * 0.6 * mid * (lin[i][c] - boxn(&chans[c], x, y, 2));
                }
                out[i][c] = v;
            }
        }
    }
    out
}
