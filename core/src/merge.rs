//! Merge ops on fully-rendered images: focus stack, exposure fusion
//! (Mertens), and translation alignment used by both.
//!
//! These operate on display-referred RGBA8 output of `Engine::export`, so
//! each source photo's own sidecar recipe is applied before merging.

use anyhow::{anyhow, Result};
use crate::RgbaImage;

fn luma(p: &[u8]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

fn luma_plane(img: &RgbaImage, w: u32, h: u32) -> Vec<f32> {
    let d = &img.data;
    (0..(w * h) as usize)
        .map(|i| luma(&d[i * 4..i * 4 + 4]))
        .collect()
}

/// downscale a luma plane by box average to (dw, dh)
fn down_luma(src: &[f32], w: usize, h: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0; dw * dh];
    for y in 0..dh {
        let y0 = y * h / dh;
        let y1 = ((y + 1) * h / dh).max(y0 + 1).min(h);
        for x in 0..dw {
            let x0 = x * w / dw;
            let x1 = ((x + 1) * w / dw).max(x0 + 1).min(w);
            let mut s = 0.0;
            let mut n = 0;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    s += src[yy * w + xx];
                    n += 1;
                }
            }
            out[y * dw + x] = s / n.max(1) as f32;
        }
    }
    out
}

/// translational alignment of `mov` onto `ref` by SAD search on a ~256px
/// luma grid, scaled back to full-res pixels
fn align_translate(ra: &RgbaImage, rb: &RgbaImage) -> (i32, i32) {
    let (w, h) = (ra.width, ra.height);
    let gw = 192usize;
    let gh = (h as usize * gw / w as usize).max(1);
    let a = down_luma(&luma_plane(ra, w, h), w as usize, h as usize, gw, gh);
    let b = down_luma(&luma_plane(rb, w, h), w as usize, h as usize, gw, gh);
    let r = 6i32; // search radius in grid px (~3% of width)
    let mut best = (0i32, 0i32);
    let mut best_score = f32::MAX;
    // skip a 10% border so shifted regions don't dominate the score
    let mx = gw as i32 / 10;
    let my = gh as i32 / 10;
    for dy in -r..=r {
        for dx in -r..=r {
            let mut s = 0.0f32;
            let mut n = 0;
            for y in my..(gh as i32 - my) {
                for x in mx..(gw as i32 - mx) {
                    let bx = x + dx;
                    let by = y + dy;
                    if bx < 0 || by < 0 || bx >= gw as i32 || by >= gh as i32 {
                        continue;
                    }
                    s += (a[(y * gw as i32 + x) as usize] - b[(by * gw as i32 + bx) as usize]).abs();
                    n += 1;
                }
            }
            if n > 0 {
                let sc = s / n as f32;
                if sc < best_score {
                    best_score = sc;
                    best = (dx, dy);
                }
            }
        }
    }
    (
        (best.0 as i64 * w as i64 / gw as i64) as i32,
        (best.1 as i64 * h as i64 / gh as i64) as i32,
    )
}

/// shift img by (dx,dy) pixels, clamping at edges
fn shift(img: &RgbaImage, dx: i32, dy: i32) -> RgbaImage {
    let (w, h) = (img.width as i64, img.height as i64);
    let mut out = RgbaImage {
        width: img.width,
        height: img.height,
        data: vec![0u8; (img.width * img.height) as usize * 4],
    };
    for y in 0..h {
        for x in 0..w {
            let sx = (x - dx as i64).clamp(0, w - 1);
            let sy = (y - dy as i64).clamp(0, h - 1);
            let si = ((sy * w + sx) * 4) as usize;
            let di = ((y * w + x) * 4) as usize;
            out.data[di..di + 4].copy_from_slice(&img.data[si..si + 4]);
        }
    }
    out
}

fn box_blur(src: &[f32], w: usize, h: usize, rad: usize) -> Vec<f32> {
    // two-pass separable box blur
    let mut tmp = vec![0.0f32; w * h];
    let mut out = vec![0.0f32; w * h];
    let n = (2 * rad + 1) as f32;
    for y in 0..h {
        let mut acc = 0.0;
        for x in 0..w {
            let xa = x.saturating_add(rad).min(w - 1);
            let xs = x.saturating_sub(rad + 1);
            if x == 0 {
                for i in 0..=xa {
                    acc += src[y * w + i];
                }
            } else {
                acc += src[y * w + xa];
                if x > rad {
                    acc -= src[y * w + xs.min(w - 1)];
                }
            }
            tmp[y * w + x] = acc / n;
        }
    }
    for x in 0..w {
        let mut acc = 0.0;
        for y in 0..h {
            let ya = y.saturating_add(rad).min(h - 1);
            let ys = y.saturating_sub(rad + 1);
            if y == 0 {
                for i in 0..=ya {
                    acc += tmp[i * w + x];
                }
            } else {
                acc += tmp[ya * w + x];
                if y > rad {
                    acc -= tmp[ys.min(h - 1) * w + x];
                }
            }
            out[y * w + x] = acc / n;
        }
    }
    out
}

/// focus stack: per-pixel sharpest-source selection smoothed into a blend.
/// `imgs` must all share dimensions.
pub fn focus_stack(imgs: &[RgbaImage]) -> Result<RgbaImage> {
    if imgs.is_empty() {
        return Err(anyhow!("focus_stack: no images"));
    }
    let (w, h) = (imgs[0].width as usize, imgs[0].height as usize);
    for im in imgs {
        if im.width as usize != w || im.height as usize != h {
            return Err(anyhow!("focus_stack: size mismatch"));
        }
    }
    let n = imgs.len();
    if n == 1 {
        return Ok(imgs[0].clone());
    }
    // focus measure: |laplacian| of luma, blurred so regions vote together
    let mut weights: Vec<Vec<f32>> = Vec::with_capacity(n);
    for im in imgs {
        let l = luma_plane(im, w as u32, h as u32);
        let mut e = vec![0.0f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let c = l[y * w + x];
                let lap = 4.0 * c - l[y * w + x - 1] - l[y * w + x + 1]
                    - l[(y - 1) * w + x]
                    - l[(y + 1) * w + x];
                e[y * w + x] = lap.abs();
            }
        }
        weights.push(box_blur(&e, w, h, 6));
    }
    let mut out = RgbaImage { width: w as u32, height: h as u32, data: vec![0u8; w * h * 4] };
    let od = &mut out.data;
    for i in 0..w * h {
        let mut sum = 0.0f32;
        let mut wsum = [0.0f32; 3];
        for (k, wt) in weights.iter().enumerate() {
            let wv = wt[i].max(1e-6);
            let pd = &imgs[k].data;
            for c in 0..3 {
                wsum[c] += wv * pd[i * 4 + c] as f32;
            }
            sum += wv;
        }
        for c in 0..3 {
            od[i * 4 + c] = (wsum[c] / sum).round().clamp(0.0, 255.0) as u8;
        }
        od[i * 4 + 3] = 255;
    }
    Ok(out)
}

fn gaussian_down(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    // 5-tap [1 4 6 4 1]/16 blur then 2x decimate
    const K: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for k in 0..5 {
                let xx = (x as i32 + k as i32 - 2).clamp(0, w as i32 - 1) as usize;
                s += K[k] * src[y * w + xx];
            }
            tmp[y * w + x] = s / 16.0;
        }
    }
    let (dw, dh) = ((w + 1) / 2, (h + 1) / 2);
    let mut out = vec![0.0f32; dw * dh];
    for y in 0..dh {
        let sy = (2 * y).min(h - 1);
        for x in 0..dw {
            let sx = (2 * x).min(w - 1);
            let mut s = 0.0;
            for k in 0..5 {
                let yy = (sy as i32 + k as i32 - 2).clamp(0, h as i32 - 1) as usize;
                s += K[k] * tmp[yy * w + sx];
            }
            out[y * dw + x] = s / 16.0;
        }
    }
    (out, dw, dh)
}

fn upsample_to(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * dh];
    for y in 0..dh {
        let fy = y as f32 * (sh as f32 - 1.0) / (dh as f32 - 1.0).max(1.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let ty = fy - y0 as f32;
        for x in 0..dw {
            let fx = x as f32 * (sw as f32 - 1.0) / (dw as f32 - 1.0).max(1.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(sw - 1);
            let tx = fx - x0 as f32;
            let a = src[y0 * sw + x0] * (1.0 - tx) + src[y0 * sw + x1] * tx;
            let b = src[y1 * sw + x0] * (1.0 - tx) + src[y1 * sw + x1] * tx;
            out[y * dw + x] = a * (1.0 - ty) + b * ty;
        }
    }
    out
}

/// Mertens exposure fusion: per-source weights (contrast × saturation ×
/// well-exposedness) merged through a Laplacian pyramid so each source
/// contributes its best-exposed detail without a hard HDR radiance solve.
pub fn exposure_fuse(imgs: &[RgbaImage]) -> Result<RgbaImage> {
    if imgs.is_empty() {
        return Err(anyhow!("exposure_fuse: no images"));
    }
    let (w, h) = (imgs[0].width as usize, imgs[0].height as usize);
    for im in imgs {
        if im.width as usize != w || im.height as usize != h {
            return Err(anyhow!("exposure_fuse: size mismatch"));
        }
    }
    if imgs.len() == 1 {
        return Ok(imgs[0].clone());
    }
    let npx = w * h;
    // per-source weights
    let mut wmaps: Vec<Vec<f32>> = Vec::with_capacity(imgs.len());
    for im in imgs {
        let d = &im.data;
        let l = luma_plane(im, w as u32, h as u32);
        let mut wm = vec![0.0f32; npx];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                // contrast: |laplacian| at the pixel
                let lap = if x > 0 && x < w - 1 && y > 0 && y < h - 1 {
                    (4.0 * l[i] - l[i - 1] - l[i + 1] - l[i - w] - l[i + w]).abs() / 255.0
                } else {
                    0.0
                };
                // saturation: channel std-dev
                let (r, g, b) = (
                    d[i * 4] as f32 / 255.0,
                    d[i * 4 + 1] as f32 / 255.0,
                    d[i * 4 + 2] as f32 / 255.0,
                );
                let mu = (r + g + b) / 3.0;
                let sat =
                    ((r - mu) * (r - mu) + (g - mu) * (g - mu) + (b - mu) * (b - mu)).sqrt() / 3.0;
                // well-exposedness: gaussian around 0.5, sigma 0.2
                let ex = (-((r - 0.5) * (r - 0.5)) / (2.0 * 0.04)).exp()
                    * (-((g - 0.5) * (g - 0.5)) / (2.0 * 0.04)).exp()
                    * (-((b - 0.5) * (b - 0.5)) / (2.0 * 0.04)).exp();
                wm[i] = (lap * sat * ex + 1e-12).max(0.0);
            }
        }
        wmaps.push(wm);
    }
    // normalize weights per pixel
    for i in 0..npx {
        let mut s = 0.0f32;
        for wm in &wmaps {
            s += wm[i];
        }
        let s = s.max(1e-12);
        for wm in &mut wmaps {
            wm[i] /= s;
        }
    }
    let levels = (((w.min(h)) as f32).log2() as usize).saturating_sub(2).clamp(3, 9);
    // per-channel fused laplacian pyramid
    let mut acc: Vec<Vec<f32>> = Vec::new();
    let mut acc_dims: Vec<(usize, usize)> = Vec::new();
    for c in 0..3 {
        // build fused pyramid level by level
        let mut level_imgs: Vec<Vec<f32>> = imgs
            .iter()
            .map(|im| {
                im.data
                    .chunks_exact(4)
                    .map(|p| p[c] as f32 / 255.0)
                    .collect()
            })
            .collect();
        let mut level_w: Vec<Vec<f32>> = wmaps.clone();
        let (mut lw, mut lh) = (w, h);
        let mut fused: Vec<(Vec<f32>, usize, usize)> = Vec::new();
        for lev in 0..levels {
            // laplacian of each source at this level, then weighted merge
            let (dw2, dh2) = ((lw + 1) / 2, (lh + 1) / 2);
            let mut lsum = vec![0.0f32; lw * lh];
            for k in 0..imgs.len() {
                let (lkd, lkw, lkh) = gaussian_down(&level_imgs[k], lw, lh);
                let lup = upsample_to(&lkd, lkw, lkh, lw, lh);
                for i in 0..lw * lh {
                    lsum[i] += level_w[k][i] * (level_imgs[k][i] - lup[i]);
                }
            }
            fused.push((lsum, lw, lh));
            level_imgs = (0..imgs.len())
                .map(|k| {
                    let (d, dw2, dh2) = gaussian_down(&level_imgs[k], lw, lh);
                    let _ = (dw2, dh2);
                    d
                })
                .collect();
            level_w = wmaps
                .iter()
                .map(|wm| gaussian_down(wm, lw, lh).0)
                .collect();
            // renormalize weights at coarser level
            for i in 0..dw2 * dh2 {
                let mut s = 0.0f32;
                for wm in &level_w {
                    s += wm[i];
                }
                let s = s.max(1e-12);
                for wm in &mut level_w {
                    wm[i] /= s;
                }
            }
            // the last fused level keeps the weighted-average base
            if lev == levels - 1 {
                let mut base = vec![0.0f32; dw2 * dh2];
                for k in 0..imgs.len() {
                    for i in 0..dw2 * dh2 {
                        base[i] += level_w[k][i] * level_imgs[k][i];
                    }
                }
                fused.push((base, dw2, dh2));
            }
            lw = dw2;
            lh = dh2;
        }
        // collapse pyramid
        let (mut cur, mut cw, mut ch) = fused.pop().unwrap();
        for (l, lw2, lh2) in fused.iter().rev() {
            let up = upsample_to(&cur, cw, ch, *lw2, *lh2);
            let mut nx = vec![0.0f32; lw2 * lh2];
            for i in 0..lw2 * lh2 {
                nx[i] = l[i] + up[i];
            }
            cur = nx;
            cw = *lw2;
            ch = *lh2;
        }
        acc.push(cur);
        acc_dims.push((cw, ch));
    }
    let mut out = RgbaImage { width: w as u32, height: h as u32, data: vec![0u8; w * h * 4] };
    let od = &mut out.data;
    for i in 0..npx {
        for c in 0..3 {
            od[i * 4 + c] = (acc[c][i].clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        od[i * 4 + 3] = 255;
    }
    Ok(out)
}

/// merge a set of rendered images: "hdr" (exposure fusion) or "focus".
/// Sources are translation-aligned onto the first before merging.
pub fn merge(imgs: &[RgbaImage], mode: &str) -> Result<RgbaImage> {
    if imgs.len() < 2 {
        return Err(anyhow!("merge: need at least 2 images"));
    }
    let base = &imgs[0];
    let mut aligned: Vec<RgbaImage> = Vec::with_capacity(imgs.len());
    aligned.push(base.clone());
    for im in &imgs[1..] {
        if im.width != base.width || im.height != base.height {
            return Err(anyhow!("merge: source sizes differ — same-camera bursts only"));
        }
        let (dx, dy) = align_translate(base, im);
        aligned.push(shift(im, dx, dy));
    }
    match mode {
        "hdr" | "fuse" => exposure_fuse(&aligned),
        "focus" | "stack" => focus_stack(&aligned),
        other => Err(anyhow!("merge: unknown mode '{other}'")),
    }
}
