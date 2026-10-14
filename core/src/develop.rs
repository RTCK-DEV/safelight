//! Development pipeline (CPU reference implementation).
//! Stages: normalize -> demosaic -> WB -> camera->sRGB matrix ->
//!         tone/color adjustments -> sRGB gamma -> orient.
//! The same math is mirrored by the wgpu compute pipeline in gpu.rs.
use crate::decode::{CfaPattern, Decoded, Mosaic};
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
    pub sharp_mask: f32,
    /// box-blur radius for the unsharp base 1..3 (interp between boxes)
    pub sharp_radius: f32,
    /// 0 = coarse-band sharpening, 1 = fine-band (LR Detail/100)
    pub sharp_detail: f32,
    pub noise_luma: f32,
    pub rotation_deg: f32,
    pub clarity: f32,
    /// LR Texture: fine-scale midtone detail contrast, -1..1
    pub texture: f32,
    pub vignette: f32,
    /// post-crop vignette shape [mid, round, feather]; mid<0 = legacy ramp
    pub vig_shape: [f32; 3],
    /// grain cell size in px (1 = per-pixel white noise)
    pub grain_size: f32,
    /// 0 = pure white noise, 1 = heavy low-frequency clumping
    pub grain_rough: f32,
    pub grain: f32,
    // grading
    pub lift: [f32; 3],
    pub gamma: [f32; 3],
    pub gain: [f32; 3],
    pub shadow_col: [f32; 3],
    pub shadow_sat: f32,
    pub high_col: [f32; 3],
    pub high_sat: f32,
    /// LR SplitToningBalance -1..1 (shadow↔highlight crossover shift)
    pub grade_balance: f32,
    /// LR ColorGradeBlending 0..1 — range overlap width (0.5 = LR 50)
    pub grade_blend: f32,
    /// auto-contrast remap points (identity when 0,1)
    pub black_pt: f32,
    pub white_pt: f32,
    // retouch (frame-normalized coords, converted to virtual-px inside)
    pub spots: [[f32; 4]; 8],
    pub n_spots: u32,
    pub lights: [[f32; 4]; 8],
    pub n_lights: u32,
    pub crop: [f32; 4],
    // DaVinci extras
    pub offset: [f32; 3],
    pub mid_col: [f32; 3],
    pub mid_sat: f32,
    pub pivot: f32,
    pub hl_roll: f32,
    pub sh_roll: f32,
    /// per-channel curve LUTs, 768 floats (r|g|b); empty = identity
    pub chan_luts: Vec<f32>,
    /// hue-domain curve LUTs, 1280 floats (hh|hs|hl|ls|ss); empty = identity
    pub hue_luts: Vec<f32>,
    pub qh: [f32; 3],
    pub qs: [f32; 3],
    pub ql: [f32; 3],
    pub qadj: [f32; 4],
    pub q_invert: bool,
    /// matte finesse [clean_black, clean_white]
    pub q_clean: [f32; 2],
    /// matte finesse blur → widens soft edges
    pub q_blur: f32,
    /// highlight/isolate preview of the key
    pub q_show: bool,
    pub has_qual: bool,
    /// power windows: packed [kind(+2=invert), p0,p1,p2,p3, p4(rot), p5(soft), ev, sat, temp, strength]
    pub wins: [[f32; 24]; 16],
    /// per-window LR range refinement: [mode, kind, p0..p3]; mode 0=none,
    /// 1=subtract, 2=intersect; kind 1=color(h,s,tol,inv), 2=lum(lo,hi,f,inv)
    pub refines: [[f32; 6]; 16],
    /// per-window LocalPointColors: [0]=count (≤2), swatch s at 1+s*20:
    /// [src3, shift3, amt, hr4, sr4, lr4, variance]
    pub lpcs: [[f32; 41]; 16],
    pub n_wins: u32,
    /// adjustment brushes: flattened segments [x1,y1,x2,y2] in
    /// dst-normalized coords, then per-stroke records of 3 entries:
    /// [radius, soft, signed_opacity(+paint/-erase), layer],
    /// [seg_start, seg_count, 0, 0],
    /// [xmin, ymin, xmax, ymax] aspect-space bbox (padded by radius).
    /// Layer params (ev,sat,temp,strength) in brush_layers;
    /// (contrast,clarity,dehaze,tint,hi,sh,wh,bl,texture) in brush_layers2.
    /// brush_misc = [n_strokes, dst_aspect, link_q_bits, 0].
    /// brush_seeds = per-stroke edge-aware seed colour [r,g,b,tol],
    /// parallel to strokes (tol>0 = enabled; rgb filled at render time
    /// from the linear image at the stroke's first dab).
    pub brush_segs: Vec<[f32; 4]>,
    pub brush_strokes: Vec<[f32; 4]>,
    pub brush_layers: [[f32; 4]; 4],
    pub brush_layers2: [[f32; 12]; 4],
    pub brush_misc: [f32; 4],
    pub brush_seeds: Vec<[f32; 4]>,
    /// HDR zone wheels [hue, amt, ev, sat] for dark/shadow/light/global
    pub zones: [[f32; 4]; 4],
    pub mixer: [f32; 9],
    pub mono: [f32; 3],
    /// clone stamps in virtual-px [sx,sy,dx,dy,r,_] (max 8)
    pub clones: [[f32; 6]; 8],
    pub n_clones: u32,
    pub beauty: f32,
    pub noise_chroma: f32,
    /// dehaze veil strength (recipe dehaze * 0.85) — 0 = off
    pub dehaze: f32,
    /// atmospheric light estimate A (min-channel p99.5, >= 0.05)
    pub dehaze_a: f32,
    pub ca_fix: f32,
    /// measured lateral-CA radial scale for R and B (auto mode; filled in
    /// by the pipeline, not from the recipe)
    pub ca_r: f32,
    pub ca_b: f32,
    pub deband: f32,
    pub glow: f32,
    /// lens flare [cx, cy, strength, hue] in dst-normalized coords
    pub flare: [f32; 4],
    /// LR defringe: [pu_amount, pu_lo_deg, pu_hi_deg, 0, gr_amount,
    /// gr_lo_deg, gr_hi_deg, 0]
    pub defringe: [f32; 8],
    /// tone equalizer: EV per log2-luma zone (centers -4..+4 EV, 9 zones)
    pub zone_ev: [f32; 9],
    /// imported .cube 3D LUT (applied display-referred) + amount
    pub lut3d: Option<crate::lut::CubeLut>,
    pub lut_amt: f32,
    /// keystone trapezoid warps -0.4..0.4
    pub key_v: f32,
    pub key_h: f32,
    /// lens profile correction amount (baked into virtual sampling)
    pub lens_corr_amt: f32,
    /// EV-domain tone LUT (512 entries over linear y in 0..1.6): folds
    /// shadows/highlights/whites/blacks + rolloff into one luminance-preserving
    /// curve with an extended-Reinhard shoulder and soft-knee toe.
    pub tone_lut: [f32; 512],
    /// split tone LUTs for the spatial (masked) model: shadow-side sliders
    /// only / highlight-side sliders only — the regional deviation of each
    /// is exponentiated separately.
    pub tone_sh: [f32; 512],
    pub tone_hl: [f32; 512],
    pub has_tone: bool,
    /// serial correction stages (DaVinci serial nodes): (params, opacity,
    /// invert_key) per enabled stage, max 4. Each stage re-runs adjust() +
    /// local ops on the sampled colour, then blends by its own key
    /// (qualifier × window union) × opacity.
    pub stages: Vec<(Params, f32, bool)>,
    /// ColorSlice: 7 fixed hue wedges (R/Skin/Y/G/C/B/M), each
    /// [hue_shift, sat_delta, lum_delta, enabled]
    pub cs: [[f32; 4]; 7],
    pub has_cs: bool,
    /// ColorWarper control points [h, s, dh, ds, radius], max 8 honoured
    pub warper: Vec<[f32; 5]>,
    /// LR Point Color swatches (see Recipe::point_colors for layout)
    pub point_colors: Vec<[f32; 19]>,
    /// per-swatch Color Variance (-1..1), index-aligned with point_colors
    pub point_color_vars: Vec<f32>,
    // ---- .dcp camera profile (Adobe colour pipeline) ----
    /// hue/sat/val shift table applied in WB-scaled camera space,
    /// BEFORE the cam->XYZ matrix (DNG ProfileHueSatMap)
    pub profile_hsm: Option<crate::dcp::HsMap>,
    /// same table shape applied to the working-space HSV
    /// (DNG ProfileLookTable) — the profile's "look"
    pub profile_look: Option<crate::dcp::HsMap>,
    /// profile tone curve as a 256-entry LUT on each channel 0..1,
    /// applied at the start of adjust() (empty = identity)
    pub profile_tone_lut: Vec<f32>,
    /// working-space exit transform applied after the profile ops
    /// (ProPhoto -> sRGB when a DCP resolved, else identity)
    pub post_profile: [[f32; 3]; 3],
    /// LR camera calibration: 3x3 chroma-rotation matrix (row-major 9)
    /// + shadow green/magenta tint [9..12]
    pub calib: [f32; 12],
    /// real AI-mask bitmaps extracted from the catalog `.lrcat-data` blob
    /// store: index-aligned with win flag 1024 — win[21] is the slot here.
    /// Each carries the affine from post-crop normalized (nx,ny) into the
    /// bitmap's (u,v): u = nx*ax + bx.
    pub wmats: Vec<WMatte>,
    /// LR PatchMatch remove spots: per-target-px absolute source coords
    /// decoded from the catalog patchmap, sensor/lin px space.
    pub pms: Vec<PmMap>,
    /// frame the pm coords were authored in [w, h] — the raw's sensor dims
    /// (or raster dims); `lin` may be a subsampled virtual frame, so the pm
    /// pass scales coords by lin_w/pm_frame[0]. 0 = identity.
    pub pm_frame: [f32; 2],
    /// LR Lens Blur amount 0..1 — defocus blend gated by (1-subject matte)
    pub lens_blur: f32,
    /// [highlight_boost, highlight_thresh, aspect, shape, spherical_ab]
    pub lens_blur_opts: [f32; 5],
    /// [cat_eye, cat_eye_scale, rotation, shape_detail]
    pub lens_blur_opts2: [f32; 4],
}

/// statistics gathered by the sparse sampling pass (auto WB / exposure / contrast)
#[derive(Debug, Clone)]
pub struct Stats {
    pub means: [f32; 3],
    pub luma_mean: f32,
    pub luma_hist: [u32; 256],
    /// 256-bin histogram of min(r,g,b) — dark-channel prior for dehaze
    pub min_hist: [u32; 256],
    pub count: u64,
    /// mean raw level inside the WB-pick rectangle (camera space)
    pub pick_means: [f32; 3],
    pub pick_count: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            means: [0.0; 3],
            luma_mean: 0.0,
            luma_hist: [0; 256],
            min_hist: [0; 256],
            count: 0,
            pick_means: [0.0; 3],
            pick_count: 0,
        }
    }
}

impl Stats {
    /// 256-bin luma histogram (linear 0..1 domain)
    pub fn needs(r: &Recipe) -> bool {
        r.wb_mode == WbMode::Auto
            || r.wb_mode == WbMode::Pick
            || r.auto_exposure
            || r.auto_contrast
            || r.dehaze > 0.0
    }
    fn luma_percentile(&self, p: f32) -> f32 {
        let target = (self.count as f64 * p as f64) as u64;
        let mut acc = 0u64;
        for (i, &b) in self.luma_hist.iter().enumerate() {
            acc += b as u64;
            if acc >= target.max(1) {
                return i as f32 / 255.0;
            }
        }
        1.0
    }
    /// dark-channel percentile: atmospheric light estimate for dehaze
    fn dark_percentile(&self, p: f32) -> f32 {
        let target = (self.count as f64 * p as f64) as u64;
        let mut acc = 0u64;
        for (i, &b) in self.min_hist.iter().enumerate() {
            acc += b as u64;
            if acc >= target.max(1) {
                return i as f32 / 255.0;
            }
        }
        1.0
    }
}

/// base display curve applied when the recipe has no explicit point curve —
/// Adobe's renderer likewise never outputs raw scene-linear; this is the
/// ACR "Medium Contrast" preset shape, a mild filmic S. `linear_output`
/// or an explicit `curve` replaces it entirely.
const BASE_TONE_CURVE: &[[f32; 2]] = &[
    [0.0, 0.0],
    [0.125, 0.086],
    [0.25, 0.22],
    [0.5, 0.5],
    [0.75, 0.78],
    [0.878, 0.902],
    [1.0, 1.0],
];

pub(crate) fn catmull_lut(points: &[[f32; 2]]) -> Vec<f32> {
    curve_lut(points, |x| x, 1.0)
}

/// catmull-rom LUT with caller-chosen endpoints/default and output ceiling.
/// `default` supplies the endpoint values (and the whole curve when `points`
/// is empty), so identity/gain/remap curves share this path.
fn curve_lut(points: &[[f32; 2]], default: impl Fn(f32) -> f32, maxy: f32) -> Vec<f32> {
    // no user points: the curve is exactly the default mapping. (Degenerate
    // endpoint-only Catmull-Rom is NOT the identity — it bows ~20% dark at
    // x=0.25 — so the empty case must not go through the spline.)
    if points.is_empty() {
        return (0..256)
            .map(|i| default(i as f32 / 255.0).clamp(0.0, maxy))
            .collect();
    }
    // build a sorted point list incl. endpoints
    let mut pts: Vec<[f32; 2]> = Vec::new();
    pts.push([0.0, default(0.0)]);
    let mut mid: Vec<[f32; 2]> = points
        .iter()
        .map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)])
        .collect();
    mid.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
    mid.retain(|p| p[0] > 0.0 && p[0] < 1.0);
    if mid.is_empty() {
        // endpoint-only curve (e.g. LR "Linear" = [[0,0],[1,1]]): the
        // Catmull-Rom endpoint clamp would bow it ~20% dark — a two-point
        // ramp IS the default mapping, emit it directly.
        return (0..256)
            .map(|i| default(i as f32 / 255.0).clamp(0.0, maxy))
            .collect();
    }
    pts.extend(mid);
    pts.push([1.0, default(1.0)]);
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    if pts.len() < 2 {
        return (0..256)
            .map(|i| default(i as f32 / 255.0).clamp(0.0, maxy))
            .collect();
    }
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
        lut.push(y.clamp(0.0, maxy));
    }
    lut
}

/// linear-interp sample of a packed 256-entry lut
fn lut_at(lut: &[f32], x: f32) -> f32 {
    let idx = (x.clamp(0.0, 1.0) * 255.0) as usize;
    lut[idx.min(255)]
}

/// linear-interpolated 512-entry tone LUT — mirrors gpu.rs `tone_at`
fn tone_lut_at(lut: &[f32; 512], x: f32) -> f32 {
    let f = x.clamp(0.0, 1.0) * 511.0;
    let i0 = (f as usize).min(510);
    lut[i0] + (lut[i0 + 1] - lut[i0]) * (f - i0 as f32)
}

/// rgb -> (hue 0..1, sat, v=max) — mirrored in the WGSL adjust
fn rgb_to_hsv(x: [f32; 3]) -> (f32, f32, f32) {
    let mx = x[0].max(x[1]).max(x[2]);
    let mn = x[0].min(x[1]).min(x[2]);
    let d = mx - mn;
    let s = if mx > 1e-6 { d / mx } else { 0.0 };
    let h = if d < 1e-6 {
        0.0
    } else if mx == x[0] {
        (x[1] - x[2]) / d / 6.0
    } else if mx == x[1] {
        (2.0 + (x[2] - x[0]) / d) / 6.0
    } else {
        (4.0 + (x[0] - x[1]) / d) / 6.0
    };
    (h - h.floor(), s, mx)
}

/// (hue 0..1, sat, v) -> rgb — standard HSV sector interpolation
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h6 = (h - h.floor()) * 6.0;
    let i = h6 as u32 % 6;
    let f = h6 - h6.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match i {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// cyclic distance between two hues in 0..1
fn hue_dist(a: f32, b: f32) -> f32 {
    let d = (a - b).abs() - (a - b).abs().floor();
    d.min(1.0 - d)
}

/// skin-tone proximity for the beauty/smoothing mask
fn skin_mask(x: [f32; 3]) -> f32 {
    let (h, s, v) = rgb_to_hsv(x);
    let hw = 1.0 - sstep(0.02, 0.12, hue_dist(h, 0.075));
    let sw = sstep(0.05, 0.25, s);
    let lw = sstep(0.15, 0.35, v);
    hw * sw * lw
}

/// hue (0..1, 0=red) -> rgb for split-toning targets
fn hue_to_rgb(h: f32) -> [f32; 3] {
    let h = (h - h.floor()) * 6.0;
    let i = h as usize % 6;
    let f = h - h.floor();
    let seg = [
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0],
    ];
    let a = seg[i];
    let b = seg[(i + 1) % 6];
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// cinematic presets: additive tweaks on top of the user's params.
fn apply_look(p: &mut Params, look: &str) {
    match look {
        "teal_orange" => {
            p.shadow_col = hue_to_rgb(0.52);
            p.shadow_sat = (p.shadow_sat + 0.30).min(1.0);
            p.high_col = hue_to_rgb(0.08);
            p.high_sat = (p.high_sat + 0.25).min(1.0);
            p.contrast += 0.12;
        }
        "film_fade" => {
            for c in &mut p.lift {
                *c += 0.05;
            }
            p.contrast -= 0.18;
            p.saturation -= 0.20;
        }
        "bleach" => {
            p.saturation -= 0.45;
            p.contrast += 0.22;
            p.highlights += 0.35;
            for c in &mut p.lift {
                *c += 0.015;
            }
        }
        "noir" => {
            p.saturation = -1.0;
            p.contrast += 0.30;
            for c in &mut p.lift {
                *c += 0.01;
            }
        }
        "matte" => {
            for c in &mut p.lift {
                *c += 0.035;
            }
            p.contrast -= 0.22;
            p.saturation -= 0.12;
            p.high_col = hue_to_rgb(0.10);
            p.high_sat = (p.high_sat + 0.15).min(1.0);
        }
        // ---- camera-matching profiles (Lightroom "Camera Matching"
        // equivalents — approximated renderings, not vendor data) ----
        // Fujifilm film simulations
        "fuji_provia" => {
            p.saturation += 0.03;
            p.contrast += 0.03;
        }
        "fuji_velvia" => {
            p.saturation += 0.35;
            p.contrast += 0.15;
            p.high_col = hue_to_rgb(0.08);
            p.high_sat = (p.high_sat + 0.10).min(1.0);
            p.shadow_col = hue_to_rgb(0.55);
            p.shadow_sat = (p.shadow_sat + 0.08).min(1.0);
        }
        "fuji_astia" => {
            p.contrast -= 0.12;
            p.saturation += 0.06;
            p.high_col = hue_to_rgb(0.09);
            p.high_sat = (p.high_sat + 0.08).min(1.0);
            for c in &mut p.lift {
                *c += 0.01;
            }
        }
        "fuji_chrome" => {
            // Classic Chrome: muted editorial — desaturated, warm
            // shadows, slightly cyan highlights
            p.saturation -= 0.30;
            p.contrast += 0.08;
            p.shadow_col = hue_to_rgb(0.07);
            p.shadow_sat = (p.shadow_sat + 0.12).min(1.0);
            p.high_col = hue_to_rgb(0.52);
            p.high_sat = (p.high_sat + 0.06).min(1.0);
            for c in &mut p.lift {
                *c += 0.015;
            }
        }
        "fuji_acros" => {
            p.saturation = -1.0;
            p.contrast += 0.22;
            for c in &mut p.lift {
                *c += 0.008;
            }
        }
        // Canon Picture Styles
        "canon_faithful" => {
            p.saturation -= 0.08;
            p.contrast -= 0.02;
        }
        "canon_landscape" => {
            p.saturation += 0.22;
            p.contrast += 0.10;
            p.shadow_col = hue_to_rgb(0.38);
            p.shadow_sat = (p.shadow_sat + 0.10).min(1.0);
        }
        "canon_portrait" => {
            p.contrast -= 0.12;
            p.saturation -= 0.05;
            p.high_col = hue_to_rgb(0.09);
            p.high_sat = (p.high_sat + 0.06).min(1.0);
        }
        "canon_mono" => {
            p.saturation = -1.0;
            p.contrast += 0.15;
        }
        // Nikon Picture Controls
        "nikon_vivid" => {
            p.saturation += 0.30;
            p.contrast += 0.18;
        }
        "nikon_neutral" => {
            p.contrast -= 0.08;
            p.saturation -= 0.05;
        }
        "nikon_portrait" => {
            p.contrast -= 0.10;
            p.saturation -= 0.04;
        }
        "nikon_mono" => {
            p.saturation = -1.0;
            p.contrast += 0.12;
        }
        // Sony Creative Styles
        "sony_vivid" => {
            p.saturation += 0.25;
            p.contrast += 0.15;
        }
        "sony_neutral" => {
            p.contrast -= 0.06;
            p.saturation -= 0.04;
        }
        "sony_portrait" => {
            p.contrast -= 0.10;
            p.saturation -= 0.03;
        }
        "sony_bw" => {
            p.saturation = -1.0;
            p.contrast += 0.14;
        }
        // Olympus Picture Modes
        "oly_vivid" => {
            p.saturation += 0.28;
            p.contrast += 0.12;
        }
        "oly_muted" => {
            p.saturation -= 0.25;
            p.contrast -= 0.05;
        }
        "oly_mono" => {
            p.saturation = -1.0;
            p.contrast += 0.15;
        }
        // Panasonic Photo Styles
        "pana_vivid" => {
            p.saturation += 0.25;
            p.contrast += 0.12;
        }
        "pana_natural" => {
            p.saturation -= 0.08;
            p.contrast -= 0.05;
        }
        "pana_mono" => {
            p.saturation = -1.0;
            p.contrast += 0.13;
        }
        // Leica film styles
        "leica_vivid" => {
            p.saturation += 0.20;
            p.contrast += 0.12;
        }
        "leica_natural" => {
            p.saturation -= 0.05;
            p.contrast -= 0.03;
        }
        "leica_bw" => {
            p.saturation = -1.0;
            p.contrast += 0.18;
        }
        _ => {}
    }
}

/// resolve + parse the recipe's .dcp profile. Results are cached by path.
/// `recipe.profile` is either an absolute/relative .dcp path, or a profile
/// name ("Adobe Standard", "Camera Standard", ...) matched against
/// `m.info.make`/`model` via crate::dcp::find_profile.
fn load_dcp_profile(r: &Recipe, m: &Mosaic) -> Option<std::sync::Arc<crate::dcp::DcpProfile>> {
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<
        Mutex<std::collections::HashMap<String, Option<Arc<crate::dcp::DcpProfile>>>>,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));

    let name = r.profile.trim();
    let name = if name.is_empty() {
        // named looks upgrade to the real .dcp when the file is present:
        // "adobe_standard" -> "Adobe Standard", "camera_vivid" -> "Camera Vivid".
        match r.look.as_str() {
            "adobe_standard" => "Adobe Standard",
            "camera_standard" | "camera_neutral" | "camera_vivid" | "camera_portrait"
            | "camera_landscape" | "camera_faithful" => {
                let mut s = String::from("Camera ");
                s.push_str(&r.look[7..8].to_uppercase());
                s.push_str(&r.look[8..]);
                // borrow dance: return owned below
                let key = format!("{}|{}|{}", s, m.info.make, m.info.model);
                return resolve_dcp(cache, &key, &s, m);
            }
            _ => return None,
        }
    } else {
        name
    };
    let key = format!("{}|{}|{}", name, m.info.make, m.info.model);
    resolve_dcp(cache, &key, name, m)
}

fn resolve_dcp(
    cache: &std::sync::Mutex<
        std::collections::HashMap<String, Option<std::sync::Arc<crate::dcp::DcpProfile>>>,
    >,
    key: &str,
    name: &str,
    m: &Mosaic,
) -> Option<std::sync::Arc<crate::dcp::DcpProfile>> {
    use std::sync::Arc;
    if let Some(hit) = cache.lock().ok()?.get(key) {
        return hit.clone();
    }
    let path = {
        let p = std::path::Path::new(name);
        if p.is_file() {
            Some(p.to_path_buf())
        } else {
            crate::dcp::find_profile(&m.info.make, &m.info.model, name)
        }
    };
    let prof = path
        .and_then(|p| crate::dcp::DcpProfile::load(&p))
        .map(Arc::new);
    if std::env::var_os("SL_DEBUG").is_some() {
        match &prof {
            Some(pr) => eprintln!(
                "[dcp] '{}' loaded for {} {}: cm1={} cm2={} fwd={} hsm={} look={} tone={} be={:.2}",
                pr.name,
                m.info.make,
                m.info.model,
                pr.color_matrix1.is_some(),
                pr.color_matrix2.is_some(),
                pr.forward_matrix1.is_some() || pr.forward_matrix2.is_some(),
                pr.hsm1.is_some() || pr.hsm2.is_some(),
                pr.look_table.is_some(),
                pr.tone_curve.len(),
                pr.baseline_exposure
            ),
            None => eprintln!(
                "[dcp] profile '{}' not found for {} {}",
                name, m.info.make, m.info.model
            ),
        }
    }
    if let Ok(mut c) = cache.lock() {
        if c.len() > 64 {
            c.clear();
        }
        c.insert(key.to_string(), prof.clone());
    }
    prof
}

pub fn build_params(m: &Mosaic, r: &Recipe, stats: Option<&Stats>) -> Params {
    // as-shot WB: prefer libraw's effective pre_mul (dcraw's actual channel
    // scaling), normalized to green. Fall back to cam_mul when absent.
    let mut wb = {
        let pm = m.pre_mul;
        if pm[1] > 0.0 && pm[0] > 0.0 && pm[2] > 0.0 {
            [pm[0] / pm[1], 1.0, pm[2] / pm[1]]
        } else {
            let g = if m.cam_mul[1] > 0.0 {
                m.cam_mul[1]
            } else {
                1.0
            };
            [
                m.cam_mul[0] / g,
                1.0,
                if m.cam_mul[2] > 0.0 {
                    m.cam_mul[2] / g
                } else {
                    1.0
                },
            ]
        }
    };
    if r.wb_mode == WbMode::Auto {
        if let Some(s) = stats {
            let gm = s.means[1].max(1e-4);
            wb = [gm / s.means[0].max(1e-4), 1.0, gm / s.means[2].max(1e-4)];
        }
    }
    if r.wb_mode == WbMode::Pick {
        if let Some(s) = stats {
            if s.pick_count > 0 {
                let gm = s.pick_means[1].max(1e-4);
                wb = [
                    gm / s.pick_means[0].max(1e-4),
                    1.0,
                    gm / s.pick_means[2].max(1e-4),
                ];
            }
        }
    }
    // resolve the .dcp profile early: an absolute-kelvin WB needs the
    // profile's color matrix to derive camera-space neutral gains.
    // heuristic camera-look names a real .dcp replaces entirely
    const DCP_LOOK_NAMES: &[&str] = &[
        "adobe_standard",
        "camera_standard",
        "camera_neutral",
        "camera_vivid",
        "camera_portrait",
        "camera_landscape",
        "camera_faithful",
    ];
    let mut prof = if !r.profile.is_empty() || DCP_LOOK_NAMES.contains(&r.look.as_str()) {
        load_dcp_profile(r, m)
    } else {
        None
    };
    // enhanced .xmp profile (crs:Look → Settings/**/Profiles/*.xmp): the
    // profile's own CameraProfile supplies the base .dcp, its baked-in
    // grade merges under the user's adjustments.
    let r_eff_storage;
    let mut r = r;
    if prof.is_none() && !r.profile.is_empty() {
        if let Some((pr, amt)) = crate::xmp::load_xmp_profile(&r.profile) {
            let mut rr = r.clone();
            crate::xmp::merge_profile_under(&mut rr, &pr, amt * r.profile_amount);
            if !pr.profile.is_empty() {
                rr.profile = pr.profile.clone();
            }
            prof = load_dcp_profile(&rr, m);
            r_eff_storage = rr;
            r = &r_eff_storage;
        }
    }
    let dcp_active = prof.is_some();

    if r.temp_abs > 0.0 {
        // absolute kelvin WB (Lightroom WhiteBalance="Custom" +
        // crs:Temperature semantics): find the camera-space neutral for
        // the target illuminant through the active color matrix, WB gains
        // are its per-channel reciprocal normalized to green.
        let t_k = r.temp_abs.clamp(1500.0, 50000.0);
        let mut xy = crate::dcp::kelvin_to_xy(t_k);
        // tint: slide the illuminant along the perpendicular green/magenta
        // axis (±150 LR). LR Tint<0 renders greener => the illuminant moves
        // magenta (lower chromaticity-y) so the WB boost green, and vice versa.
        xy[1] += r.tint_abs.clamp(-150.0, 150.0) * 0.0004;
        let xyz = crate::dcp::xy_to_xyz(xy);
        let cam_neutral = if let Some(pr) = &prof {
            pr.color_matrix_at(t_k)
                .map(|cm| crate::dcp::mat_mul_vec(cm, xyz))
        } else {
            None
        };
        let n = match cam_neutral {
            Some(n) => n,
            None => {
                // no profile: XYZ -> sRGB -> cam via inverse of libraw's
                // cam->sRGB rgb_cam
                let rgb = crate::dcp::mat_mul_vec(crate::dcp::XYZ_TO_SRGB, xyz);
                let inv = crate::dcp::invert([
                    [m.rgb_cam[0][0], m.rgb_cam[0][1], m.rgb_cam[0][2]],
                    [m.rgb_cam[1][0], m.rgb_cam[1][1], m.rgb_cam[1][2]],
                    [m.rgb_cam[2][0], m.rgb_cam[2][1], m.rgb_cam[2][2]],
                ]);
                inv.map(|im| crate::dcp::mat_mul_vec(im, rgb))
                    .unwrap_or([1.0, 1.0, 1.0])
            }
        };
        let g = n[1].max(1e-4);
        wb = [
            (g / n[0].max(1e-4)).clamp(0.05, 20.0),
            1.0,
            (g / n[2].max(1e-4)).clamp(0.05, 20.0),
        ];
    } else {
        // relative temperature/tint shift (v1 approximation)
        let t = r.temperature.clamp(-1.0, 1.0);
        let ti = r.tint.clamp(-1.0, 1.0);
        wb[0] *= 1.0 + t * 0.35 + ti * 0.12;
        wb[1] *= 1.0 - ti * 0.30;
        wb[2] *= 1.0 - t * 0.35 + ti * 0.12;
    }

    // ProPhoto (D50, linear) — ACR's working space for the profile stage
    const XYZ_D50_TO_PROPHOTO: crate::dcp::Mat3 = [
        [1.3459433, -0.2556075, -0.0511118],
        [-0.5445989, 1.5081673, 0.0205351],
        [0.0, 0.0, 1.2118128],
    ];
    const PROPHOTO_TO_SRGB: crate::dcp::Mat3 = [
        [1.7157984, -0.4799412, -0.2358566],
        [-0.5101695, 1.3918325, 0.0303034],
        [0.0925821, -0.0428812, 0.9497079],
    ];
    // cam -> sRGB: use libraw's precomputed rgb_cam (cam->sRGB, row-normalized).
    // WB is applied separately with a post-WB clip so blown highlights stay
    // white instead of shifting magenta.
    let mut cam2srgb = [
        [m.rgb_cam[0][0], m.rgb_cam[0][1], m.rgb_cam[0][2]],
        [m.rgb_cam[1][0], m.rgb_cam[1][1], m.rgb_cam[1][2]],
        [m.rgb_cam[2][0], m.rgb_cam[2][1], m.rgb_cam[2][2]],
    ];

    // ---- .dcp camera profile (Adobe colour pipeline) ----
    // recipe.profile names a profile ("Adobe Standard", "Camera Standard", ...)
    // or a .dcp path. When present we replace the libraw matrix with the
    // WB-interpolated forward matrix and keep the profile's HSV maps +
    // tone curve for colorize/adjust. BaselineExposure folds into
    // exposure_mul so the profile's expected gain happens automatically.
    let mut profile_hsm = None;
    let mut profile_look = None;
    let mut profile_tone_lut: Vec<f32> = Vec::new();
    let mut profile_baseline_ev = 0.0f32;
    let mut cam2prof: Option<crate::dcp::Mat3> = None;
    {
        let wb_neutral = [
            1.0 / wb[0].max(1e-4),
            1.0 / wb[1].max(1e-4),
            1.0 / wb[2].max(1e-4),
        ];
        if let Some(prof) = &prof {
            if let Some(mtx) = prof.cam_to_xyz_d50(wb_neutral) {
                cam2srgb = crate::dcp::mat_mul(crate::dcp::xyz_d50_to_srgb(), mtx);
                // ACR runs its profile stage (look table + tone curve) in
                // the profile working space — ProPhoto for DCPs. Render the
                // demosaic there and convert back after the profile ops:
                // per-channel curves in ProPhoto keep saturated colors
                // (e.g. the blue edge of a sunset) where an sRGB-space
                // curve would crush them.
                cam2prof = Some(crate::dcp::mat_mul(XYZ_D50_TO_PROPHOTO, mtx));
            }
            profile_hsm = prof.hue_sat_map(wb_neutral).cloned();
            profile_look = prof.look_table.clone();
            // ACR semantics: a profile with no ProfileToneCurve renders
            // through the app's built-in default curve (the filmic ~+1EV
            // midtone lift) — without it every Adobe Standard render
            // comes out a stop+ darker than Lightroom's.
            let curve_src: Vec<[f32; 2]> = if prof.tone_curve.len() >= 2 {
                prof.tone_curve
                    .iter()
                    .map(|&(x, y)| [x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)])
                    .collect()
            } else {
                crate::dcp::ACR_DEFAULT_CURVE
                    .iter()
                    .map(|&(x, y)| [x, y])
                    .collect()
            };
            // DCP ProfileToneCurve is evaluated with the DNG natural
            // cubic spline solver (dng_spline_solver), not Catmull-Rom
            let mut sp_pts: Vec<[f32; 2]> = curve_src.iter().map(|p| [p[0], p[1]]).collect();
            sp_pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
            sp_pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
            profile_tone_lut = crate::dcp::spline_lut(&sp_pts);
            // BaselineExposureOffset (0xC7A5) is the profile's own EV
            // correction — it sits on top of the file's BaselineExposure
            // per the DNG spec (e.g. DJI ships file +1.8, profile −1.0).
            profile_baseline_ev = prof.baseline_exposure + prof.baseline_exposure_offset;
        }
        // XMP LookTable (dng_look_table) replaces the DCP look
        if let Some(lt) = &r.look_table {
            profile_look = Some(lt.clone());
        }
    }

    // post-WB+matrix mean luma: luma/M/WB are all linear, so luma of the
    // mean equals the mean of luma — exact scene-mean in the working space.
    let post_mean_luma = |s: &Stats| -> f32 {
        let cm = [s.means[0] * wb[0], s.means[1] * wb[1], s.means[2] * wb[2]];
        let mut l = 0.0f32;
        for (row, wgt) in cam2srgb.iter().zip([0.2126f32, 0.7152, 0.0722]) {
            l += wgt * (row[0] * cm[0] + row[1] * cm[1] + row[2] * cm[2]);
        }
        l
    };
    // auto exposure: steer scene mean luma toward 18% gray, cap ±3EV.
    // BaselineExposure folds in here — from the .dcp (profile's expected
    // gain) and from the file itself (DNG 0xC62A — e.g. DJI writes +1.8
    // because its raws are intentionally exposed dark).
    let dng_baseline_ev = m.dng_ops.as_ref().map(|o| o.baseline_ev).unwrap_or(0.0);
    // Sony expanded-LOW ISO (50/64/80 on bodies whose native base is 100):
    // the sensor records at native ISO and the file is tagged down, so ACR
    // compensates with a -log2(100/iso) EV baseline pull — verified against
    // Lightroom previews of ISO50 files (-1EV matched within measurement
    // error). Gated to Sony; native-ISO64 cameras (e.g. Nikon D850, Fuji
    // GFX) must not be pulled.
    let iso_pull_ev = if m.info.make.to_ascii_uppercase().contains("SONY")
        && m.info.iso > 0.0
        && m.info.iso < 100.0
    {
        (m.info.iso / 100.0).log2()
    } else {
        0.0
    };
    let mut exposure_mul =
        (2.0f32).powf(r.exposure + profile_baseline_ev + dng_baseline_ev + iso_pull_ev);
    if r.auto_exposure {
        if let Some(s) = stats {
            let l = post_mean_luma(s);
            if l > 1e-4 {
                exposure_mul *= (2.0f32).powf((0.18f32 / l).log2().clamp(-3.0, 3.0));
            }
        }
    }
    // auto contrast: p1/p99 stretch, expressed in the post-exposure working
    // space (raw percentiles scaled by scene gain * exposure_mul)
    let (mut black_pt, mut white_pt) = (0.0f32, 1.0f32);
    if r.auto_contrast {
        if let Some(s) = stats {
            let scene_gain = if s.luma_mean > 1e-4 {
                post_mean_luma(s) / s.luma_mean
            } else {
                1.0
            };
            let f = scene_gain * exposure_mul;
            black_pt = (s.luma_percentile(0.01) * f).clamp(0.0, 0.35);
            white_pt = (s.luma_percentile(0.99) * f).clamp(black_pt + 0.02, 1.2);
        }
    }

    let mut spots = [[0.0; 4]; 8];
    for (i, s) in r.spots.iter().take(8).enumerate() {
        spots[i] = [
            s[0].clamp(0.0, 1.0),
            s[1].clamp(0.0, 1.0),
            s[2].clamp(0.001, 0.3),
            0.0,
        ];
    }
    let mut lights = [[0.0; 4]; 8];
    for (i, l) in r.lights.iter().take(8).enumerate() {
        lights[i] = [
            l[0].clamp(0.0, 1.0),
            l[1].clamp(0.0, 1.0),
            l[2].clamp(0.01, 1.0),
            l[3].clamp(-4.0, 4.0),
        ];
    }
    let crop = [
        r.crop[0].clamp(0.0, 0.9),
        r.crop[1].clamp(0.0, 0.9),
        r.crop[2].clamp(0.0, 0.9),
        r.crop[3].clamp(0.0, 0.9),
    ];

    // per-channel custom curves: 768-float packed luts, empty when unused
    let chan_luts = if r.curve_r.is_empty() && r.curve_g.is_empty() && r.curve_b.is_empty() {
        Vec::new()
    } else {
        let mut v = curve_lut(&r.curve_r, |x| x, 1.0);
        v.extend(curve_lut(&r.curve_g, |x| x, 1.0));
        v.extend(curve_lut(&r.curve_b, |x| x, 1.0));
        v
    };
    // hue-domain curves: hh/ss are remaps (default x), hs/hl/ls are gains (1)
    let hue_luts = if r.hue_hue.is_empty()
        && r.hue_sat.is_empty()
        && r.hue_lum.is_empty()
        && r.lum_sat.is_empty()
        && r.sat_sat.is_empty()
    {
        Vec::new()
    } else {
        let mut v = curve_lut(&r.hue_hue, |x| x, 1.0);
        v.extend(curve_lut(&r.hue_sat, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.hue_lum, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.lum_sat, |_| 1.0, 2.0));
        v.extend(curve_lut(&r.sat_sat, |x| x, 1.0));
        v
    };

    // power windows → packed [kind, p0..p5, ev, sat, temp, strength]
    // strength = opacity × enabled (folded so disabled windows are free).
    // windows are stored frame-normalized (pre-crop, matching the UI markers);
    // window_mask runs in dst-normalized post-crop space, so translate here.
    let sw = (1.0 - r.crop[0] - r.crop[2]).max(0.01);
    let sh = (1.0 - r.crop[1] - r.crop[3]).max(0.01);
    let mut wins = [[0.0; 24]; 16];
    let mut refines = [[0.0; 6]; 16];
    let mut lpcs = [[0.0; 41]; 16];
    let mut wmats: Vec<WMatte> = Vec::new();
    // Pre-load every window's LR blob-store bitmap so correction members
    // can be folded before packing.
    let mut loaded: Vec<Option<Matte>> = r
        .windows
        .iter()
        .take(16)
        .map(|w| {
            if !w.mask_file.is_empty() && w.mask_area[0] > 1.0 && w.mask_area[1] > 1.0 {
                load_mask_bitmap(&w.mask_file)
            } else {
                None
            }
        })
        .collect();
    // LR semantics: a correction's mask members combine with
    // MaskBlendMode (0 add, 1 subtract, 2 intersect); MaskInverted flips
    // the member first. When all members resolved to blob bitmaps, fold
    // them into one matte covering the union of their bboxes in the
    // shared WholeImageArea mask space — the correction applies once.
    let (mut merged, skip_win) = fold_mask_groups(&r.windows[..r.windows.len().min(16)], &loaded);
    for (i, v) in loaded.iter_mut().enumerate() {
        if skip_win[i] {
            *v = None;
        }
    }
    for (i, w) in r.windows.iter().take(16).enumerate() {
        if skip_win[i] {
            continue;
        }
        refines[i] = w.refine;
        let mut n = 0usize;
        for (row, var) in w.pcs.iter().take(2) {
            if let Some(sw) = pc_normalize(row) {
                let o = 1 + n * 20;
                lpcs[i][o..o + 3].copy_from_slice(&sw.src);
                lpcs[i][o + 3..o + 6].copy_from_slice(&sw.shift);
                lpcs[i][o + 6] = sw.amt;
                lpcs[i][o + 7..o + 11].copy_from_slice(&sw.hr);
                lpcs[i][o + 11..o + 15].copy_from_slice(&sw.sr);
                lpcs[i][o + 15..o + 19].copy_from_slice(&sw.lr);
                lpcs[i][o + 19] = var.clamp(0.0, 1.0);
                n += 1;
            }
        }
        lpcs[i][0] = n as f32;
        let grad = w.kind == "gradient";
        let (p0, p1, p2, p3) = if grad {
            // [x1,y1,x2,y2]: both endpoints move through the same map
            (
                (w.p[0] - r.crop[0]) / sw,
                (w.p[1] - r.crop[1]) / sh,
                (w.p[2] - r.crop[0]) / sw,
                (w.p[3] - r.crop[1]) / sh,
            )
        } else {
            // circle/ellipse: centre + radii rescale into crop space
            (
                (w.p[0] - r.crop[0]) / sw,
                (w.p[1] - r.crop[1]) / sh,
                w.p[2] / sw,
                w.p[3] / sh,
            )
        };
        let lum = w.kind == "lum";
        let subj = w.kind == "subject";
        let skyw = w.kind == "sky";
        let part = w.kind == "part";
        let vegw = w.kind == "veg";
        let nsky = w.kind == "nsky";
        let cran = w.kind == "crange";
        wins[i] = [
            (if grad { 1.0 } else { 0.0 })
                + (if w.invert { 2.0 } else { 0.0 })
                + (if lum { 4.0 } else { 0.0 })
                + (if w.link_q { 8.0 } else { 0.0 })
                + (if subj { 16.0 } else { 0.0 })
                + (if skyw { 32.0 } else { 0.0 })
                + (if part { 64.0 } else { 0.0 })
                + (if vegw { 128.0 } else { 0.0 })
                + (if nsky { 256.0 } else { 0.0 })
                + (if cran { 512.0 } else { 0.0 }),
            if lum || cran {
                w.p[0].clamp(0.0, 1.0)
            } else {
                p0
            },
            if lum || cran {
                w.p[1].clamp(0.0, 1.0)
            } else {
                p1
            },
            if lum || cran {
                w.p[2].clamp(0.0, 1.0)
            } else {
                p2
            },
            if lum || cran {
                w.p[3].clamp(0.0, 1.0)
            } else {
                p3
            },
            w.p[4],
            w.p[5],
            w.ev.clamp(-4.0, 4.0),
            w.sat.clamp(-1.0, 1.0),
            w.temp.clamp(-1.0, 1.0),
            if w.enabled {
                w.opacity.clamp(0.0, 1.0)
            } else {
                0.0
            },
            w.mid,
            w.contrast.clamp(-1.0, 1.0),
            w.clarity.clamp(-1.0, 1.0),
            w.dehaze.clamp(-1.0, 1.0),
            w.tint.clamp(-1.0, 1.0),
            w.hi.clamp(-1.0, 1.0),
            w.sh.clamp(-1.0, 1.0),
            w.wh.clamp(-1.0, 1.0),
            w.bl.clamp(-1.0, 1.0),
            w.texture.clamp(-1.0, 1.0),
            0.0,
            0.0,
            0.0,
        ];
        // real LR AI-mask bitmap (extracted .lrcat-data blob → PNG beside
        // the sidecar): supersedes the heuristic kind matte entirely.
        // Mask space = WholeImageArea in *sensor* orientation; the TIFF
        // covers a bbox at mask_org. Post-crop nx,ny → display-frame
        // (dx,dy) → mask-space px via the dcraw flip code → bitmap u,v:
        //   dx = crop_l + nx*sw, mx_px = f(dx,dy)*area_w - org_x, u = /bw
        // A correction-group leader carries the pre-folded member matte
        // and union origin instead of its own blob.
        let is_merged = merged.contains_key(&i);
        let merged_m = merged.remove(&i);
        let solo = std::mem::take(&mut loaded[i]);
        if let Some((bm, org)) = merged_m
            .map(|(m, o)| (m, o))
            .or_else(|| solo.map(|m| (m, w.mask_org)))
        {
            if w.mask_area[0] > 1.0 && w.mask_area[1] > 1.0 {
                let (aw, ah) = (w.mask_area[0], w.mask_area[1]);
                let (ox, oy) = (org[0], org[1]);
                let (bw, bh) = (bm.w as f32, bm.h as f32);
                let (cl, ct) = (r.crop[0], r.crop[1]);
                // display-normalized → mask-normalized per flip code,
                // composed with the crop map into u = ux*nx+uy*ny+uc.
                let (ux, uy, uc, vx, vy, vc) = match m.info.flip {
                    3 => (
                        // display = 180° rot: mx_n = 1-dx, my_n = 1-dy
                        -sw * aw / bw,
                        0.0,
                        (aw * (1.0 - cl) - ox) / bw,
                        0.0,
                        -sh * ah / bh,
                        (ah * (1.0 - ct) - oy) / bh,
                    ),
                    5 => (
                        // display = 90° CCW: mx_n = 1-dy, my_n = dx
                        0.0,
                        -sh * aw / bw,
                        (aw * (1.0 - ct) - ox) / bw,
                        sw * ah / bh,
                        0.0,
                        (cl * ah - oy) / bh,
                    ),
                    6 => (
                        // display = 90° CW: mx_n = dy, my_n = 1-dx
                        0.0,
                        sh * aw / bw,
                        (ct * aw - ox) / bw,
                        -sw * ah / bh,
                        0.0,
                        (ah * (1.0 - cl) - oy) / bh,
                    ),
                    _ => (
                        sw * aw / bw,
                        0.0,
                        (cl * aw - ox) / bw,
                        0.0,
                        sh * ah / bh,
                        (ct * ah - oy) / bh,
                    ),
                };
                // flag 1024 + clear the heuristic kind bits (16..512); keep
                // invert(2)/link_q(8). Folded mattes already applied each
                // member's MaskInverted, so drop the leader's own invert.
                let keep = if is_merged { 0b1001 } else { 0b1011 };
                wins[i][0] = ((wins[i][0] as i32 & keep) | 1024) as f32;
                wins[i][21] = wmats.len() as f32;
                wmats.push(WMatte {
                    m: bm,
                    ux,
                    uy,
                    uc,
                    vx,
                    vy,
                    vc,
                });
            }
        }
    }

    // LR PatchMatch remove spots: decode the extracted 16-bit RGB patchmap
    // into absolute source px (ch0,ch1 normalize into the search rect; ch2
    // is refinement data we don't need). pm coords live in LR's frame
    // (whole_image origin) — shift into lin px.
    let mut pms: Vec<PmMap> = Vec::new();
    for s in r.pm_spots.iter().take(12) {
        if s.file.is_empty() {
            continue;
        }
        let Some((pw, ph, src)) = load_pm_map(s) else {
            continue;
        };
        // pm coords — target/search/source *and* the feather ellipse — live
        // in LR's whole-image space, which is sensor-native like the mask
        // bitmaps (verified: target/sensor_dims ≈ ellipse). Scale the
        // normalized ellipse straight into sensor px; no flip mapping.
        let (ec, er) = (s.ellipse[0] * m.w as f32, s.ellipse[1] * m.h as f32);
        let (fw, fh) = (m.w as f32, m.h as f32);
        pms.push(PmMap {
            src,
            pw,
            ph,
            dst: [
                s.target[0] - s.frame_org[0],
                s.target[1] - s.frame_org[1],
                s.target[2] - s.frame_org[0],
                s.target[3] - s.frame_org[1],
            ],
            ell: [ec, er, s.ellipse[2] * fw, s.ellipse[3] * fh],
            opacity: s.opacity.clamp(0.0, 1.0),
        });
    }

    // adjustment brushes → flattened segments + per-stroke records.
    // Strokes live in frame-normalized coords like windows; translate into
    // dst-normalized space here. Radius is a frame-height fraction → /sh.
    // Per-stroke records (3 entries):
    //   [radius_dst, soft, opacity_signed(+paint/−erase), layer_idx]
    //   [seg_start, seg_count, 0, 0]
    //   [xmin, ymin, xmax, ymax] normalized-space bbox (unpadded)
    let mut brush_segs: Vec<[f32; 4]> = Vec::new();
    let mut brush_strokes: Vec<[f32; 4]> = Vec::new();
    let mut brush_seeds: Vec<[f32; 4]> = Vec::new();
    let mut brush_layers = [[0.0f32; 4]; 4];
    let mut brush_layers2 = [[0.0f32; 12]; 4];
    let mut link_bits = 0.0f32;
    for (li, b) in r.brushes.iter().take(4).enumerate() {
        brush_layers[li] = [
            b.ev.clamp(-4.0, 4.0),
            b.sat.clamp(-1.0, 1.0),
            b.temp.clamp(-1.0, 1.0),
            if b.enabled {
                b.opacity.clamp(0.0, 1.0)
            } else {
                0.0
            },
        ];
        brush_layers2[li] = [
            b.contrast.clamp(-1.0, 1.0),
            b.clarity.clamp(-1.0, 1.0),
            b.dehaze.clamp(-1.0, 1.0),
            b.tint.clamp(-1.0, 1.0),
            b.hi.clamp(-1.0, 1.0),
            b.sh.clamp(-1.0, 1.0),
            b.wh.clamp(-1.0, 1.0),
            b.bl.clamp(-1.0, 1.0),
            b.texture.clamp(-1.0, 1.0),
            0.0,
            0.0,
            0.0,
        ];
        if b.link_q {
            link_bits += (1u32 << li) as f32;
        }
        for st in b.strokes.iter() {
            if st.pts.len() < 2 {
                continue;
            }
            if brush_strokes.len() >= 64 * 3 {
                break;
            }
            let seg_start = brush_segs.len() as f32;
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for w in st.pts.windows(2) {
                if brush_segs.len() >= 4096 {
                    break;
                }
                let a = [(w[0][0] - r.crop[0]) / sw, (w[0][1] - r.crop[1]) / sh];
                let c = [(w[1][0] - r.crop[0]) / sw, (w[1][1] - r.crop[1]) / sh];
                x0 = x0.min(a[0].min(c[0]));
                y0 = y0.min(a[1].min(c[1]));
                x1 = x1.max(a[0].max(c[0]));
                y1 = y1.max(a[1].max(c[1]));
                brush_segs.push([a[0], a[1], c[0], c[1]]);
            }
            let nseg = brush_segs.len() as f32 - seg_start;
            if nseg < 1.0 {
                continue;
            }
            brush_strokes.push([
                (st.radius / sh).clamp(0.001, 1.0),
                st.soft.clamp(0.0, 0.98),
                if st.erase {
                    -st.opacity.abs()
                } else {
                    st.opacity.abs()
                },
                li as f32,
            ]);
            // e1.z/w: edge-aware flag + tolerance (GPU seed kernel reads
            // these straight from the Uni block)
            brush_strokes.push([
                seg_start,
                nseg,
                if b.edge_aware { 1.0 } else { 0.0 },
                if b.edge_aware {
                    b.edge_tol.clamp(0.05, 1.0)
                } else {
                    0.0
                },
            ]);
            brush_strokes.push([x0, y0, x1, y1]);
            // edge-aware: seed slot carries the tolerance as its enable
            // flag; the rgb is sampled from the image at render time.
            brush_seeds.push(if b.edge_aware {
                [0.0, 0.0, 0.0, b.edge_tol.clamp(0.05, 1.0)]
            } else {
                [0.0; 4]
            });
        }
    }
    let brush_misc = [(brush_strokes.len() / 3) as f32, link_bits, 0.0, 0.0];

    let mut clones = [[0.0; 6]; 8];
    for (i, c) in r.clones.iter().take(8).enumerate() {
        clones[i] = [
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
            c[3].clamp(0.0, 1.0),
            c[4].clamp(0.001, 0.3),
            0.0,
        ];
    }

    let zone_pack = |z: [f32; 4]| -> [f32; 4] {
        [
            z[0] - z[0].floor(),
            z[1].clamp(0.0, 1.0),
            z[2].clamp(-4.0, 4.0),
            z[3].clamp(-1.0, 1.0),
        ]
    };
    let mut mixer = r.mixer;
    for c in &mut mixer {
        *c = c.clamp(-2.0, 2.0);
    }

    // profile resolved to a matrix → demosaic into ProPhoto and convert
    // back to sRGB after the profile ops; otherwise stay in sRGB.
    let (work_m, post_profile) = match cam2prof {
        Some(mm) => (mm, PROPHOTO_TO_SRGB),
        None => (
            cam2srgb,
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ),
    };
    let mut p = Params {
        wb,
        m: work_m,
        lut: if r.linear_output {
            catmull_lut(&r.curve)
        } else if r.curve.is_empty() {
            catmull_lut(BASE_TONE_CURVE)
        } else {
            catmull_lut(&r.curve)
        },
        exposure_mul,
        contrast: r.contrast,
        highlights: r.highlights,
        shadows: r.shadows,
        whites: r.whites,
        blacks: r.blacks,
        saturation: r.saturation,
        vibrance: r.vibrance,
        sharpen: r.sharpen,
        sharp_mask: r.sharp_mask.clamp(0.0, 1.0),
        sharp_radius: r.sharp_radius.clamp(1.0, 3.0),
        sharp_detail: r.sharp_detail.clamp(0.0, 1.0),
        noise_luma: r.noise_luma,
        rotation_deg: r.rotation_deg.clamp(-45.0, 45.0),
        clarity: r.clarity.clamp(-1.0, 1.0),
        texture: r.texture.clamp(-1.0, 1.0),
        vignette: r.vignette.clamp(-1.0, 1.0),
        vig_shape: if r.vig_mid < 0.0 && r.vig_round <= -2.0 && r.vig_feather < 0.0 {
            [-1.0, 0.0, 0.0]
        } else {
            [
                if r.vig_mid >= 0.0 {
                    r.vig_mid.clamp(0.0, 1.0)
                } else {
                    0.5
                },
                if r.vig_round > -2.0 {
                    r.vig_round.clamp(-1.0, 1.0)
                } else {
                    0.0
                },
                if r.vig_feather >= 0.0 {
                    r.vig_feather.clamp(0.0, 1.0)
                } else {
                    0.5
                },
            ]
        },
        grain: r.grain.clamp(0.0, 1.0),
        grain_size: 1.0 + r.grain_size.clamp(0.0, 1.0) * 4.0,
        grain_rough: r.grain_rough.clamp(0.0, 1.0),
        lift: [
            r.lift[0].clamp(-0.5, 0.5),
            r.lift[1].clamp(-0.5, 0.5),
            r.lift[2].clamp(-0.5, 0.5),
        ],
        gamma: [
            r.gamma[0].clamp(0.2, 5.0),
            r.gamma[1].clamp(0.2, 5.0),
            r.gamma[2].clamp(0.2, 5.0),
        ],
        gain: [
            r.gain[0].clamp(0.0, 4.0),
            r.gain[1].clamp(0.0, 4.0),
            r.gain[2].clamp(0.0, 4.0),
        ],
        shadow_col: hue_to_rgb(r.shadow_hue),
        shadow_sat: r.shadow_sat.clamp(0.0, 1.0),
        high_col: hue_to_rgb(r.highlight_hue),
        high_sat: r.highlight_sat.clamp(0.0, 1.0),
        grade_balance: r.grade_balance.clamp(-1.0, 1.0),
        grade_blend: r.grade_blend.clamp(0.0, 1.0),
        black_pt,
        white_pt,
        spots,
        n_spots: r.spots.len().min(8) as u32,
        lights,
        n_lights: r.lights.len().min(8) as u32,
        crop,
        offset: [
            r.offset[0].clamp(-0.25, 0.25),
            r.offset[1].clamp(-0.25, 0.25),
            r.offset[2].clamp(-0.25, 0.25),
        ],
        mid_col: hue_to_rgb(r.midtone_hue),
        mid_sat: r.midtone_sat.clamp(0.0, 1.0),
        pivot: r.pivot.clamp(0.02, 0.8),
        hl_roll: r.highlight_rolloff.clamp(0.0, 2.0),
        sh_roll: r.shadow_rolloff.clamp(0.0, 2.0),
        chan_luts,
        hue_luts,
        qh: [
            r.qh[0] - r.qh[0].floor(),
            r.qh[1].clamp(0.0, 0.5),
            r.qh[2].clamp(0.0, 0.5),
        ],
        qs: [
            r.qs[0].clamp(0.0, 1.0),
            r.qs[1].clamp(0.0, 1.0),
            r.qs[2].clamp(0.0, 0.5),
        ],
        ql: [
            r.ql[0].clamp(0.0, 1.0),
            r.ql[1].clamp(0.0, 1.0),
            r.ql[2].clamp(0.0, 0.5),
        ],
        qadj: [
            r.qadj[0].clamp(-0.5, 0.5),
            r.qadj[1].clamp(-1.0, 1.0),
            r.qadj[2].clamp(-1.0, 1.0),
            r.qadj[3].clamp(-1.0, 1.0),
        ],
        q_invert: r.q_invert,
        q_clean: [r.q_clean[0].clamp(0.0, 1.0), r.q_clean[1].clamp(0.0, 1.0)],
        q_blur: r.q_blur.clamp(0.0, 1.0),
        q_show: r.q_show,
        brush_segs,
        brush_strokes,
        brush_layers,
        brush_layers2,
        brush_misc,
        brush_seeds,
        // show-key mode works even with no adjustment applied
        has_qual: r.qh[1] > 0.0,
        wins,
        refines,
        wmats,
        pms,
        pm_frame: [m.w as f32, m.h as f32],
        lpcs,
        n_wins: r.windows.len().min(16) as u32,
        zones: [
            zone_pack(r.z_dark),
            zone_pack(r.z_shadow),
            zone_pack(r.z_light),
            zone_pack(r.z_global),
        ],
        mixer,
        mono: r.mono,
        clones,
        n_clones: r.clones.len().min(8) as u32,
        beauty: r.beauty.clamp(0.0, 1.0),
        noise_chroma: r.noise_chroma.clamp(0.0, 1.0),
        dehaze: r.dehaze.clamp(0.0, 1.0) * 0.85,
        dehaze_a: stats
            .map(|s| s.dark_percentile(0.995))
            .unwrap_or(1.0)
            .max(0.05),
        ca_fix: r.ca_fix.clamp(-0.5, 0.5),
        ca_r: 0.0,
        ca_b: 0.0,
        deband: r.deband.clamp(0.0, 1.0),
        glow: r.glow.clamp(0.0, 1.0),
        flare: [
            r.flare[0].clamp(0.0, 1.0),
            r.flare[1].clamp(0.0, 1.0),
            r.flare[2].clamp(0.0, 1.0),
            r.flare[3] - r.flare[3].floor(),
        ],
        defringe: [
            r.defringe_pu[0].clamp(0.0, 1.0),
            r.defringe_pu[1],
            r.defringe_pu[2],
            0.0,
            r.defringe_gr[0].clamp(0.0, 1.0),
            r.defringe_gr[1],
            r.defringe_gr[2],
            0.0,
        ],
        zone_ev: {
            let mut z = r.zones_ev;
            for v in &mut z {
                *v = v.clamp(-4.0, 4.0);
            }
            z
        },
        // embedded Adobe profile LUT takes precedence over a file LUT
        lut3d: r.rgb_lut.clone().or_else(|| {
            if !r.lut_file.is_empty() && r.lut_amount > 0.0 {
                crate::lut::load(&r.lut_file)
            } else {
                None
            }
        }),
        lut_amt: r.lut_amount.clamp(0.0, 1.0),
        key_v: r.key_v.clamp(-0.4, 0.4),
        key_h: r.key_h.clamp(-0.4, 0.4),
        lens_corr_amt: r.lens_corr.min(1.0),
        tone_lut: build_tone_lut(r),
        tone_sh: build_tone_lut(&Recipe {
            highlights: 0.0,
            whites: 0.0,
            highlight_rolloff: 1.0,
            ..r.clone()
        }),
        tone_hl: build_tone_lut(&Recipe {
            shadows: 0.0,
            blacks: 0.0,
            shadow_rolloff: 1.0,
            ..r.clone()
        }),
        has_tone: r.shadows != 0.0
            || r.highlights != 0.0
            || r.whites != 0.0
            || r.blacks != 0.0
            || r.highlight_rolloff != 1.0
            || r.shadow_rolloff != 1.0,
        stages: Vec::new(),
        cs: r.color_slice,
        has_cs: r.color_slice.iter().any(|s| s[3] > 0.5),
        warper: r.warper.iter().take(8).cloned().collect(),
        point_colors: r.point_colors.clone(),
        point_color_vars: r.point_color_vars.clone(),
        profile_hsm,
        profile_look,
        profile_tone_lut,
        post_profile,
        calib: calib_matrix(&r.calib),
        lens_blur: r.lens_blur.clamp(0.0, 1.0),
        lens_blur_opts: r.lens_blur_opts,
        lens_blur_opts2: r.lens_blur_opts2,
    };
    // heuristic look adjustments are skipped when the named look was
    // upgraded to a real .dcp (the profile IS the look now)
    if !(dcp_active && DCP_LOOK_NAMES.contains(&r.look.as_str())) {
        apply_look(&mut p, &r.look);
    }
    // Lightroom-catalog images render through ACR's adaptive default tone
    // scale: the measured residual vs our pipeline is a strong shadow lift
    // converging to a mild highlight compression — fitted empirically from
    // digest-matched Lightroom previews and composed on top of the recipe's
    // tone LUT. PV15 uses a weaker variant (blend 0.5 toward identity) —
    // its residual has the same shape but smaller magnitude.
    if r.process_version > 0.0 {
        // fitted against digest-matched previews: the blend does NOT
        // attenuate with |Exposure2012| — a constant ~0.5 beats every
        // tested exposure-dependent falloff (LR's adaptive tone keeps
        // working even under a big user push)
        let k = if r.process_version < 12.0 { 1.0 } else { 0.5 };
        if k > 0.0 {
            for v in &mut p.lut {
                *v = *v + (pv2012_tone(*v) - *v) * k;
            }
        }
    }
    // serial stages: each stage is a full Recipe evaluated with the same
    // mosaic+stats context. Nested stages are stripped to bound the work.
    p.stages = r
        .stages
        .iter()
        .filter(|s| s.enabled)
        .take(4)
        .map(|s| {
            let mut sr = (*s.params).clone();
            sr.stages = Vec::new();
            // stage masks/brush coords live in the base stage's dst space;
            // stages compose ON TOP of the base look — their empty curve
            // must mean "identity", not a second base tone curve.
            sr.crop = r.crop;
            sr.linear_output = true;
            (
                build_params(m, &sr, stats),
                s.opacity.clamp(0.0, 1.0),
                s.invert,
            )
        })
        .collect();
    p
}

/// EV-domain tone curve: extended-Reinhard shoulder (white point from Whites,
/// extra compression from Highlights and highlight_rolloff) plus a toe shaped
/// by Blacks (soft-knee crush or fade lift), Shadows lift and shadow_rolloff.
/// 512 samples over linear luma 0..1.6, monotone.
/// Tone-map constants — defaults fitted against digest-fresh Lightroom
/// previews; SL_TONE_LUT="8 or 10 comma floats" overrides for offline fits
/// (last two: eval-luma floor, gain cap — the WGSL mirror is hardcoded).
fn tone_fit_consts() -> [f32; 11] {
    static C: std::sync::OnceLock<[f32; 11]> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        let mut c = [0.0488f32, 0.9349, 3.0063, 0.6151, 1.6655, 1.5514, 0.3064, 0.0463, 0.04, 32.0, 1.5];
        if let Ok(v) = std::env::var("SL_TONE_LUT") {
            let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if p.len() >= 8 {
                let n = p.len().min(11);
                c[..n].copy_from_slice(&p[..n]);
            }
        }
        c
    })
}

fn build_tone_lut(r: &Recipe) -> [f32; 512] {
    let sh = r.shadows;
    let hl = r.highlights;
    // diagnostic override: SL_TONE_LUT="sh_mul,sh_band,hl_mul,hl_lo,hl_hi,
    // wh_scale,wl_min,bk_mul" replaces the fitted constants for offline
    // fitting against Lightroom previews.
    let c = tone_fit_consts();
    let (sh_mul, sh_band, hl_mul, hl_lo, hl_hi) = (c[0], c[1], c[2], c[3], c[4]);
    // constants fitted against digest-fresh Lightroom preview renders
    // across 13 slider-diverse images (differential evolution on the full
    // downstream chain): LR's Highlights recovery compresses ~2.4x harder
    // than the original constants, its Shadows band is wider but weaker,
    // and the whites shoulder keeps a higher minimum white point.
    let mut wl = (1.0 + r.whites * c[5]).clamp(c[6], 3.0);
    // highlight_rolloff > 1 compresses harder (smaller white point)
    if r.highlight_rolloff > 1.0 {
        wl /= 1.0 + 0.8 * (r.highlight_rolloff - 1.0);
    }
    let wl2 = wl * wl;
    // scale so the shoulder maps y=1 to ~1
    let norm = 1.0 * (1.0 + 1.0 / wl2) / 2.0;
    let b = r.blacks;
    let shr = r.shadow_rolloff;
    let lut: Vec<f32> = (0..512)
        .map(|i| {
            let y0 = 1.6 * i as f32 / 511.0;
            let mut y = y0;
            // shadows lift (low band gain, linear domain)
            if sh != 0.0 {
                y += sh * sh_mul * (1.0 - sstep(0.0, sh_band, y0)) * (1.0 - y0 / 1.6).max(0.0);
            }
            // shadow rolloff shaping
            if shr != 1.0 {
                let w = 1.0 - sstep(0.0, 0.35, y0);
                y *= 1.0 + (1.0 - shr) * 0.35 * w;
                y *= 1.0 - (shr - 1.0).max(0.0) * 0.25 * w;
            }
            // blacks: >0 fades (lift toe), <0 crushes with a soft knee
            if b > 0.0 {
                let k = b * 0.10;
                y = k + (1.0 - k) * y;
            } else if b < 0.0 {
                let k = -b * c[7];
                let e = 0.004f32;
                let soft = |v: f32| ((v - k) + ((v - k) * (v - k) + e * e).sqrt()) * 0.5;
                y = (soft(y) - soft(0.0)) / (soft(1.0) - soft(0.0));
            }
            // extended-Reinhard shoulder
            y = y * (1.0 + y / wl2) / (1.0 + y);
            y /= norm;
            // highlights recovery: LR slider <0 recovers (compresses the
            // upper band), >0 brightens — extra compression in the upper band
            if hl != 0.0 {
                y /= (1.0 - hl * hl_mul * sstep(hl_lo, hl_hi, y)).max(0.15);
            }
            y.clamp(0.0, 1.0)
        })
        .collect();
    let mut out = [0.0f32; 512];
    out.copy_from_slice(&lut);
    // enforce monotonicity
    for i in 1..512 {
        if out[i] < out[i - 1] {
            out[i] = out[i - 1];
        }
    }
    // exposure-push headroom compression: past +1EV, LR crushes the whole
    // above-white range far below the shoulder — measured on digest-matched
    // previews (+2.4EV maps l≈1.1→0.13 lin where the shoulder alone gives
    // ~0.5). Applied AFTER the monotone pass because LR's recovered-
    // highlight mapping is genuinely non-monotone in extreme pushes.
    if c[10] != 0.0 && r.exposure > 1.0 {
        for (i, v) in out.iter_mut().enumerate() {
            let y0 = 1.6 * i as f32 / 511.0;
            *v /= (1.0 + (r.exposure - 1.0) * c[10] * sstep(0.8, 1.8, y0)).max(0.15);
        }
    }
    out
}

/// ACR PV2012 default tone-scale compensation (empirical, linear domain).
/// Measured as the mean ours→Lightroom transfer across digest-matched
/// preview renders of unedited PV11.0 Sony files: a strong toe lift that
/// converges to a slight highlight shoulder. Control points (x -> y):
const PV2012_TONE: &[[f32; 2]] = &[
    [0.0, 0.0],
    [0.001, 0.0131],
    [0.0072, 0.0371],
    [0.0145, 0.0486],
    [0.025, 0.0620],
    [0.0387, 0.0847],
    [0.0611, 0.1104],
    [0.086, 0.1364],
    [0.1232, 0.1719],
    [0.1727, 0.2166],
    [0.2456, 0.2826],
    [0.4009, 0.3973],
    [0.6201, 0.5654],
    [0.8, 0.74],
    [1.0, 0.93],
];

fn pv2012_tone(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let n = PV2012_TONE.len();
    for w in PV2012_TONE.windows(2) {
        let (a, b) = (w[0], w[1]);
        if x <= b[0] {
            let t = if b[0] > a[0] {
                (x - a[0]) / (b[0] - a[0])
            } else {
                1.0
            };
            return a[1] + (b[1] - a[1]) * t;
        }
    }
    // above the last control point: continue the mild shoulder
    PV2012_TONE[n - 1][1] + (x - PV2012_TONE[n - 1][0]).max(0.0) * 0.3
}

/// smoothstep(edge0, edge1, x) helper
/// ColorSlice wedge centers (DaVinci Red/Skin/Yellow/Green/Cyan/Blue/Magenta).
const SLICE_CENTER: [f32; 7] = [0.0, 0.0833, 0.1667, 0.3333, 0.5, 0.6667, 0.8333];

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

fn srgb_decode(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// tone equalizer: interpolated EV for a pixel of linear luma `l`.
/// Zones are centered at log2 EV -4..+4 (9 zones); linear interp between
/// centers == unit-partitioned triangle weights.
fn zone_ev_at(l: f32, z: &[f32; 9]) -> f32 {
    let e = l.max(1e-6).log2();
    let t = (e + 4.0).clamp(0.0, 8.0);
    let i = (t as usize).min(7);
    let f = t - i as f32;
    z[i] * (1.0 - f) + z[i + 1] * f
}

/// apply tone+color adjustments on linear sRGB triple. Shared logic with gpu shader.
/// order: exposure -> auto-contrast remap -> lift/gamma/gain -> blacks/whites ->
///        contrast -> shadows/highlights -> saturation/vibrance -> split tone -> LUT
/// LR camera calibration → 3x3 matrix + shadow tint packed into 12
/// floats. Each primary's chroma vector (primary - gray/3) is rotated in
/// the plane ⟂ (1,1,1) by its hue slider (±60° max, + = toward the next
/// primary on the hue wheel) and scaled by its saturation slider.
fn calib_matrix(c: &[f32; 7]) -> [f32; 12] {
    let g = 1.0f32 / 3.0;
    let gh = 1.0 / 3.0f32.sqrt();
    let mut out = [0.0f32; 12];
    for j in 0..3 {
        let mut v = [0.0f32; 3];
        v[j] = 1.0 - g;
        v[(j + 1) % 3] -= g;
        v[(j + 2) % 3] -= g;
        let vl = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
        let e1 = [v[0] / vl, v[1] / vl, v[2] / vl];
        // e2 = gray_hat × e1 (unit, since gray ⊥ e1)
        let e2 = [
            gh * (e1[2] - e1[1]),
            gh * (e1[0] - e1[2]),
            gh * (e1[1] - e1[0]),
        ];
        let h = c[1 + j * 2].clamp(-1.0, 1.0) * std::f32::consts::FRAC_PI_3;
        let s = 1.0 + c[2 + j * 2].clamp(-1.0, 1.0);
        let (ch, sh) = (h.cos() * vl * s, h.sin() * vl * s);
        for k in 0..3 {
            // column j = gray/3 + s * rotated chroma
            out[k * 3 + j] = g + ch * e1[k] + sh * e2[k];
        }
    }
    out[9] = c[0].clamp(-1.0, 1.0);
    out
}

// ---- LR Point Color (crs:PointColors) ------------------------------------
//
// A swatch is a feathered trapezoid selection over hue/sat/lum: each axis
// has [LowerNone, LowerFull, UpperFull, UpperNone] bounds (full weight
// between the inner pair, feathering to zero at the outer pair), and the
// mask is the product of the three axis weights. RangeAmount scales each
// range's width about its centre (LR "Range" slider: 0.5 = as-stored).
// Selected pixels get weighted hue/sat/lum shifts.

/// normalized swatch: src = [h01, sat, lum], shift = [dh_cyc, ds, dl],
/// ranges = [LN, LF, UF, UN] per axis
pub struct PcSwatch {
    pub src: [f32; 3],
    pub shift: [f32; 3],
    pub amt: f32,
    pub hr: [f32; 4],
    pub sr: [f32; 4],
    pub lr: [f32; 4],
}

/// standard trapezoid: 0 below ln, ramp to 1 at lf, flat to uf, 0 past un
fn trap01(x: f32, ln: f32, lf: f32, uf: f32, un: f32) -> f32 {
    let up = if lf > ln + 1e-6 {
        ((x - ln) / (lf - ln)).clamp(0.0, 1.0)
    } else {
        if x >= lf {
            1.0
        } else {
            0.0
        }
    };
    let dn = if un > uf + 1e-6 {
        ((un - x) / (un - uf)).clamp(0.0, 1.0)
    } else {
        if x <= uf {
            1.0
        } else {
            0.0
        }
    };
    up.min(dn)
}

/// detect the 19-float layout. Two hypotheses:
///   A (alphabetical, matches the LrDevelopController swatch dump):
///     [HR(LF,LN,UF,UN), HueShift, LR(LF,LN,UF,UN), LumScale, RangeAmount,
///      SR(LF,LN,UF,UN), SatScale, SrcHue, SrcLum, SrcSat]
///   B (logical src-first):
///     [SrcHue, SrcSat, SrcLum, HueShift, SatScale, LumScale, RangeAmount,
///      HR, SR, LR] with natural [LN,LF,UF,UN] ranges.
/// Returns the normalized swatch, or None for the all-(-1) sentinel /
/// inconsistent rows.
pub fn pc_normalize(v: &[f32; 19]) -> Option<PcSwatch> {
    if v.iter().all(|x| *x < 0.0) {
        return None;
    }
    let in01 = |x: f32| (0.0..=1.0 + 1e-4).contains(&x);
    // layout B (confirmed against real LR preset XMP — Adobe Premium
    // "Adaptive" presets serialize swatches this way, and community
    // dumps of the Lua PointColors structure match): src at 0..3
    // (hue 0..6, sat, lum), shifts at 3..6 (hue, sat, lum),
    // RangeAmount at 6, natural-order [LN,LF,UF,UN] ranges at 7,11,15.
    let b_ok = (0.0..=6.0).contains(&v[0])
        && in01(v[1])
        && in01(v[2])
        && in01(v[6])
        && [7usize, 11, 15].iter().all(|&i| {
            v[i] <= v[i + 1] + 1e-4 && v[i + 1] <= v[i + 2] + 1e-4 && v[i + 2] <= v[i + 3] + 1e-4
        });
    if b_ok {
        return Some(PcSwatch {
            src: [v[0] / 6.0, v[1], v[2]],
            shift: [v[3], v[4], v[5]],
            amt: v[6],
            hr: [v[7], v[8], v[9], v[10]],
            sr: [v[11], v[12], v[13], v[14]],
            lr: [v[15], v[16], v[17], v[18]],
        });
    }
    // layout A (alternative hypothesis kept for robustness): range
    // blocks [LF,LN,UF,UN] at 0,5,11; shifts at 4,9,15; amount at 10;
    // src at 16..19 (hue 0..6, lum, sat)
    let a_ok = in01(v[10])
        && (0.0..=6.0).contains(&v[16])
        && in01(v[17])
        && in01(v[18])
        && [0usize, 5, 11].iter().all(|&i| {
            v[i] >= v[i + 1] - 1e-4        // LF >= LN
                && v[i + 1] <= v[i + 2] + 1e-4 // LN <= UF
                && v[i] <= v[i + 2] + 1e-4   // LF <= UF
                && v[i + 2] <= v[i + 3] + 1e-4 // UF <= UN
        });
    if a_ok {
        return Some(PcSwatch {
            src: [v[16] / 6.0, v[18], v[17]],
            shift: [v[4], v[15], v[9]],
            amt: v[10],
            hr: [v[1], v[0], v[2], v[3]],
            lr: [v[6], v[5], v[7], v[8]],
            sr: [v[12], v[11], v[13], v[14]],
        });
    }
    None
}

/// scale trapezoid bounds about their centre by k (RangeAmount*2)
fn range_scale(r: [f32; 4], k: f32) -> [f32; 4] {
    let c = (r[0] + r[3]) * 0.5;
    let mut o = [0.0f32; 4];
    for (i, b) in r.iter().enumerate() {
        o[i] = (c + (*b - c) * k).clamp(0.0, 1.0);
    }
    // keep ordering after clamping
    for i in 1..4 {
        if o[i] < o[i - 1] {
            o[i] = o[i - 1];
        }
    }
    o
}

/// apply one swatch: w = w_h*w_s*w_l over (hue, sat, brightness); the lum
/// axis uses the HSV value channel — that is the metric LR's sampled
/// SrcLum most closely matches, and it places the picked colour inside
/// its own ranges by construction. The hue range is a strip recentered
/// on the picked hue (src maps to 0.5 — LR's range defaults are constant
/// regardless of SrcHue, so the range cannot be absolute), while the
/// sat/lum ranges are absolute axis values.
///
/// `var` is the LR Color Variance (-1..1): after applying the swatch's
/// HSL shift, colours are pushed away from (var>0) or pulled toward
/// (var<0) the shifted swatch colour — LR pushes/pulls around the
/// "after" side of the swatch.
pub fn pc_apply(x: [f32; 3], sw: &PcSwatch, var: f32) -> [f32; 3] {
    let (h, s, v) = rgb_to_hsv(x);
    let l = v;
    let k = (sw.amt * 2.0).max(0.0);
    let hr = range_scale(sw.hr, k);
    let sr = range_scale(sw.sr, k);
    let lr = range_scale(sw.lr, k);
    let hpos = (h - sw.src[0] + 0.5).rem_euclid(1.0);
    let wh = trap01(hpos, hr[0], hr[1], hr[2], hr[3]);
    let w = wh * trap01(s, sr[0], sr[1], sr[2], sr[3]) * trap01(l, lr[0], lr[1], lr[2], lr[3]);
    if w <= 1e-4 {
        return x;
    }
    // full-strength shifted colour (the selection weight blends it in)
    let xs = hsv_to_rgb(
        (h + sw.shift[0] * 0.5).rem_euclid(1.0),
        (s * (1.0 + sw.shift[1])).clamp(0.0, 1.0),
        (v * (1.0 + sw.shift[2])).max(0.0),
    );
    let tgt = if var.abs() > 1e-4 {
        // "after" colour: the swatch source with its shifts applied
        let a = hsv_to_rgb(
            (sw.src[0] + sw.shift[0] * 0.5).rem_euclid(1.0),
            (sw.src[1] * (1.0 + sw.shift[1])).clamp(0.0, 1.0),
            (sw.src[2] * (1.0 + sw.shift[2])).max(0.0),
        );
        [
            a[0] + (xs[0] - a[0]) * (1.0 + var),
            a[1] + (xs[1] - a[1]) * (1.0 + var),
            a[2] + (xs[2] - a[2]) * (1.0 + var),
        ]
    } else {
        xs
    };
    [
        x[0] + (tgt[0] - x[0]) * w,
        x[1] + (tgt[1] - x[1]) * w,
        x[2] + (tgt[2] - x[2]) * w,
    ]
}

/// Everything in adjust() that precedes the tone-map stage — factored out
/// for diagnostics (SL_DUMP_PRETONE writes this stage's rgb per pixel).
/// Returns rgb right before the tone LUT.
fn pre_tone_rgb(rgb: [f32; 3], p: &Params, veil_min: f32) -> [f32; 3] {
    let mut x = rgb;
    // camera calibration: primary hue/sat matrix + shadow green/magenta
    let ident = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    if p.calib[..9]
        .iter()
        .zip(ident)
        .any(|(a, b)| (a - b).abs() > 1e-5)
    {
        x = [
            p.calib[0] * x[0] + p.calib[1] * x[1] + p.calib[2] * x[2],
            p.calib[3] * x[0] + p.calib[4] * x[1] + p.calib[5] * x[2],
            p.calib[6] * x[0] + p.calib[7] * x[1] + p.calib[8] * x[2],
        ];
    }
    if p.calib[9] != 0.0 {
        let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        x[1] -= p.calib[9] * (1.0 - sstep(0.0, 0.4, l)) * 0.05;
    }
    // exposure — a scene-linear gain (baseline + user EV) so it must
    // precede the profile's nonlinear ops: ACR pushes hot data into the
    // tone curve's shoulder rather than scaling the mapped output.
    for c in &mut x {
        *c *= p.exposure_mul;
    }
    // .dcp profile "look": ProfileLookTable (HSV shift in working space)
    // then ProfileToneCurve — the profile's base rendering, applied before
    // user edits (dcpsim order: matrix -> looktable -> tonecurve -> user).
    if let Some(lt) = &p.profile_look {
        x = lt.apply(x);
    }
    if !p.profile_tone_lut.is_empty() {
        for c in &mut x {
            *c = lut_at(&p.profile_tone_lut, c.clamp(0.0, 1.0));
        }
    }
    // exit the profile working space (ProPhoto -> sRGB when a DCP was
    // resolved; identity otherwise) — everything below is output-referred.
    if p.post_profile != [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
        x = [
            p.post_profile[0][0] * x[0] + p.post_profile[0][1] * x[1] + p.post_profile[0][2] * x[2],
            p.post_profile[1][0] * x[0] + p.post_profile[1][1] * x[1] + p.post_profile[1][2] * x[2],
            p.post_profile[2][0] * x[0] + p.post_profile[2][1] * x[1] + p.post_profile[2][2] * x[2],
        ];
    }
    // auto-contrast percentile remap
    if p.white_pt - p.black_pt < 0.999 || p.black_pt > 0.001 {
        for c in &mut x {
            *c = (*c - p.black_pt) / (p.white_pt - p.black_pt).max(0.02);
        }
    }
    // dehaze (dark-channel prior): veil = dehaze * min(r,g,b); transmission
    // t = 1 - veil/A floored at 0.35; x' = (x - veil)/t — mirrors gpu.rs
    if p.dehaze > 0.0 {
        // dark-channel prior: the veil is a SPATIALLY SMOOTHED min-channel
        // (patchwise min, blurred) — a per-pixel min underestimates haze on
        // bright/chromatic surfaces, which is where LR's Dehaze bites.
        let mn = if veil_min.is_finite() {
            veil_min
        } else {
            x[0].min(x[1]).min(x[2])
        };
        let t = (1.0 - p.dehaze * mn / p.dehaze_a).max(0.35);
        let v = p.dehaze * mn;
        for c in &mut x {
            *c = ((*c - v) / t).max(0.0);
        }
    }
    // tone equalizer: per-zone exposure on the post-exposure luma
    if p.zone_ev.iter().any(|&v| v != 0.0) {
        let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let evg = 2.0f32.powf(zone_ev_at(l, &p.zone_ev));
        for c in &mut x {
            *c *= evg;
        }
    }
    // lift/gamma/gain: out = gain * pow(x + lift, 1/gamma)
    for c in 0..3 {
        if p.lift[c] != 0.0 || p.gamma[c] != 1.0 || p.gain[c] != 1.0 {
            x[c] = p.gain[c] * (x[c] + p.lift[c]).max(0.0).powf(1.0 / p.gamma[c]);
        }
    }
    // global offset wheel
    for c in 0..3 {
        x[c] += p.offset[c];
    }
    x
}

/// tone-map gain: evaluate the LUT at the pixel luma and apply it as a
/// ratio so hue stays put. Constants are fitted against digest-fresh LR
/// previews — the low evaluation floor + hard gain cap reproduce LR's
/// deep-shadow lift without runaway noise amplification.
/// LR's shadows/highlights are spatially adaptive: the tone gain for a
/// pixel depends on its surround (masked adjustment). We reproduce this by
/// evaluating the tone LUT at an effective luma blended toward a heavily
/// blurred luma guide. SL_SPAT="mode,w,sigma" (env): mode 1 = eval toward
/// guide, 2 = away from guide, 4 = gain-space blend.
fn spatial_tone() -> (i32, f32, f32, f32) {
    static V: std::sync::OnceLock<(i32, f32, f32, f32)> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let s = std::env::var("SL_SPAT").unwrap_or_default();
        let mut it = s.split(',').map(|v| v.trim().parse::<f32>().unwrap_or(0.0));
        (
            it.next().unwrap_or(0.0) as i32,
            it.next().unwrap_or(0.0),
            it.next().unwrap_or(0.0),
            it.next().unwrap_or(0.0),
        )
    })
}

fn tone_gain(x: [f32; 3], p: &Params, guide: f32) -> f32 {
    let c = tone_fit_consts();
    let l = (0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2]).clamp(0.0, 1.6);
    let (mode, w, w_hl, _sig) = spatial_tone();
    let le = l.max(c[8]);
    let base = tone_lut_at(&p.tone_lut, le / 1.6) / le;
    if !guide.is_finite() || (w == 0.0 && w_hl == 0.0) {
        return base.min(c[9]);
    }
    let g = guide.clamp(0.0, 1.6).max(c[8]);
    let gg = tone_lut_at(&p.tone_lut, g / 1.6) / g;
    let gain = match mode {
        1 => tone_lut_at(&p.tone_lut, ((l + w * (guide - l)).max(c[8])).min(1.6) / 1.6)
            / (l + w * (guide - l)).max(c[8]).min(1.6),
        2 => tone_lut_at(&p.tone_lut, ((l + w * (l - guide)).max(c[8])).min(1.6) / 1.6)
            / (l + w * (l - guide)).max(c[8]).min(1.6),
        // regional deviation, split: shadow-band deviation and
        // highlight-band deviation exponentiated separately
        5 => {
            let gsh = tone_lut_at(&p.tone_sh, g / 1.6) / g;
            let ghl = tone_lut_at(&p.tone_hl, g / 1.6) / g;
            base * gsh.powf(w) * ghl.powf(w_hl)
        }
        // base-detail split: the tone curve acts on the smooth base only,
        // high-frequency detail is preserved: out = T(g) + w·(l − g)
        7 | 8 => (tone_lut_at(&p.tone_lut, g / 1.6) + w * (l - guide)) / le,
        // masked delta: l + (1-w)·D(l) + w·D(g), D = T−identity
        6 => {
            let dl = base * le - le;
            let dg = gg * g - g;
            (le + (1.0 - w) * dl + w * dg) / le
        }
        _ => base * (1.0 - w) + gg * w,
    };
    gain.max(0.0).min(c[9])
}

#[allow(dead_code)] // tests use the guide-free entry point
fn adjust(rgb: [f32; 3], p: &Params) -> [f32; 3] {
    adjust_g(rgb, p, f32::NAN, f32::NAN)
}

fn adjust_g(rgb: [f32; 3], p: &Params, guide: f32, veil_min: f32) -> [f32; 3] {
    let mut x = pre_tone_rgb(rgb, p, veil_min);
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let dbg = std::env::var_os("SL_PROBE2").is_some()
        && x[0] + x[1] + x[2] > 0.06
        && x[0] + x[1] + x[2] < 0.30
        && N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 40;
    if dbg {
        eprintln!("[adj] pretone={x:?} tone_lut0.1={:.4}", tone_lut_at(&p.tone_lut, 0.1 / 1.6));
    }
    // EV-domain tone map (luminance-preserving): folds shadows/highlights/
    // whites/blacks + rolloff into one curve — hue stays put, highlights
    // roll off on an extended-Reinhard shoulder instead of clipping
    if p.has_tone {
        let g = tone_gain(x, p, guide);
        for c in &mut x {
            *c *= g;
        }
    }
    if dbg {
        eprintln!("[adj] tone={x:?}");
    }
    // contrast around adjustable pivot, then soft-limit into ~[0,1.35] —
    // extreme pushes compress smoothly instead of hard-clipping at the
    // curve LUT (LR's S-curve contrast preserves both ends)
    let cm = 1.0 + p.contrast * 0.9;
    if p.contrast != 0.0 {
        for c in &mut x {
            let v = (*c - p.pivot) * cm + p.pivot;
            let s = |u: f32| (u + (u * u + 0.0016).sqrt()) * 0.5;
            *c = 1.35 - s(1.35 - s(v));
        }
    }
    if dbg {
        eprintln!("[adj] contrast={x:?}");
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
    // defringe (LR): desaturate chromatic pixels inside the purple and
    // green hue bands — CA fringe lives in those bands at high sat
    if p.defringe[0] > 0.0 || p.defringe[4] > 0.0 {
        let (h, s, v) = rgb_to_hsv(x);
        let deg = h * 360.0;
        let gate = |lo: f32, hi: f32| -> f32 {
            let mid = (lo + hi) * 0.5;
            let mut d = (deg - mid).abs();
            if d > 180.0 {
                d = 360.0 - d;
            }
            let half = ((hi - lo).abs() * 0.5).max(0.5);
            1.0 - sstep(half, half + 5.0, d)
        };
        let k = (p.defringe[0] * gate(p.defringe[1], p.defringe[2])
            + p.defringe[4] * gate(p.defringe[5], p.defringe[6]))
            * sstep(0.05, 0.3, s);
        if k > 0.0 {
            x = hsv_to_rgb(h, s * (1.0 - k).max(0.0), v);
        }
    }
    // split toning: pull shadows/midtones/highlights toward their tint colors
    if p.shadow_sat > 0.0 || p.high_sat > 0.0 || p.mid_sat > 0.0 {
        let luma = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        // LR SplitToningBalance shifts the shadow↔highlight crossover;
        // ColorGradeBlending widens/narrows the overlap (0.5 = LR 50)
        let sp = 0.5 + p.grade_balance * 0.2;
        let bw = 0.7 + p.grade_blend * 0.6;
        let ws = (1.0 - sstep(sp - 0.5 * bw, sp + 0.05 * bw, luma)) * p.shadow_sat;
        let wh = sstep(sp - 0.05 * bw, sp + 0.5 * bw, luma) * p.high_sat;
        let wm = (1.0 - (luma - sp).abs() * 2.0 / bw).max(0.0) * p.mid_sat;
        // colorize semantics (LR): blend toward the tint at the pixel's own
        // luma — additive (col-luma) lifts blacks and creates a floor
        for c in 0..3 {
            x[c] += ws * (p.shadow_col[c] * luma - x[c]) * 0.5
                + wh * (p.high_col[c] * luma - x[c]) * 0.5
                + wm * (p.mid_col[c] * luma - x[c]) * 0.5;
        }
    }
    // HDR zone wheels: per-band [hue, amt, ev, sat]
    for (i, z) in p.zones.iter().enumerate() {
        if z[1] == 0.0 && z[2] == 0.0 && z[3] == 0.0 {
            continue;
        }
        let luma = (0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2]).clamp(0.0, 1.0);
        let w = match i {
            0 => 1.0 - sstep(0.0, 0.18, luma),
            1 => sstep(0.05, 0.25, luma) * (1.0 - sstep(0.25, 0.55, luma)),
            2 => sstep(0.45, 0.75, luma),
            _ => 1.0,
        };
        if w <= 0.0 {
            continue;
        }
        let zc = hue_to_rgb(z[0]);
        let evg = (2.0f32).powf(z[2] * w);
        for c in 0..3 {
            x[c] = x[c] * evg + w * z[1] * (zc[c] - luma) * 0.4;
        }
        if z[3] != 0.0 {
            let l2 = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
            for c in 0..3 {
                x[c] = l2 + (x[c] - l2) * (1.0 + z[3] * w);
            }
        }
    }
    // RGB channel mixer (3x3, row-major)
    if p.mixer != [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        x = [
            p.mixer[0] * x[0] + p.mixer[1] * x[1] + p.mixer[2] * x[2],
            p.mixer[3] * x[0] + p.mixer[4] * x[1] + p.mixer[5] * x[2],
            p.mixer[6] * x[0] + p.mixer[7] * x[1] + p.mixer[8] * x[2],
        ];
    }
    // monochrome conversion (mixer-style luma weights)
    if p.mono[0] != 0.0 || p.mono[1] != 0.0 || p.mono[2] != 0.0 {
        let g = p.mono[0] * x[0] + p.mono[1] * x[1] + p.mono[2] * x[2];
        x = [g, g, g];
    }
    // PV2012-style overrange: channels above white fold toward the luma
    // on an exponential shoulder so hot colors converge to ~white instead
    // of hard-clipping at the curve LUT (mirrors gpu.rs)
    let m = x[0].max(x[1]).max(x[2]);
    if m > 1.0 {
        let l = (0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2]).clamp(0.0, 1.0);
        let h = (1.0 - l).max(0.02);
        for c in &mut x {
            let e = *c - l;
            if e > 0.0 {
                *c = l + h * (1.0 - (-e / h).exp());
            }
        }
    }
    if dbg {
        eprintln!("[adj] pre-lut={x:?}");
    }
    // tone curve lut
    for c in &mut x {
        let idx = (c.clamp(0.0, 1.0) * 255.0) as usize;
        *c = p.lut[idx.min(255)];
    }
    if dbg {
        eprintln!("[adj] post-lut={x:?}");
    }
    // per-channel custom curves
    if !p.chan_luts.is_empty() {
        for c in 0..3 {
            x[c] = lut_at(&p.chan_luts[c * 256..], x[c]);
        }
    }
    // hue-domain curves (DaVinci Hue-vs-* and Lum/Sat-vs-Sat)
    if !p.hue_luts.is_empty() {
        let (h, s, v) = rgb_to_hsv(x);
        let lum = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let h2 = lut_at(&p.hue_luts[0..], h);
        let mut s2 = s * lut_at(&p.hue_luts[256..], h) * lut_at(&p.hue_luts[768..], lum);
        s2 = lut_at(&p.hue_luts[1024..], s2.clamp(0.0, 1.0));
        let l2 = lum * lut_at(&p.hue_luts[512..], lum);
        let mut x2 = hsv_to_rgb(h2, s2.clamp(0.0, 1.0), v);
        let l3 = 0.2126 * x2[0] + 0.7152 * x2[1] + 0.0722 * x2[2];
        if l3 > 1e-5 {
            for c in 0..3 {
                x2[c] *= l2 / l3;
            }
        }
        x = x2;
    }
    // ColorSlice (7 hue wedges) + ColorWarper (local hue/sat displacements).
    // Same math lives in gpu.rs.
    if p.has_cs || !p.warper.is_empty() {
        let (h, s, v) = rgb_to_hsv(x);
        let mut dh = 0.0f32;
        let mut ds = 0.0f32;
        let mut dl = 0.0f32;
        if p.has_cs {
            // hue wedges fade out at low chroma (grey has no hue)
            let cw = sstep(0.04, 0.15, s);
            for (i, sl) in p.cs.iter().enumerate() {
                if sl[3] <= 0.5 {
                    continue;
                }
                let d = hue_dist(h, SLICE_CENTER[i]);
                let w = (1.0 - d * 7.0).max(0.0) * cw;
                dh += sl[0] * w;
                ds += sl[1] * w;
                dl += sl[2] * w;
            }
        }
        for w in p.warper.iter() {
            let hd = hue_dist(h, w[0]);
            let sd = (s - w[1]).abs();
            let e = ((hd / 0.12).powi(2) + (sd / w[4].max(0.03)).powi(2)).sqrt();
            if e < 1.0 {
                let t = 1.0 - e;
                let wgt = t * t * (3.0 - 2.0 * t);
                dh += w[2] * wgt;
                ds += w[3] * wgt;
            }
        }
        if dh != 0.0 || ds != 0.0 || dl != 0.0 {
            x = hsv_to_rgb(
                (h + dh).rem_euclid(1.0),
                (s * (1.0 + ds)).clamp(0.0, 1.5),
                (v * (1.0 + dl)).clamp(0.0, 8.0),
            );
        }
    }
    // LR Point Color swatches: feathered trapezoid selection over
    // hue/sat/lum applied as weighted hue/sat/lum shifts.
    for (i, v19) in p.point_colors.iter().enumerate() {
        if let Some(sw) = pc_normalize(v19) {
            x = pc_apply(x, &sw, p.point_color_vars.get(i).copied().unwrap_or(0.0));
        }
    }
    // HSL qualifier: soft windows on hue/sat/lum, adjust inside the mask.
    // Also runs when only q_show is set so the matte can be previewed.
    if p.has_qual || p.q_show {
        let (h, s, _v) = rgb_to_hsv(x);
        let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
        let mask = qual_mask(x, p);
        // highlight/isolate: desaturate everything outside the key
        if p.q_show {
            for c in 0..3 {
                x[c] = l + (x[c] - l) * mask;
            }
        }
        if mask > 0.001 && p.has_qual {
            let h2 = h + p.qadj[0];
            let s2 = (s * (1.0 + p.qadj[1])).clamp(0.0, 1.0);
            let mut xq = hsv_to_rgb(h2, s2, _v);
            let lq = 0.2126 * xq[0] + 0.7152 * xq[1] + 0.0722 * xq[2];
            if lq > 1e-5 {
                for c in 0..3 {
                    xq[c] *= (l * (1.0 + p.qadj[2])) / lq;
                }
            }
            xq[0] += p.qadj[3] * 0.06;
            xq[2] -= p.qadj[3] * 0.06;
            for c in 0..3 {
                x[c] += (xq[c] - x[c]) * mask;
            }
        }
    }
    // imported .cube LUT: run display-referred (encode -> lut -> decode back
    // to linear so grain/vignette/windows keep working in the linear domain)
    if let Some(cube) = &p.lut3d {
        let enc = [srgb_encode(x[0]), srgb_encode(x[1]), srgb_encode(x[2])];
        let v = cube.sample(enc[0], enc[1], enc[2]);
        for c in 0..3 {
            x[c] = srgb_decode(enc[c] + (v[c] - enc[c]) * p.lut_amt);
        }
    }
    x
}

/// HSL qualifier matte for `x` (hue range / sat range / lum range, blur-dilated
/// edges, black/white clean remap, invert). Shared by the qualifier block and
/// power-window `link_q` gating. Same math lives in gpu.rs `qual_mask`.
pub fn qual_mask(x: [f32; 3], p: &Params) -> f32 {
    let (h, s, _v) = rgb_to_hsv(x);
    let l = 0.2126 * x[0] + 0.7152 * x[1] + 0.0722 * x[2];
    let qb = p.q_blur * 0.25;
    let mh = 1.0
        - sstep(
            p.qh[1],
            p.qh[1] + (p.qh[2] + qb).max(1e-4),
            hue_dist(h, p.qh[0]),
        );
    let qs2 = p.qs[2] + qb;
    let ms =
        sstep(p.qs[0] - qs2, p.qs[0] + qs2, s) * (1.0 - sstep(p.qs[1] - qs2, p.qs[1] + qs2, s));
    let ql2 = p.ql[2] + qb;
    let ml =
        sstep(p.ql[0] - ql2, p.ql[0] + ql2, l) * (1.0 - sstep(p.ql[1] - ql2, p.ql[1] + ql2, l));
    let mut mask = mh * ms * ml;
    let cb = p.q_clean[0];
    let cw = p.q_clean[1];
    if cb > 0.0 || cw < 1.0 {
        mask = ((mask - cb) / (cw - cb).max(1e-4)).clamp(0.0, 1.0);
    }
    if p.q_invert {
        mask = 1.0 - mask;
    }
    mask
}

// ---- color-difference demosaic (full res, stride == 1) ------------------
// G plane first: at non-green sites interpolate G from mirrored green tap
// pairs weighted by inverse gradient (edge-aware, MHC-style), falling back
// to inverse-distance. R/B then come from the smooth difference channels
// (R-G, B-G) interpolated over same-color taps — the assumption AHD/RCD
// share, applied generically to Bayer and X-Trans CFAs via tap tables.

/// per-CFA-phase tap tables (phase = (y%ph)*pw + x%pw); channel index
/// 0=R, 1=G (cells 1|3), 2=B. Shared by the CPU path and the GPU upload.
pub struct DemosaicTaps {
    /// (offset, count) into `taps`, indexed phase*3 + channel
    pub idx: Vec<(u32, u32)>,
    /// (dx, dy) sensor offsets
    pub taps: Vec<(i32, i32)>,
}

pub fn build_demosaic_taps(cfa: &CfaPattern) -> DemosaicTaps {
    let (pw, ph) = (cfa.w, cfa.h);
    let mut idx = vec![(0u32, 0u32); pw * ph * 3];
    let mut taps: Vec<(i32, i32)> = Vec::new();
    for py in 0..ph {
        for px in 0..pw {
            let phase = py * pw + px;
            for ch in 0..3usize {
                let off = taps.len() as u32;
                // 5x5 (Chebyshev 2, corners dropped) covers every Bayer case
                // and most X-Trans; widen only if a phase finds nothing.
                let mut rad = 2i32;
                loop {
                    let r2 = (rad * rad + 1) as f32;
                    let mut found = false;
                    for dy in -rad..=rad {
                        for dx in -rad..=rad {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            if ((dx * dx + dy * dy) as f32) > r2 {
                                continue;
                            }
                            let cell = cfa.cells[(py as i32 + dy).rem_euclid(ph as i32) as usize
                                * pw
                                + (px as i32 + dx).rem_euclid(pw as i32) as usize];
                            let cc = if cell == 3 { 1usize } else { cell as usize };
                            if cc == ch {
                                taps.push((dx, dy));
                                found = true;
                            }
                        }
                    }
                    if found || rad >= 4 {
                        break;
                    }
                    rad += 1;
                }
                idx[phase * 3 + ch] = (off, taps.len() as u32 - off);
            }
        }
    }
    DemosaicTaps { idx, taps }
}

/// full-res demosaic → camera-space linear RGB plane (m.w × m.h)
fn demosaic_plane(m: &Mosaic, norm: &[f32; 4], taps: &DemosaicTaps) -> Vec<[f32; 3]> {
    let (vw, vh) = (m.w, m.h);
    if let Some(lr) = &m.linear {
        // embedded demosaiced data (LR Denoise/SuperRes DNG): direct copy
        // of the visible rect, normalized by the IFD's black/white levels
        let mut out = vec![[0.0f32; 3]; vw * vh];
        for vy in 0..vh {
            let sy = (m.top + vy).min(lr.h - 1);
            for vx in 0..vw {
                let sx = (m.left + vx).min(lr.w - 1);
                let i = sy * lr.w + sx;
                let mut px = [0.0f32; 3];
                for c in 0..3 {
                    if let Some(p) = lr.planes.get(c) {
                        let v = ((p[i] as f32 - lr.black[c]) / (lr.white - lr.black[c]).max(1.0))
                            .max(0.0);
                        // SAFELIGHT_LINEAR_SRGB=1: treat stored values as
                        // sRGB-encoded rather than scene-linear (diagnostic)
                        px[c] = if std::env::var_os("SAFELIGHT_LINEAR_SRGB").is_some() {
                            srgb_decode(v)
                        } else {
                            v
                        };
                    }
                }
                out[vy * vw + vx] = px;
            }
        }
        return out;
    }
    let (rw, rh) = (m.raw_w as i32, m.raw_h as i32);
    let (pw, ph) = (m.cfa.w, m.cfa.h);
    let nv = |sx: i32, sy: i32| -> f32 {
        let cx = sx.clamp(0, rw - 1) as usize;
        let cy = sy.clamp(0, rh - 1) as usize;
        let col = m.cfa.cells[(cy % ph) * pw + (cx % pw)] as usize;
        let ci = col.min(3);
        (m.data[cy * m.raw_w + cx] as f32 - black_at(m, ci)) * norm[ci]
    };
    let is_color = |sx: i32, sy: i32, ch: usize| -> bool {
        let cy = sy.clamp(0, rh - 1) as usize;
        let cx = sx.clamp(0, rw - 1) as usize;
        let col = m.cfa.cells[(cy % ph) * pw + (cx % pw)];
        let cc = if col == 3 { 1usize } else { col as usize };
        cc == ch
    };
    let is_green = |sx: i32, sy: i32| is_color(sx, sy, 1);
    let phase_at = |sx: usize, sy: usize| (sy % ph) * pw + (sx % pw);
    // g plane value at a sensor coordinate (clamped into the visible area)
    let g_at = |g: &[f32], jx: i32, jy: i32| -> f32 {
        let jvx = (jx as usize).clamp(m.left, m.left + vw - 1) - m.left;
        let jvy = (jy as usize).clamp(m.top, m.top + vh - 1) - m.top;
        g[jvy * vw + jvx]
    };

    // pass 1: green plane
    let mut g = vec![0.0f32; vw * vh];
    for vy in 0..vh {
        let sy0 = m.top + vy;
        for vx in 0..vw {
            let sx0 = m.left + vx;
            g[vy * vw + vx] = if is_green(sx0 as i32, sy0 as i32) {
                nv(sx0 as i32, sy0 as i32)
            } else {
                // weighted median of neighbouring greens, weighted by
                // how well the pixel's OWN measured channel agrees with
                // the same-channel tap on each green tap's side — taps
                // on our side of an edge win (no straddled means, which
                // are what made zipper ticks).
                let phase = phase_at(sx0, sy0);
                let col = m.cfa.cells[phase] as usize;
                let own = nv(sx0 as i32, sy0 as i32);
                let (off_g, cnt_g) = taps.idx[phase * 3 + 1];
                let (off_c, cnt_c) = taps.idx[phase * 3 + col];
                let mut cand: [(f32, f32); 24] = [(0.0, 0.0); 24];
                let mut nc = 0usize;
                let mut wtot = 0.0f32;
                for k in off_g..off_g + cnt_g {
                    if nc >= cand.len() {
                        break;
                    }
                    let (dx, dy) = taps.taps[k as usize];
                    let jx = (sx0 as i32 + dx).clamp(0, rw - 1);
                    let jy = (sy0 as i32 + dy).clamp(0, rh - 1);
                    // same-channel tap most aligned with direction j
                    let mut best_dot = 0.0f32;
                    let mut best_val = 0.0f32;
                    let jlen = ((dx * dx + dy * dy) as f32).sqrt();
                    for t in off_c..off_c + cnt_c {
                        let (tx, ty) = taps.taps[t as usize];
                        let dot = (tx * dx + ty * dy) as f32;
                        if dot <= 0.0 {
                            continue;
                        }
                        let tlen = ((tx * tx + ty * ty) as f32).sqrt();
                        let al = dot / (tlen * jlen);
                        if al > best_dot {
                            best_dot = al;
                            best_val = nv(sx0 as i32 + tx, sy0 as i32 + ty);
                        }
                    }
                    let mut w = 1.0 / ((dx * dx + dy * dy) as f32 + 0.5);
                    if best_dot > 0.5 {
                        w /= (best_val - own).abs() + 0.04;
                    }
                    cand[nc] = (nv(jx, jy), w);
                    nc += 1;
                    wtot += w;
                }
                if nc == 0 {
                    0.0
                } else {
                    let c = &mut cand[..nc];
                    c.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                    let mut acc = 0.0f32;
                    let mut dm = c[nc - 1].0;
                    for &(d, w) in c.iter() {
                        acc += w;
                        if acc >= wtot * 0.5 {
                            dm = d;
                            break;
                        }
                    }
                    dm
                }
            };
        }
    }

    // pass 2: difference planes dR = R-G, dB = B-G at every pixel
    // (raw minus G at same-color sites, directional interpolation else)
    let mut dr = vec![0.0f32; vw * vh];
    let mut db = vec![0.0f32; vw * vh];
    for vy in 0..vh {
        let sy0 = m.top + vy;
        for vx in 0..vw {
            let sx0 = m.left + vx;
            let phase = phase_at(sx0, sy0);
            let col = m.cfa.cells[phase];
            let cc = if col == 3 { 1usize } else { col as usize };
            let gv = g[vy * vw + vx];
            let dpl = [&mut dr[..], &mut db[..]];
            for (ci, ch) in [0usize, 2usize].iter().enumerate() {
                let ch = *ch;
                dpl[ci][vy * vw + vx] = if cc == ch {
                    nv(sx0 as i32, sy0 as i32) - gv
                } else {
                    let (off, cnt) = taps.idx[phase * 3 + ch];
                    // weighted median of same-channel C-G diffs, weighted
                    // by green similarity — taps on the same side of an
                    // edge as us have similar greens, so the median snaps
                    // to the correct side instead of averaging across it
                    // (kills zipper ticks on saturated edges).
                    let mut cand: [(f32, f32); 24] = [(0.0, 0.0); 24];
                    let mut nc = 0usize;
                    let mut wtot = 0.0f32;
                    for k in off..off + cnt {
                        let (dx, dy) = taps.taps[k as usize];
                        let jx = (sx0 as i32 + dx).clamp(0, rw - 1);
                        let jy = (sy0 as i32 + dy).clamp(0, rh - 1);
                        let dj = nv(jx, jy) - g_at(&g, jx, jy);
                        let w = 1.0
                            / ((g_at(&g, jx, jy) - gv).abs() + 0.04)
                            / ((dx * dx + dy * dy) as f32 + 0.5);
                        if nc < cand.len() {
                            cand[nc] = (dj, w);
                            nc += 1;
                            wtot += w;
                        }
                    }
                    if nc == 0 {
                        0.0
                    } else {
                        // weighted median: smallest d whose cumulative
                        // weight (sorted by d) reaches half the total
                        let n = nc;
                        let c = &mut cand[..n];
                        c.sort_by(|a, b| {
                            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        let mut acc = 0.0f32;
                        let mut dm = c[n - 1].0;
                        for &(d, w) in c.iter() {
                            acc += w;
                            if acc >= wtot * 0.5 {
                                dm = d;
                                break;
                            }
                        }
                        dm
                    }
                };
            }
        }
    }

    // pass 3: reconstruct — G + 5x5 median of the difference plane.
    // isolated zipper ticks are 1-px outliers in the (smooth) diff
    // channel; the median kills them while real colour detail — which
    // is coherent across several pixels — survives.
    let mut out = vec![[0.0f32; 3]; vw * vh];
    for vy in 0..vh {
        let sy0 = m.top + vy;
        for vx in 0..vw {
            let sx0 = m.left + vx;
            let phase = phase_at(sx0, sy0);
            let col = m.cfa.cells[phase];
            let cc = if col == 3 { 1usize } else { col as usize };
            let gv = g[vy * vw + vx];
            let mut px = [0.0f32; 3];
            px[1] = gv;
            for (ci, ch) in [0usize, 2usize].iter().enumerate() {
                let ch = *ch;
                let dp = if ci == 0 { &dr } else { &db };
                px[ch] = if cc == ch {
                    nv(sx0 as i32, sy0 as i32)
                } else {
                    let mut nb = [0.0f32; 25];
                    let mut n = 0usize;
                    for oy in -2i32..=2 {
                        for ox in -2i32..=2 {
                            let jx = (vx as i32 + ox).clamp(0, vw as i32 - 1) as usize;
                            let jy = (vy as i32 + oy).clamp(0, vh as i32 - 1) as usize;
                            nb[n] = dp[jy * vw + jx];
                            n += 1;
                        }
                    }
                    nb[..n].sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    gv + nb[n / 2]
                };
            }
            out[vy * vw + vx] = px;
        }
    }
    out
}

// ---- lens profile correction (lensfun DB) ---------------------------------
// distortion: given display radius ru (lens-normalised), find the recorded
// radius rd such that f(rd) = ru, by Newton iteration. lensfun defines the
// polynomial as distorted→undistorted, so rendering needs the inverse.

fn lens_f(model: u32, abc: [f32; 3], rd: f32) -> f32 {
    if model == 1 {
        let (a, b, c) = (abc[0], abc[1], abc[2]);
        rd * (a * rd * rd * rd + b * rd * rd + c * rd + (1.0 - a - b - c))
    } else {
        rd * (1.0 + abc[0] * rd * rd)
    }
}
fn lens_fp(model: u32, abc: [f32; 3], rd: f32) -> f32 {
    if model == 1 {
        let (a, b, c) = (abc[0], abc[1], abc[2]);
        4.0 * a * rd * rd * rd + 3.0 * b * rd * rd + 2.0 * c * rd + (1.0 - a - b - c)
    } else {
        1.0 + 3.0 * abc[0] * rd * rd
    }
}
/// Adobe ACM: rd = ru·(1 + k1 ru² + k2 ru⁴ + k3 ru⁶) — forward eval,
/// no inversion needed (model maps corrected→distorted directly).
fn adobe_fwd(abc: [f32; 3], ru: f32) -> f32 {
    let r2 = ru * ru;
    ru * (1.0 + abc[0] * r2 + abc[1] * r2 * r2 + abc[2] * r2 * r2 * r2)
}

/// model 3 (Adobe ACM): coefficients are defined on the focal-unit radius
/// r = dist_px/(foc·Dmax); convert via s = halfd_px/(foc·Dmax).
/// `foc` = normalized focal (f35mm/35); pass 0 for lensfun models.
fn inv_dist(model: u32, abc: [f32; 3], ru: f32, s: f32) -> f32 {
    if model == 0 || ru <= 0.0 {
        return ru;
    }
    if model == 3 {
        let s = s.max(1e-3);
        return (adobe_fwd(abc, ru * s) / s).max(0.0);
    }
    let mut rd = ru;
    for _ in 0..5 {
        let f = lens_f(model, abc, rd) - ru;
        let fp = lens_fp(model, abc, rd).max(1e-4);
        rd -= f / fp;
        if rd < 0.0 {
            rd = ru * 0.5;
        }
    }
    rd
}
/// TCA poly2 scale: ru = rd * (v + b rd^2) — inverse via the same solver.
fn inv_tca(v: f32, b: f32, ru: f32) -> f32 {
    if ru <= 0.0 || (v - 1.0).abs() < 1e-6 && b.abs() < 1e-6 {
        return ru;
    }
    let mut rd = ru;
    for _ in 0..5 {
        let f = rd * (v + b * rd * rd) - ru;
        let fp = (v + 3.0 * b * rd * rd).max(1e-4);
        rd -= f / fp;
        if rd < 0.0 {
            rd = ru * 0.5;
        }
    }
    rd
}

/// Adobe Camera Model path (.lcp): full 2-D corrected→distorted warp with
/// radial + tangential terms, per-channel chromatic warps on the distorted
/// green position, and residual-illumination vignette on distorted
/// focal-unit coords. Mirrors RawTherapee's lcp.cc implementation.
fn correct_plane_adobe(
    plane: &[[f32; 3]],
    w: usize,
    h: usize,
    a: &crate::lensdb::AdobeCor,
    dng: Option<&crate::dngops::DngOps>,
    amount: f32,
) -> Vec<[f32; 3]> {
    let dmax = w.max(h) as f32;
    let x0 = a.center[0] * w as f32;
    let y0 = a.center[1] * h as f32;
    let fx = a.foc[0] * dmax;
    let fy = a.foc[1] * dmax;
    let vx0 = a.vig_center[0] * w as f32;
    let vy0 = a.vig_center[1] * h as f32;
    let vfx = a.vig_foc[0].max(1e-3) * dmax;
    let vfy = a.vig_foc[1].max(1e-3) * dmax;
    let has_vig = a.vig_a != [0.0; 3];
    let (k1, k2, k3, ty, tx) = (a.k[0], a.k[1], a.k[2], a.k[3], a.k[4]);
    let halfd = ((w as f32 * 0.5).powi(2) + (h as f32 * 0.5).powi(2))
        .sqrt()
        .max(1.0);
    let mut out = vec![[0.0f32; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let xd = (x as f32 - x0) / fx;
            let yd = (y as f32 - y0) / fy;
            let rsqr = xd * xd + yd * yd;
            // forward ACM warp: corrected → distorted (focal units)
            let cf = 1.0 + rsqr * (k1 + rsqr * (k2 + rsqr * k3)) + 2.0 * (ty * yd + tx * xd);
            let xg = xd * cf + tx * rsqr;
            let yg = yd * cf + ty * rsqr;
            // blend toward identity by amount
            let xg = xd + (xg - xd) * amount;
            let yg = yd + (yg - yd) * amount;
            let mut px = [0.0f32; 3];
            for ch in 0..3usize {
                let m = match ch {
                    0 => a.chr,
                    2 => a.chb,
                    _ => None,
                };
                let (xc, yc) = match m {
                    Some(m) => {
                        let r2 = xg * xg + yg * yg;
                        let cs = 1.0
                            + r2 * (m[1] + r2 * (m[2] + r2 * m[3]))
                            + 2.0 * (m[4] * yg + m[5] * xg);
                        let xc = m[0] * (xg * cs + m[5] * r2);
                        let yc = m[0] * (yg * cs + m[4] * r2);
                        (xg + (xc - xg) * amount, yg + (yc - yg) * amount)
                    }
                    None => (xg, yg),
                };
                let mut sx = x0 + xc * fx;
                let mut sy = y0 + yc * fy;
                if let Some(ops) = dng {
                    if let Some(wr) = &ops.warp {
                        // WarpRectilinear: halfdiag-normalised about the
                        // embedded centre, per-channel kr
                        let wx = wr.center[0] * w as f32;
                        let wy = wr.center[1] * h as f32;
                        let ox = (sx - wx) / halfd;
                        let oy = (sy - wy) / halfd;
                        let r2 = ox * ox + oy * oy;
                        let k = &wr.kr[ch.min(2)];
                        let f = k[0]
                            + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * (k[4] + r2 * k[5]))));
                        let f = 1.0 + (f - 1.0) * amount;
                        sx = wx + ox * f * halfd;
                        sy = wy + oy * f * halfd;
                    }
                    let mut v = bilinear3(plane, w, h, sx, sy, ch);
                    if let Some(gm) = &ops.gain {
                        let gg = gm.at(sx as f64, sy as f64, ch, w, h) as f32;
                        v *= 1.0 + (gg - 1.0) * amount;
                    }
                    px[ch] = v;
                } else {
                    px[ch] = bilinear3(plane, w, h, sx, sy, ch);
                }
            }
            if has_vig {
                // residual illumination on the distorted source position,
                // in the vignette model's own frame
                let sxd = x0 + xg * fx;
                let syd = y0 + yg * fy;
                let xdv = (sxd - vx0) / vfx;
                let ydv = (syd - vy0) / vfy;
                let rv2 = xdv * xdv + ydv * ydv;
                let att = (1.0
                    + a.vig_a[0] * rv2
                    + a.vig_a[1] * rv2 * rv2
                    + a.vig_a[2] * rv2 * rv2 * rv2)
                    .max(0.05);
                let g = 1.0 + (1.0 / att - 1.0) * amount;
                for ch in px.iter_mut() {
                    *ch *= g;
                }
            }
            out[i] = px;
        }
    }
    out
}

/// lens-correction stage for this frame: external profile (lensfun / Adobe
/// LCP) merged with any DNG-embedded opcode corrections (GainMap +
/// WarpRectilinear). Returns None when nothing applies.
pub(crate) fn lens_stage(m: &Mosaic, r: &Recipe) -> Option<crate::lensdb::Correction> {
    if r.lens_corr <= 0.001 {
        return None;
    }
    // a preset may pin a specific lens profile (crs:LensProfileName);
    // try it first, fall back to the EXIF lens.
    let mut c = if !r.lens_name.is_empty() {
        crate::lensdb::correction_for(
            &r.lens_name,
            &m.info.make,
            &m.info.model,
            m.info.focal,
            m.info.aperture,
        )
        .or_else(|| {
            crate::lensdb::correction_for(
                &m.info.lens,
                &m.info.make,
                &m.info.model,
                m.info.focal,
                m.info.aperture,
            )
        })
    } else {
        crate::lensdb::correction_for(
            &m.info.lens,
            &m.info.make,
            &m.info.model,
            m.info.focal,
            m.info.aperture,
        )
    }
    .unwrap_or_else(crate::lensdb::Correction::none);
    c.dng = m.dng_ops.clone();
    crate::dngops::env_filter(&mut c.dng);
    if c.dng.is_some() && std::env::var_os("SAFELIGHT_VERBOSE").is_some() {
        eprintln!(
            "[dng] embedded ops: warp={} gain={:?}",
            c.dng.as_ref().unwrap().warp.is_some(),
            c.dng
                .as_ref()
                .unwrap()
                .gain
                .as_ref()
                .map(|g| (g.rows, g.cols, g.planes))
        );
    }
    if c.is_empty() && c.dng.is_none() {
        None
    } else {
        Some(c)
    }
}

/// geometric remap + vignette compensation for the cam-space plane.
/// Returns (corrected plane, per-pixel gain). Bilinear per channel —
/// lateral CA gives R and B their own source radii.
pub fn correct_plane(
    plane: &[[f32; 3]],
    w: usize,
    h: usize,
    c: &crate::lensdb::Correction,
    amount: f32,
) -> Vec<[f32; 3]> {
    if let Some(a) = &c.adobe {
        return correct_plane_adobe(plane, w, h, a, c.dng.as_ref(), amount);
    }
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let halfd = (cx * cx + cy * cy).sqrt().max(1.0);
    // focal-unit scale for Adobe ACM: halfd_px/(foc·Dmax)
    let dmax = w.max(h) as f32;
    let lens_s = halfd
        / (c.adobe
            .as_ref()
            .map(|a| a.foc[0])
            .unwrap_or(c.vig_foc)
            .max(1e-3)
            * dmax);
    let warp = c.dng.as_ref().and_then(|o| o.warp.as_ref());
    let gain = c.dng.as_ref().and_then(|o| o.gain.as_ref());
    let mut out = vec![[0.0f32; 3]; w * h];
    let vig_on = c.vig != [0.0; 3];
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let rn = (dx * dx + dy * dy).sqrt() / halfd;
            let i = y * w + x;
            if rn < 1e-6 {
                out[i] = plane[i];
                continue;
            }
            let (ux, uy) = (dx / (rn * halfd), dy / (rn * halfd));
            let ru = rn * c.scale; // lens-normalised display radius
            let rd = inv_dist(c.model, c.abc, ru, lens_s);
            let mut px = [0.0f32; 3];
            for ch in 0..3usize {
                let (v, b) = match ch {
                    0 => (c.tca[0], c.tca[1]),
                    1 => (1.0, 0.0),
                    _ => (c.tca[2], c.tca[3]),
                };
                // Adobe TCA is a forward warp on rd (fitted v+b·r²);
                // lensfun TCA is rd→ru and needs the inverse
                let rd_ch = if c.model == 3 {
                    // fitted Adobe TCA warp is defined on focal units
                    let s2 = lens_s * lens_s;
                    rd * (v + b * rd * rd * s2) / c.scale
                } else {
                    inv_tca(v, b, rd) / c.scale
                };
                // blend corrected position with identity by amount
                let r_px = (rd_ch + (rn - rd_ch) * (1.0 - amount)) * halfd;
                let (mut sx, mut sy) = (cx + ux * r_px, cy + uy * r_px);
                // DNG WarpRectilinear: ideal -> warped in halfdiag-normalized
                // coords about the embedded center (per-channel kr)
                if let Some(wr) = warp {
                    let wx = wr.center[0] * w as f32;
                    let wy = wr.center[1] * h as f32;
                    let ox = (sx - wx) / halfd;
                    let oy = (sy - wy) / halfd;
                    let r2 = ox * ox + oy * oy;
                    let k = &wr.kr[ch.min(2)];
                    let f =
                        k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * (k[4] + r2 * k[5]))));
                    let f = 1.0 + (f - 1.0) * amount;
                    sx = wx + ox * f * halfd;
                    sy = wy + oy * f * halfd;
                }
                px[ch] = bilinear3(plane, w, h, sx, sy, ch);
                // DNG GainMap: illumination gain at the source position
                if let Some(gm) = gain {
                    let g = gm.at(sx as f64, sy as f64, ch, w, h) as f32;
                    px[ch] *= 1.0 + (g - 1.0) * amount;
                }
            }
            if vig_on {
                // Adobe vignette poly is defined on the distorted radius
                // (coefficients pre-scaled into display-normalised units);
                // lensfun PA uses the corrected (display) radius.
                let rv = if c.vig_adobe { rd } else { ru };
                let r2 = rv * rv;
                let att =
                    (1.0 + c.vig[0] * r2 + c.vig[1] * r2 * r2 + c.vig[2] * r2 * r2 * r2).max(0.05);
                let g = 1.0 + (1.0 / att - 1.0) * amount;
                for ch in px.iter_mut() {
                    *ch *= g;
                }
            }
            out[i] = px;
        }
    }
    out
}

/// map an output pixel to its source position + vignette gain
/// (single-channel green path — preview quality is fine without TCA)
pub fn lens_map(
    c: &crate::lensdb::Correction,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    amount: f32,
    stride: usize,
) -> (f32, f32, f32) {
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let halfd = (cx * cx + cy * cy).sqrt().max(1.0);
    let dx = x as f32 - cx;
    let dy = y as f32 - cy;
    let rn = (dx * dx + dy * dy).sqrt() / halfd;
    let (mut sx, mut sy) = (x as f32, y as f32);
    let mut g = 1.0f32;
    if rn >= 1e-6 {
        let ru = rn * c.scale;
        let dmax = w.max(h) as f32;
        let lens_s = halfd
            / (c.adobe
                .as_ref()
                .map(|a| a.foc[0])
                .unwrap_or(c.vig_foc)
                .max(1e-3)
                * dmax);
        let rd = inv_dist(c.model, c.abc, ru, lens_s) / c.scale;
        let r_px = (rd + (rn - rd) * (1.0 - amount)) * halfd;
        g = if c.vig != [0.0; 3] {
            let rv = if c.vig_adobe {
                // distorted radius in focal units: dist_px/(vig_foc·Dmax)
                let dmax = w.max(h) as f32;
                rd * halfd / (c.vig_foc.max(1e-3) * dmax)
            } else {
                ru
            };
            let r2 = rv * rv;
            let att =
                (1.0 + c.vig[0] * r2 + c.vig[1] * r2 * r2 + c.vig[2] * r2 * r2 * r2).max(0.05);
            1.0 + (1.0 / att - 1.0) * amount
        } else {
            1.0
        };
        // src = centre + unit_direction * corrected_radius
        let s = r_px / (rn * halfd);
        sx = cx + dx * s;
        sy = cy + dy * s;
    }
    if let Some(ops) = &c.dng {
        if let Some(wr) = &ops.warp {
            let wx = wr.center[0] * w as f32;
            let wy = wr.center[1] * h as f32;
            let ox = (sx - wx) / halfd;
            let oy = (sy - wy) / halfd;
            let r2 = ox * ox + oy * oy;
            let k = &wr.kr[1];
            let f = k[0] + r2 * (k[1] + r2 * (k[2] + r2 * (k[3] + r2 * (k[4] + r2 * k[5]))));
            let f = 1.0 + (f - 1.0) * amount;
            sx = wx + ox * f * halfd;
            sy = wy + oy * f * halfd;
        }
        if let Some(gm) = &ops.gain {
            // map pixels live in full-res image coords — scale the virtual pos
            let gg = gm.at(
                sx as f64 * stride as f64,
                sy as f64 * stride as f64,
                1,
                w * stride,
                h * stride,
            ) as f32;
            g *= 1.0 + (gg - 1.0) * amount;
        }
    }
    (sx, sy, g)
}

fn bilinear3(plane: &[[f32; 3]], w: usize, h: usize, sx: f32, sy: f32, ch: usize) -> f32 {
    let x0 = sx.floor().clamp(0.0, (w - 1) as f32) as usize;
    let y0 = sy.floor().clamp(0.0, (h - 1) as f32) as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (sx - x0 as f32).clamp(0.0, 1.0);
    let ty = (sy - y0 as f32).clamp(0.0, 1.0);
    let a = plane[y0 * w + x0][ch] + tx * (plane[y0 * w + x1][ch] - plane[y0 * w + x0][ch]);
    let b = plane[y1 * w + x0][ch] + tx * (plane[y1 * w + x1][ch] - plane[y1 * w + x0][ch]);
    a + ty * (b - a)
}

/// generic same-colour-mean demosaic (works for bayer & xtrans).
/// Each virtual pixel covers a `stride`x`stride` block of sensor pixels at
/// (left + vx*stride, top + vy*stride). The CFA phase is computed on raw
/// sensor coordinates, which is what libraw's filters/xtrans refer to.
/// When a channel has no samples inside the block (small stride), the
/// window is widened until every channel is covered.
fn demosaic_pixel(m: &Mosaic, vx: usize, vy: usize, stride: usize, norm: &[f32; 4]) -> [f32; 3] {
    if let Some(lr) = &m.linear {
        let x0 = m.left + vx * stride;
        let y0 = m.top + vy * stride;
        let xe = (x0 + stride).min(lr.w.min(m.left + m.w));
        let ye = (y0 + stride).min(lr.h.min(m.top + m.h));
        let mut out = [0.0f32; 3];
        let mut n = 0u32;
        for sy in y0..ye {
            for sx in x0..xe {
                let i = sy * lr.w + sx;
                for c in 0..3 {
                    if let Some(p) = lr.planes.get(c) {
                        out[c] += (p[i] as f32 - lr.black[c])
                            / (lr.white - lr.black[c]).max(1.0);
                    }
                }
                n += 1;
            }
        }
        if n > 0 {
            let srgb_in = std::env::var_os("SAFELIGHT_LINEAR_SRGB").is_some();
            for c in &mut out {
                *c = (*c / n as f32).max(0.0);
                if srgb_in {
                    *c = srgb_decode(*c);
                }
            }
        }
        return out;
    }
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
                sums[cc] += (raw - black_at(m, col.min(3))) * norm[col.min(3)];
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
pub(crate) fn norm_factors(m: &Mosaic) -> [f32; 4] {
    let mut n = [0.0f32; 4];
    for c in 0..4 {
        // libraw's linear_max is the real per-channel saturation point when
        // known (e.g. Sony ARW: 15360 < maximum 16383); normalizing by the
        // advertised maximum would under-expose by ~7%.
        let white = if m.linear_max[c] > m.black[c] as u32 {
            (m.linear_max[c] as f32).min(m.maximum as f32)
        } else {
            m.maximum as f32
        };
        let denom = (white - black_at(m, c)).max(1.0);
        n[c] = 1.0 / denom;
    }
    n
}

/// diagnostic knob: SAFELIGHT_BLACK_BIAS=delta subtracts extra DN on top of
/// the reported black point — probes effective-black differences vs ACR.
fn black_at(m: &Mosaic, c: usize) -> f32 {
    m.black[c.min(3)]
        + std::env::var("SAFELIGHT_BLACK_BIAS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0)
}

/// Full development: decoded -> rgba8 oriented, fit inside max_px (0 = full res).
pub fn develop_cpu(d: &Decoded, r: &Recipe, max_px: u32) -> RgbaImage {
    develop_cpu_ctx(d, r, max_px, None, None)
}

/// full-res linear image for the AI denoise pre-pass: demosaic -> lens corr ->
/// WB -> cam->sRGB matrix (stride 1). `r` supplies WB/lens_corr as they stood
/// when the denoise was baked. Returns (lin, w, h).
pub fn lin_base(d: &Decoded, r: &Recipe) -> Result<(Vec<[f32; 3]>, usize, usize), String> {
    match d {
        Decoded::Mosaic(m) => {
            let stride = 1usize;
            let (vw, vh) = (m.w, m.h);
            let norm = norm_factors(m);
            let p = build_params(m, r, None);
            let plane = demosaic_plane(m, &norm, &build_demosaic_taps(&m.cfa));
            let corr = lens_stage(m, r);
            let plane = match corr.as_ref() {
                Some(c) => correct_plane(&plane, vw, vh, c, r.lens_corr.min(1.0)),
                None => plane,
            };
            Ok((
                colorize_plane(m, stride, &norm, &p, Some(&plane), corr.as_ref(), vw, vh),
                vw,
                vh,
            ))
        }
        Decoded::Raster { rgba, w, h, .. } => {
            let mut lin = vec![[0.0f32; 3]; w * h];
            for (i, px) in rgba.chunks_exact(4).enumerate() {
                for c in 0..3 {
                    lin[i][c] = srgb_of(px[c]);
                }
            }
            Ok((lin, *w, *h))
        }
    }
}

/// denoised base produced by the AI pass (SCUNet), stored sRGB-encoded u16 at
/// the lin-plane resolution. `None` when no cache exists for the file.
pub struct DenCache {
    pub data: Vec<u16>, // sRGB-encoded RGB16, w*h*3
    pub w: usize,
    pub h: usize,
}

/// AI subject matte (U-2-Net), stored u16 luma at a modest resolution —
/// sampled bilinear in normalized coords, so any size works.
#[derive(Debug, Clone)]
pub struct Matte {
    pub data: Vec<u16>, // u16 luma, w*h
    pub w: usize,
    pub h: usize,
}

/// LR-computed AI mask bitmap (catalog .lrcat-data blob) bound to a window:
/// the matte lives in LR's mask space (WholeImageArea, sensor orientation)
/// and the TIFF covers the mask's bounding box at Origin. The affine maps
/// post-crop normalized coords → bitmap-normalized (u,v):
/// u = nx*ux + ny*uy + uc, v = nx*vx + ny*vy + vc.
/// decoded PatchMatch heal map: `src` = per-patch-px absolute source
/// position (lin/virtual-src px), `dst` = target rect, `ell` = feather
/// ellipse, all in lin px.
#[derive(Debug, Clone)]
pub struct PmMap {
    pub src: Vec<[f32; 2]>,
    pub pw: usize,
    pub ph: usize,
    pub dst: [f32; 4],
    pub ell: [f32; 4],
    pub opacity: f32,
}

#[derive(Debug, Clone)]
pub struct WMatte {
    pub m: Matte,
    pub ux: f32,
    pub uy: f32,
    pub uc: f32,
    pub vx: f32,
    pub vy: f32,
    pub vc: f32,
}

pub fn develop_cpu_ctx(
    d: &Decoded,
    r: &Recipe,
    max_px: u32,
    den: Option<&DenCache>,
    subj: Option<&Matte>,
) -> RgbaImage {
    match d {
        Decoded::Mosaic(m) => develop_mosaic(m, r, max_px, den, subj),
        Decoded::Raster {
            rgba, w, h, flip, ..
        } => develop_raster(rgba, *w, *h, *flip, r, max_px, den, subj),
    }
}

fn develop_mosaic(
    m: &Mosaic,
    r: &Recipe,
    max_px: u32,
    den: Option<&DenCache>,
    subj: Option<&Matte>,
) -> RgbaImage {
    // virtual stride: subsample to roughly fit max_px, keeping CFA phase
    let period = m.cfa.h.max(1);
    let mut stride = 1usize;
    if max_px > 0 {
        let ratio = (m.w.max(m.h) as f32 / max_px as f32).max(1.0);
        // ratio < period: real (chroma-diff) demosaic — a period-multiple
        // stride would silently halve resolution even at full-size requests
        stride = if ratio < period as f32 {
            1
        } else {
            ((ratio / period as f32).floor() as usize).max(1) * period
        };
    }
    let vw = m.w / stride;
    let vh = m.h / stride;
    let norm = norm_factors(m);

    // sparse scene statistics for auto WB / auto exposure / auto contrast
    let stats = if Stats::needs(r) {
        let mut s = Stats::default();
        let mut acc = [0.0f64; 3];
        let mut luma = 0.0f64;
        let step = 4usize; // sparse sample
                           // WB pick rectangle (virtual src px); empty when not picking
        let pr = if r.wb_mode == WbMode::Pick {
            pick_rect(r.wb_pick, vw, vh, m.info.flip, r.wb_pick_size)
        } else {
            [-1.0; 4]
        };
        let mut pacc = [0.0f64; 3];
        let mut pcnt = 0u64;
        for vy in (0..vh).step_by(step) {
            for vx in (0..vw).step_by(step) {
                let c = demosaic_pixel(m, vx, vy, stride, &norm);
                for ch in 0..3 {
                    acc[ch] += c[ch] as f64;
                }
                let l = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0);
                luma += l as f64;
                s.luma_hist[(l * 255.0) as usize] += 1;
                s.count += 1;
                if vx as f32 >= pr[0]
                    && vx as f32 <= pr[2]
                    && vy as f32 >= pr[1]
                    && vy as f32 <= pr[3]
                {
                    for ch in 0..3 {
                        pacc[ch] += c[ch] as f64;
                    }
                    pcnt += 1;
                }
            }
        }
        if pcnt > 0 {
            s.pick_means = [
                (pacc[0] / pcnt as f64) as f32,
                (pacc[1] / pcnt as f64) as f32,
                (pacc[2] / pcnt as f64) as f32,
            ];
            s.pick_count = pcnt;
        }
        s.means = [
            (acc[0] / s.count as f64) as f32,
            (acc[1] / s.count as f64) as f32,
            (acc[2] / s.count as f64) as f32,
        ];
        s.luma_mean = (luma / s.count as f64) as f32;
        if std::env::var_os("SAFELIGHT_STATS").is_some() {
            eprintln!(
                "[stats] means={:?} luma_mean={:.4} count={}",
                s.means, s.luma_mean, s.count
            );
            eprintln!(
                "[stats] p1={:.4} p50={:.4} p99={:.4}",
                s.luma_percentile(0.01),
                s.luma_percentile(0.5),
                s.luma_percentile(0.99)
            );
        }
        Some(s)
    } else {
        None
    };
    let p = build_params(m, r, stats.as_ref());

    // full-res (stride 1) gets the color-difference demosaic — the real
    // per-pixel interpolation; stride>1 previews keep the block mean,
    // which is the correct downsample anyway
    let plane = if stride == 1 {
        Some(demosaic_plane(m, &norm, &build_demosaic_taps(&m.cfa)))
    } else {
        None
    };
    // lens profile correction remaps the cam plane. Full-res remaps the
    // demosaiced plane with bilinear per-channel sampling (TCA); previews
    // remap the virtual sampling position instead, so the distortion is
    // visible without paying for a second full-res plane.
    let corr = lens_stage(m, r);
    let plane = match (plane, &corr) {
        (Some(pl), Some(c)) => Some(correct_plane(&pl, vw, vh, c, r.lens_corr.min(1.0))),
        (pl, _) => pl,
    };

    let mut lin = colorize_plane(
        m,
        stride,
        &norm,
        &p,
        plane.as_deref(),
        corr.as_ref(),
        vw,
        vh,
    );
    blend_denoise(&mut lin, vw, vh, r.ai_denoise, den);

    finish_linear(lin, vw, vh, &p, m.info.flip, max_px, subj)
}

/// camera-plane -> WB -> camera->sRGB matrix. Shared by the render path and
/// the AI-denoise pre-pass so both see the identical linear image.
pub fn colorize_plane(
    m: &Mosaic,
    stride: usize,
    norm: &[f32; 4],
    p: &Params,
    plane: Option<&[[f32; 3]]>,
    corr: Option<&crate::lensdb::Correction>,
    vw: usize,
    vh: usize,
) -> Vec<[f32; 3]> {
    let mut lin = vec![[0.0f32; 3]; vw * vh];
    for vy in 0..vh {
        for vx in 0..vw {
            let cam = match plane {
                Some(pl) => pl[vy * vw + vx],
                None => match corr {
                    Some(c) => {
                        let (sx, sy, g) = lens_map(c, vx, vy, vw, vh, p.lens_corr_amt, stride);
                        let mut cc = demosaic_pixel(
                            m,
                            (sx.round() as usize).min(vw - 1),
                            (sy.round() as usize).min(vh - 1),
                            stride,
                            norm,
                        );
                        for v in cc.iter_mut() {
                            *v *= g;
                        }
                        cc
                    }
                    None => demosaic_pixel(m, vx, vy, stride, norm),
                },
            };
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
            // DNG ProfileHueSatMap: per-hue sat/val shifts in the profile
            // working space (ProPhoto), AFTER the camera->working matrix —
            // dng_render.cpp order: ABCDtoRGB -> HueSatMap -> exposure ->
            // LookTable. Applying it in camera space (pre-matrix) samples
            // the map at the wrong hue/sat coordinates.
            let out = match &p.profile_hsm {
                Some(h) => h.apply(out),
                None => out,
            };
            lin[vy * vw + vx] = out;
        }
    }
    lin
}

/// blend the AI-denoised base into the linear image. The cache stores the
/// denoised lin-plane sRGB-encoded; it is sampled bilinear in normalized
/// coordinates so it lines up at any preview stride.
pub fn blend_denoise(
    lin: &mut [[f32; 3]],
    vw: usize,
    vh: usize,
    amount: f32,
    den: Option<&DenCache>,
) {
    let (den, a) = match (den, amount) {
        (Some(d), a) if a > 0.001 && d.data.len() >= d.w * d.h * 3 => (d, a.min(1.0)),
        _ => return,
    };
    let (dw, dh) = (den.w as f32, den.h as f32);
    for y in 0..vh {
        let fy = ((y as f32 + 0.5) / vh as f32) * dh - 0.5;
        let y0 = fy.floor().max(0.0).min(dh - 1.0) as usize;
        let y1 = (y0 + 1).min(den.h - 1);
        let ty = fy - y0 as f32;
        for x in 0..vw {
            let fx = ((x as f32 + 0.5) / vw as f32) * dw - 0.5;
            let x0 = fx.floor().max(0.0).min(dw - 1.0) as usize;
            let x1 = (x0 + 1).min(den.w - 1);
            let tx = fx - x0 as f32;
            let i = y * vw + x;
            for c in 0..3 {
                let s00 = srgb_of(den.data[(y0 * den.w + x0) * 3 + c]);
                let s10 = srgb_of(den.data[(y0 * den.w + x1) * 3 + c]);
                let s01 = srgb_of(den.data[(y1 * den.w + x0) * 3 + c]);
                let s11 = srgb_of(den.data[(y1 * den.w + x1) * 3 + c]);
                let d =
                    (s00 * (1.0 - tx) + s10 * tx) * (1.0 - ty) + (s01 * (1.0 - tx) + s11 * tx) * ty;
                lin[i][c] = lin[i][c] * (1.0 - a) + d * a;
            }
        }
    }
}

#[inline]
fn srgb_of(v: u16) -> f32 {
    let s = v as f32 / 65535.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn develop_raster(
    rgba: &[u16],
    w: usize,
    h: usize,
    flip: i32,
    r: &Recipe,
    max_px: u32,
    den: Option<&DenCache>,
    subj: Option<&Matte>,
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
        baseline_exposure: 0.0,
        linear_max: [0; 4],
        cam_mul: [1.0; 4],
        cam_xyz: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]],
        rgb_cam: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
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
        dng_ops: None,
        linear: None,
    };
    // raster: histogram on the decoded image feeds auto-exposure/contrast
    let stats = if Stats::needs(r) {
        let mut s = Stats::default();
        for px in rgba.chunks_exact(4).step_by(4) {
            let l =
                (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 65535.0;
            s.luma_hist[(l.clamp(0.0, 1.0) * 255.0) as usize] += 1;
            s.luma_mean += l;
            s.count += 1;
        }
        s.luma_mean /= (s.count.max(1)) as f32;
        s.means = [s.luma_mean; 3];
        Some(s)
    } else {
        None
    };
    // raster input is already display-rendered: LR applies no raw
    // tone-scale compensation to it, so build params with pv=0 (skips the
    // PV2012 compensation blend while keeping the recipe's own curve)
    let mut rr = r.clone();
    rr.process_version = 0.0;
    let mut p = build_params(&fake, &rr, stats.as_ref());
    // pm coords for a raster (e.g. enhanced DNG) live in its own pixel frame
    p.pm_frame = [w as f32, h as f32];
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
    blend_denoise(&mut lin, w, h, r.ai_denoise, den);
    finish_linear(lin, w, h, &p, flip, max_px, subj)
}

/// frame-pixel dims + crop rect + rotation pivot shared by CPU/GPU.
/// returns (fw, fh, crop l/t in px, crop w/h in px, dw, dh)
pub(crate) fn frame_geometry(
    w: usize,
    h: usize,
    flip: i32,
    crop: [f32; 4],
    max_px: u32,
) -> (usize, usize, f32, f32, f32, f32, usize, usize) {
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let cl = crop[0] * fw as f32;
    let ct = crop[1] * fh as f32;
    let ew = (fw as f32 * (1.0 - crop[0] - crop[2])).max(1.0);
    let eh = (fh as f32 * (1.0 - crop[1] - crop[3])).max(1.0);
    let (dw, dh) = if max_px > 0 {
        fit(ew as u32, eh as u32, max_px)
    } else {
        (ew.round().max(1.0) as u32, eh.round().max(1.0) as u32)
    };
    (
        fw,
        fh,
        cl,
        ct,
        ew,
        eh,
        dw.max(1) as usize,
        dh.max(1) as usize,
    )
}

/// spot heal on the demosaiced buffer: replace each spot's interior with the
/// mean of a ring sampled just outside its radius (dust/blemish removal).
/// spots are [cx,cy,r] in normalized POST-FLIP frame coords.
/// convert a normalized post-flip-frame spot [cx,cy,r] to virtual-src px
/// (sx, sy, radius_px) — shared by CPU heal and the GPU uniform builder.
/// normalized post-flip frame point → virtual-src px (shared by spots/clones/pick)
pub fn frame_to_src(nx: f32, ny: f32, w: usize, h: usize, flip: i32) -> (f32, f32) {
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let fx = nx * fw as f32;
    let fy = ny * fh as f32;
    match flip {
        3 => (w as f32 - 1.0 - fx, h as f32 - 1.0 - fy),
        6 => (fy, h as f32 - 1.0 - fx),
        5 => (w as f32 - 1.0 - fy, fx),
        _ => (fx, fy),
    }
}

pub fn spot_to_src(s: [f32; 4], w: usize, h: usize, flip: i32) -> (f32, f32, f32) {
    let (sx, sy) = frame_to_src(s[0], s[1], w, h, flip);
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    (sx, sy, (s[2] * fw.max(fh) as f32).max(2.0))
}

/// WB pick rectangle in virtual-src px: `size` is the half-width as a
/// fraction of the longer frame side (default ~0.025 = 5% box)
pub fn pick_rect(pick: [f32; 2], w: usize, h: usize, flip: i32, size: f32) -> [f32; 4] {
    let (cx, cy) = frame_to_src(pick[0], pick[1], w, h, flip);
    let (fw, fh) = match flip {
        5 | 6 => (h, w),
        _ => (w, h),
    };
    let r = size.clamp(0.002, 0.2) * fw.max(fh) as f32;
    [cx - r, cy - r, cx + r, cy + r]
}

/// clone stamps: copy a source patch into the destination circle (patch replacer)
fn clone_lin(lin: &mut [[f32; 3]], w: usize, h: usize, clones: &[[f32; 6]; 8], n: u32, flip: i32) {
    if n == 0 {
        return;
    }
    let orig = lin.to_vec();
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let xi = (x.round() as i32).clamp(0, w as i32 - 1) as usize;
        let yi = (y.round() as i32).clamp(0, h as i32 - 1) as usize;
        orig[yi * w + xi]
    };
    for c in clones.iter().take(n as usize) {
        let (sx, sy) = frame_to_src(c[0], c[1], w, h, flip);
        let (dx, dy) = frame_to_src(c[2], c[3], w, h, flip);
        let (fw, fh) = match flip {
            5 | 6 => (h, w),
            _ => (w, h),
        };
        let r = (c[4] * fw.max(fh) as f32).max(2.0);
        let x0 = (dx - r).max(0.0) as i32;
        let x1 = (dx + r).min(w as f32 - 1.0) as i32;
        let y0 = (dy - r).max(0.0) as i32;
        let y1 = (dy + r).min(h as f32 - 1.0) as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - dx).hypot(y as f32 - dy)) / r;
                if d < 1.0 {
                    let src = sample(x as f32 + sx - dx, y as f32 + sy - dy);
                    let blend = 1.0 - sstep(0.7, 1.0, d);
                    let i = y as usize * w + x as usize;
                    for ch in 0..3 {
                        lin[i][ch] = lin[i][ch] * (1.0 - blend) + src[ch] * blend;
                    }
                }
            }
        }
    }
}

fn heal_lin(lin: &mut [[f32; 3]], w: usize, h: usize, spots: &[[f32; 4]; 8], n: u32, flip: i32) {
    if n == 0 {
        return;
    }
    let orig = lin.to_vec();
    // clamped sampling (same as the GPU heal pass)
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let xi = (x.round() as i32).clamp(0, w as i32 - 1) as usize;
        let yi = (y.round() as i32).clamp(0, h as i32 - 1) as usize;
        orig[yi * w + xi]
    };
    for s in spots.iter().take(n as usize) {
        let (sx, sy, r) = spot_to_src(*s, w, h, flip);
        // auto-select the source patch: candidate centres on two rings at
        // 2.1r and 3.0r, scored by how well the annulus around the candidate
        // matches the target's annulus (sqrt-domain RGB distance — same
        // search as the gpu.rs heal_src pass)
        let (ox, oy) = heal_find_source(&sample, sx, sy, r);
        // frequency separation (LightCraft-style): healed = src + blur(target-src)
        // so the donor's fine texture lands in the target's colour/tonal field
        let x0 = (sx - r).max(0.0) as i32;
        let x1 = (sx + r).min(w as f32 - 1.0) as i32;
        let y0 = (sy - r).max(0.0) as i32;
        let y1 = (sy + r).min(h as f32 - 1.0) as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 - sx).hypot(y as f32 - sy)) / r;
                if d < 1.0 {
                    // low-freq diff = mean of (target-src) over centre + 8 taps at r*0.5
                    let mut acc = [0.0f32; 3];
                    for k in 0..9 {
                        let (tx, ty) = if k == 0 {
                            (x as f32, y as f32)
                        } else {
                            let a = (k - 1) as f32 * std::f32::consts::TAU / 8.0;
                            (x as f32 + r * 0.5 * a.cos(), y as f32 + r * 0.5 * a.sin())
                        };
                        let t = sample(tx, ty);
                        let sv = sample(tx + ox, ty + oy);
                        for c in 0..3 {
                            acc[c] += (t[c] - sv[c]) / 9.0;
                        }
                    }
                    let tms = acc;
                    let sv = sample(x as f32 + ox, y as f32 + oy);
                    let blend = 1.0 - sstep(0.7, 1.0, d);
                    let i = y as usize * w + x as usize;
                    for c in 0..3 {
                        let healed = sv[c] + tms[c];
                        lin[i][c] = lin[i][c] * (1.0 - blend) + healed * blend;
                    }
                }
            }
        }
    }
}

/// LR Remove (PatchMatch) spots: the catalog patchmap gives the exact
/// per-pixel source LR computed; we apply it with the same
/// frequency-separation blend as heal_lin (≈ the poisson seamless clone
/// LR uses): healed = src + lowpass(target − src) over a box blur.
fn pm_heal_lin(lin: &mut [[f32; 3]], w: usize, h: usize, pms: &[PmMap], frame: [f32; 2]) {
    if pms.is_empty() {
        return;
    }
    // pm coords are authored in sensor px; `lin` may be a virtual (subsampled)
    // frame — scale coords into it and stride the patchmap lookup.
    let sc = if frame[0] > 0.0 {
        w as f32 / frame[0]
    } else {
        1.0
    };
    let orig = lin.to_vec();
    let sample = |x: f32, y: f32| -> [f32; 3] {
        let xi = (x.round() as i32).clamp(0, w as i32 - 1) as usize;
        let yi = (y.round() as i32).clamp(0, h as i32 - 1) as usize;
        orig[yi * w + xi]
    };
    // source fetch = 3×3 box mean at ±2 px. LR applies PatchMatch maps in
    // the gradient domain (Method="poisson"), so per-px outliers on the
    // source side never reach the output; a small box gives the same
    // robustness while still passing texture above ~4 px (mirrors the
    // gpu.rs source fetch — keep in sync).
    let sample_src = |x: f32, y: f32| -> [f32; 3] {
        let mut acc = [0.0f32; 3];
        for oy in -2i32..=2 {
            for ox in -2i32..=2 {
                if (ox + oy) % 2 != 0 {
                    continue;
                }
                let xi = ((x + ox as f32).round() as i32).clamp(0, w as i32 - 1) as usize;
                let yi = ((y + oy as f32).round() as i32).clamp(0, h as i32 - 1) as usize;
                let p = orig[yi * w + xi];
                for c in 0..3 {
                    acc[c] += p[c] / 13.0;
                }
            }
        }
        acc
    };
    // patch-space source lookup in lin px; taps outside the map reuse the
    // pixel's own source — mirrors the gpu.rs pm loop (keep in sync).
    let src_at = |pm: &PmMap, px: f32, py: f32, fb: [f32; 2]| -> [f32; 2] {
        let lx = ((px - pm.dst[0] * sc) / sc).floor() as i32;
        let ly = ((py - pm.dst[1] * sc) / sc).floor() as i32;
        if lx >= 0 && ly >= 0 && lx < pm.pw as i32 && ly < pm.ph as i32 {
            let s = pm.src[ly as usize * pm.pw + lx as usize];
            [s[0] * sc, s[1] * sc]
        } else {
            fb
        }
    };
    for pm in pms {
        // iterate the dst rect in lin px — it may be smaller than pw×ph
        let dw = ((pm.dst[2] - pm.dst[0]) * sc).round().max(1.0) as i32;
        let dh = ((pm.dst[3] - pm.dst[1]) * sc).round().max(1.0) as i32;
        // low-freq correction LP(t − sv) sampled per px over a 4×4 tap grid
        // spanning ~patch/4. A patch-wide constant keeps each PatchMatch
        // fragment's own level (visible as blocky speckle); a tap radius
        // covering several fragments restores the target's smooth field
        // while keeping the donor's fine texture (mirrors the gpu.rs pm
        // loop — keep in sync).
        let tap_r = (dw.max(dh) as f32 * 0.25).max(1.0);
        for y in 0..dh {
            for x in 0..dw {
                let (dx, dy) = (pm.dst[0] * sc + x as f32, pm.dst[1] * sc + y as f32);
                // feathered coverage by the spot's ellipse
                let d = ((dx - pm.ell[0] * sc) / (pm.ell[2] * sc).max(1.0))
                    .hypot((dy - pm.ell[1] * sc) / (pm.ell[3] * sc).max(1.0));
                let blend = (1.0 - sstep(0.7, 1.0, d)) * pm.opacity;
                if blend <= 0.0 {
                    continue;
                }
                let xi = dx.round() as i32;
                let yi = dy.round() as i32;
                if xi < 0 || yi < 0 || xi >= w as i32 || yi >= h as i32 {
                    continue;
                }
                let own = src_at(pm, dx, dy, [dx, dy]);
                let mut acc = [0.0f32; 3];
                for gy in 0..4 {
                    for gx in 0..4 {
                        let tx = dx - tap_r + (gx as f32 + 0.5) * tap_r * 0.5;
                        let ty = dy - tap_r + (gy as f32 + 0.5) * tap_r * 0.5;
                        let so = src_at(pm, tx, ty, own);
                        let t = sample(tx, ty);
                        let svv = sample_src(so[0], so[1]);
                        for c in 0..3 {
                            acc[c] += (t[c] - svv[c]) / 16.0;
                        }
                    }
                }
                let sv = sample_src(own[0], own[1]);
                let li = yi as usize * w + xi as usize;
                for c in 0..3 {
                    let healed = sv[c] + acc[c];
                    lin[li][c] = lin[li][c] * (1.0 - blend) + healed * blend;
                }
            }
        }
    }
}

/// pick the donor offset for one heal spot. `sample` is clamped nearest.
/// Same candidate set + scoring as gpu.rs `heal_src` — keep in sync.
fn heal_find_source(sample: &dyn Fn(f32, f32) -> [f32; 3], sx: f32, sy: f32, r: f32) -> (f32, f32) {
    let tau = std::f32::consts::TAU;
    // target annulus: 8 taps just outside the spot edge
    let mut targ = [[0.0f32; 3]; 8];
    for (k, t) in targ.iter_mut().enumerate() {
        let a = k as f32 * tau / 8.0;
        *t = sample(sx + r * 1.15 * a.cos(), sy + r * 1.15 * a.sin());
    }
    let mut best = (0.0f32, 0.0f32);
    let mut best_score = f32::MAX;
    // candidate offsets: 16 on ring 2.1r, 8 on ring 3.0r
    for k in 0..24 {
        let (a, dist) = if k < 16 {
            (k as f32 * tau / 16.0, r * 2.1)
        } else {
            ((k - 16) as f32 * tau / 8.0 + tau / 32.0, r * 3.0)
        };
        let ox = a.cos() * dist;
        let oy = a.sin() * dist;
        let mut score = 0.0f32;
        for (kk, t) in targ.iter().enumerate() {
            let aa = kk as f32 * tau / 8.0;
            let sv = sample(sx + ox + r * 1.15 * aa.cos(), sy + oy + r * 1.15 * aa.sin());
            for c in 0..3 {
                score += (t[c].max(0.0).sqrt() - sv[c].max(0.0).sqrt()).abs();
            }
        }
        if score < best_score {
            best_score = score;
            best = (ox, oy);
        }
    }
    best
}

/// shared tail: spatial ops -> straighten/flip/crop/fit-resize -> adjust ->
/// dodge/burn -> grain/vignette -> gamma. Same ordering as the gpu.rs finish pass.
/// atmospheric-light scalar for dehaze: 99.5th percentile of min(r,g,b)
/// over the linear sRGB buffer (sparse sample). Mirrors the gpu.rs
/// stats_main min-channel histogram on wb_matrix(demosaic()).
fn atmosphere(lin: &[[f32; 3]], w: usize, h: usize) -> f32 {
    let mut hist = [0u32; 256];
    // identical sampling grid to gpu.rs stats_main so CPU/GPU derive the
    // same A: step = ceil(sqrt(px/65536)), 2D stride over (x,y)
    let step = ((w * h) as f64 / 65536.0).sqrt().ceil().max(2.0) as usize;
    let mut n = 0u64;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let px = &lin[y * w + x];
            let mn = px[0].min(px[1]).min(px[2]).clamp(0.0, 1.0);
            hist[(mn * 255.0) as usize] += 1;
            n += 1;
        }
    }
    let target = (n as f64 * 0.995) as u64;
    let mut acc = 0u64;
    for (i, &b) in hist.iter().enumerate() {
        acc += b as u64;
        if acc >= target.max(1) {
            return (i as f32 / 255.0).max(0.05);
        }
    }
    1.0
}

fn finish_linear(
    mut lin: Vec<[f32; 3]>,
    w: usize,
    h: usize,
    p: &Params,
    flip: i32,
    max_px: u32,
    subj: Option<&Matte>,
) -> RgbaImage {
    // dehaze's atmosphere scalar is image-dependent: derive it on the actual
    // linear buffer so mosaic and raster paths share the estimate.
    let mut pc = p.clone();
    if pc.dehaze > 0.0 {
        pc.dehaze_a = atmosphere(&lin, w, h);
    }
    // LR "auto" lateral CA: measure it instead of applying a fixed shift —
    // a hardcoded amount adds fringing to CA-free images. ca_fix>0 = auto
    // strength (0.3 ≈ full), <0 = the legacy manual radial warp.
    if pc.ca_fix > 0.0 {
        let s = (pc.ca_fix / 0.3).clamp(0.0, 1.0);
        let (kr, kb) = estimate_ca_px(&lin, w, h);
        pc.ca_r = kr * s;
        pc.ca_b = kb * s;
    }
    heal_lin(&mut lin, w, h, &pc.spots, pc.n_spots, flip);
    pm_heal_lin(&mut lin, w, h, &pc.pms, pc.pm_frame);
    clone_lin(&mut lin, w, h, &pc.clones, pc.n_clones, flip);
    if pc.noise_luma > 0.0 {
        lin = guided_nr_lum(&lin, w, h, pc.noise_luma);
    }
    if pc.noise_chroma > 0.0 {
        lin = chroma_smooth(&lin, w, h, pc.noise_chroma);
    }
    if pc.beauty > 0.0 || pc.deband > 0.0 {
        lin = beauty_deband(&lin, w, h, pc.beauty, pc.deband);
    }
    if pc.glow > 0.0 {
        lin = glow_lin(&lin, w, h, pc.glow);
    }
    if pc.sharpen > 0.0 || pc.clarity != 0.0 || pc.texture != 0.0 {
        lin = sharpen_clarity(
            &lin,
            w,
            h,
            pc.sharpen,
            pc.clarity,
            pc.sharp_mask,
            pc.sharp_radius,
            pc.sharp_detail,
            pc.texture,
        );
    }
    let (_fw, _fh, cl, ct, ew, eh, dw, dh) = frame_geometry(w, h, flip, pc.crop, max_px);
    let cx = cl + ew * 0.5 - 0.5;
    let cy = ct + eh * 0.5 - 0.5;
    let sin = pc.rotation_deg.to_radians().sin();
    let cos = pc.rotation_deg.to_radians().cos();
    // edge-aware brush seeds: sample the linear image at each flagged
    // stroke's first dab (dst-normalized → src-px via the same map the
    // pixel loop uses). Stages share the base mapping (crop unified).
    fill_brush_seeds(&mut pc, &lin, w, h, flip, cl, ct, ew, eh, cx, cy, sin, cos);
    let p = &pc;
    // heuristic sky matte for LR "sky" AI masks (MaskSubType=2):
    // computed once on the filtered linear image.
    let sky_mat;
    let sky: Option<&Matte> = if p
        .wins
        .iter()
        .take(p.n_wins as usize)
        .any(|w| (w[0] as i32) & (32 | 256) != 0)
        || p.stages.iter().any(|(sp, _, _)| {
            sp.wins
                .iter()
                .take(sp.n_wins as usize)
                .any(|w| (w[0] as i32) & (32 | 256) != 0)
        }) {
        sky_mat = sky_matte(&lin, w, h);
        Some(&sky_mat)
    } else {
        None
    };
    // heuristic vegetation matte for LR landscape "Vegetation" masks
    let veg_mat;
    let veg: Option<&Matte> = if p
        .wins
        .iter()
        .take(p.n_wins as usize)
        .any(|w| (w[0] as i32) & 128 != 0)
        || p.stages.iter().any(|(sp, _, _)| {
            sp.wins
                .iter()
                .take(sp.n_wins as usize)
                .any(|w| (w[0] as i32) & 128 != 0)
        }) {
        veg_mat = veg_matte(&lin, w, h);
        Some(&veg_mat)
    } else {
        None
    };
    // lens blur: matte-res defocused copy of the linear image, blended by
    // (1 - subject matte) — the matte is the focus proxy (LR uses a depth
    // map; "keep the subject sharp, blur the rest" approximates it)
    let lbl = if p.lens_blur > 0.001 {
        Some(lens_blur_img(
            &lin,
            w,
            h,
            p.lens_blur,
            p.lens_blur_opts,
            p.lens_blur_opts2,
        ))
    } else {
        None
    };
    // diagnostic dump: pre-tone rgb buffer for offline fitting
    if let Ok(dp) = std::env::var("SL_DUMP_PRETONE") {
        let mut buf = Vec::with_capacity(dw * dh * 3);
        for dy in 0..dh {
            for dx in 0..dw {
                let nx = (dx as f32 + 0.5) / dw as f32;
                let ny = (dy as f32 + 0.5) / dh as f32;
                let (sx, sy) = dst_to_src(
                    nx, ny, cl, ct, ew, eh, cx, cy, sin, cos, p.key_v, p.key_h, flip,
                    w, h,
                );
                buf.extend_from_slice(&pre_tone_rgb(bilinear(&lin, w, h, sx, sy), p, f32::NAN));
            }
        }
        let mut bytes = Vec::with_capacity(8 + buf.len() * 4);
        bytes.extend_from_slice(&(dw as u32).to_le_bytes());
        bytes.extend_from_slice(&(dh as u32).to_le_bytes());
        for v in &buf {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let _ = std::fs::write(&dp, &bytes);
    }
    // spatial tone guide: coarse grid of pretone luma, heavily blurred —
    // shadows/highlights respond to the surround, not just the pixel.
    let (gmode, gw_blend, gw_hl, gsigma) = spatial_tone();
    let guide = if p.has_tone && gmode > 0 && (gw_blend != 0.0 || gw_hl != 0.0) {
        let gw = (dw / 8).clamp(16, 512);
        let gh = (dh / 8).clamp(16, 512);
        let mut g = vec![0.0f32; gw * gh];
        for gy in 0..gh {
            for gx in 0..gw {
                let nx = (gx as f32 + 0.5) / gw as f32;
                let ny = (gy as f32 + 0.5) / gh as f32;
                let (sx, sy) = dst_to_src(
                    nx, ny, cl, ct, ew, eh, cx, cy, sin, cos, p.key_v, p.key_h, flip, w, h,
                );
                let pt = pre_tone_rgb(bilinear(&lin, w, h, sx, sy), p, f32::NAN);
                g[gy * gw + gx] = 0.2126 * pt[0] + 0.7152 * pt[1] + 0.0722 * pt[2];
            }
        }
        // 3-pass separable box blur ≈ gaussian; sigma in output-px fraction.
        // mode 8 = edge-aware: coarse bilateral on the grid instead.
        let r = (gsigma * gw as f32 * 0.5).max(1.0) as usize;
        if gmode == 8 {
            let sigma_i = 0.12f32; // intensity sigma (linear luma units)
            let src = g.clone();
            for y in 0..gh {
                for x in 0..gw {
                    let c0 = src[y * gw + x];
                    let mut acc = 0.0f32;
                    let mut ws = 0.0f32;
                    for yy in y.saturating_sub(2 * r)..(y + 2 * r).min(gh - 1) {
                        for xx in x.saturating_sub(2 * r)..(x + 2 * r).min(gw - 1) {
                            let d2 = ((xx - x) * (xx - x) + (yy - y) * (yy - y)) as f32;
                            let sp = (-d2 / (2.0 * r as f32 * r as f32)).exp();
                            let di = src[yy * gw + xx] - c0;
                            let wij = sp * (-di * di / (2.0 * sigma_i * sigma_i)).exp();
                            acc += src[yy * gw + xx] * wij;
                            ws += wij;
                        }
                    }
                    g[y * gw + x] = if ws > 0.0 { acc / ws } else { c0 };
                }
            }
        } else {
            let mut t = vec![0.0f32; gw * gh];
            for _ in 0..3 {
                for y in 0..gh {
                    for x in 0..gw {
                        let x0 = x.saturating_sub(r);
                        let x1 = (x + r).min(gw - 1);
                        let mut acc = 0.0f32;
                        for xx in x0..=x1 {
                            acc += g[y * gw + xx];
                        }
                        t[y * gw + x] = acc / (x1 - x0 + 1) as f32;
                    }
                }
                for y in 0..gh {
                    for x in 0..gw {
                        let y0 = y.saturating_sub(r);
                        let y1 = (y + r).min(gh - 1);
                        let mut acc = 0.0f32;
                        for yy in y0..=y1 {
                            acc += t[yy * gw + x];
                        }
                        g[y * gw + x] = acc / (y1 - y0 + 1) as f32;
                    }
                }
            }
        }
        Some((g, gw, gh))
    } else {
        None
    };
    let guide_at = |nx: f32, ny: f32| -> f32 {
        match &guide {
            Some((g, gw, gh)) => {
                let gx = (nx * *gw as f32 - 0.5).clamp(0.0, *gw as f32 - 1.0);
                let gy = (ny * *gh as f32 - 0.5).clamp(0.0, *gh as f32 - 1.0);
                let x0 = gx as usize;
                let y0 = gy as usize;
                let x1 = (x0 + 1).min(*gw - 1);
                let y1 = (y0 + 1).min(*gh - 1);
                let fx = gx - x0 as f32;
                let fy = gy - y0 as f32;
                (g[y0 * gw + x0] * (1.0 - fx) + g[y0 * gw + x1] * fx) * (1.0 - fy)
                    + (g[y1 * gw + x0] * (1.0 - fx) + g[y1 * gw + x1] * fx) * fy
            }
            None => f32::NAN,
        }
    };
    // dehaze dark-channel prior: coarse grid of pretone min(r,g,b), then
    // patchwise MIN (≈3% of frame) + blur — the veil varies smoothly and is
    // driven by the darkest channel of the local surround, not the pixel.
    let veil = if p.dehaze > 0.0 {
        let vw = (dw / 8).clamp(16, 512);
        let vh = (dh / 8).clamp(16, 512);
        let mut vg = vec![0.0f32; vw * vh];
        for gy in 0..vh {
            for gx in 0..vw {
                let nx = (gx as f32 + 0.5) / vw as f32;
                let ny = (gy as f32 + 0.5) / vh as f32;
                let (sx, sy) = dst_to_src(
                    nx, ny, cl, ct, ew, eh, cx, cy, sin, cos, p.key_v, p.key_h, flip, w, h,
                );
                let pt = pre_tone_rgb(bilinear(&lin, w, h, sx, sy), p, f32::NAN);
                vg[gy * vw + gx] = pt[0].min(pt[1]).min(pt[2]);
            }
        }
        // patchwise minimum (morphological erosion over ~3% of the frame)
        let rp = ((vw as f32) * 0.03).max(2.0) as usize;
        let mut t = vec![0.0f32; vw * vh];
        for y in 0..vh {
            for x in 0..vw {
                let x0 = x.saturating_sub(rp);
                let x1 = (x + rp).min(vw - 1);
                let mut m = f32::MAX;
                for xx in x0..=x1 {
                    m = m.min(vg[y * vw + xx]);
                }
                t[y * vw + x] = m;
            }
        }
        for y in 0..vh {
            for x in 0..vw {
                let y0 = y.saturating_sub(rp);
                let y1 = (y + rp).min(vh - 1);
                let mut m = f32::MAX;
                for yy in y0..=y1 {
                    m = m.min(t[yy * vw + x]);
                }
                vg[y * vw + x] = m;
            }
        }
        // soften the patch quantization (2 box-blur passes)
        for _ in 0..2 {
            let r = rp.max(1);
            let mut t2 = vec![0.0f32; vw * vh];
            for y in 0..vh {
                for x in 0..vw {
                    let x0 = x.saturating_sub(r);
                    let x1 = (x + r).min(vw - 1);
                    let mut acc = 0.0f32;
                    for xx in x0..=x1 {
                        acc += vg[y * vw + xx];
                    }
                    t2[y * vw + x] = acc / (x1 - x0 + 1) as f32;
                }
            }
            for y in 0..vh {
                for x in 0..vw {
                    let y0 = y.saturating_sub(r);
                    let y1 = (y + r).min(vh - 1);
                    let mut acc = 0.0f32;
                    for yy in y0..=y1 {
                        acc += t2[yy * vw + x];
                    }
                    vg[y * vw + x] = acc / (y1 - y0 + 1) as f32;
                }
            }
        }
        Some((vg, vw, vh))
    } else {
        None
    };
    let veil_at = |nx: f32, ny: f32| -> f32 {
        match &veil {
            Some((g, vw, vh)) => {
                let gx = (nx * *vw as f32 - 0.5).clamp(0.0, *vw as f32 - 1.0);
                let gy = (ny * *vh as f32 - 0.5).clamp(0.0, *vh as f32 - 1.0);
                let x0 = gx as usize;
                let y0 = gy as usize;
                let x1 = (x0 + 1).min(*vw - 1);
                let y1 = (y0 + 1).min(*vh - 1);
                let fx = gx - x0 as f32;
                let fy = gy - y0 as f32;
                (g[y0 * vw + x0] * (1.0 - fx) + g[y0 * vw + x1] * fx) * (1.0 - fy)
                    + (g[y1 * vw + x0] * (1.0 - fx) + g[y1 * vw + x1] * fx) * fy
            }
            None => f32::NAN,
        }
    };
    let mut out = vec![0u8; dw * dh * 4];
    for dy in 0..dh {
        for dx in 0..dw {
            // dst -> crop rect in post-flip frame, undo straighten, undo flip -> src
            let nx = (dx as f32 + 0.5) / dw as f32;
            let ny = (dy as f32 + 0.5) / dh as f32;
            let (sx, sy) = dst_to_src(
                nx, ny, cl, ct, ew, eh, cx, cy, sin, cos, p.key_v, p.key_h, flip, w, h,
            );

            // chromatic-aberration fix: radial per-channel sample offsets
            let mut col = if p.ca_fix != 0.0 {
                let wcx = w as f32 * 0.5;
                let wcy = h as f32 * 0.5;
                let (fr, fb) = if p.ca_fix > 0.0 {
                    // auto: measured constant radial scale per channel
                    (1.0 - p.ca_r, 1.0 - p.ca_b)
                } else {
                    // manual: legacy quadratic radial warp
                    let rn = ((sx - wcx).hypot(sy - wcy) / w.max(h) as f32) * 2.0;
                    (1.0 - p.ca_fix * 0.05 * rn, 1.0 + p.ca_fix * 0.05 * rn)
                };
                [
                    bilin_ch(&lin, w, h, wcx + (sx - wcx) * fr, wcy + (sy - wcy) * fr, 0),
                    bilin_ch(&lin, w, h, sx, sy, 1),
                    bilin_ch(&lin, w, h, wcx + (sx - wcx) * fb, wcy + (sy - wcy) * fb, 2),
                ]
            } else {
                bilinear(&lin, w, h, sx, sy)
            };
            if let Some((b, bw, bh)) = &lbl {
                let bm = (1.0 - subj.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0)) * p.lens_blur;
                if bm > 0.001 {
                    let bx = (sx / w as f32) * (*bw as f32) - 0.5;
                    let by = (sy / h as f32) * (*bh as f32) - 0.5;
                    for c in 0..3 {
                        col[c] += (bilinear3(b, *bw, *bh, bx, by, c) - col[c]) * bm;
                    }
                }
            }
            if std::env::var_os("SL_PROBE").is_some() && (dx * 7 + dy * 13) % 97 == 0 {
                let pt = pre_tone_rgb(col, p, f32::NAN);
                let mut a0 = adjust_g(col, p, guide_at(nx, ny), veil_at(nx, ny));
                local_ops(&mut a0, col, p, nx, ny, dw as f32 / dh as f32, subj, sky, veg);
                eprintln!(
                    "[probe] {nx:.3},{ny:.3} cam={col:?} pretone={pt:?} adj={a0:?}"
                );
            }
            let mut adj = adjust_g(col, p, guide_at(nx, ny), veil_at(nx, ny));
            let aspect = dw as f32 / dh as f32;
            local_ops(&mut adj, col, p, nx, ny, aspect, subj, sky, veg);
            // serial correction stages (DaVinci serial nodes): full adjust +
            // local ops on the sampled colour, blended by the stage key.
            for (sp, sop, sinv) in p.stages.iter() {
                let mut key = 1.0f32;
                if sp.has_qual {
                    key *= qual_mask(col, sp);
                }
                if sp.n_wins > 0 {
                    let mut wu = 0.0f32;
                    for (wi, win) in sp.wins.iter().take(sp.n_wins as usize).enumerate() {
                        let mut m2 = window_mask(
                            win,
                            &sp.refines[wi],
                            nx,
                            ny,
                            col,
                            subj,
                            sky,
                            veg,
                            &sp.wins[..sp.n_wins as usize],
                            &sp.wmats,
                        ) * win[10];
                        if (win[0] as i32) & 8 != 0 {
                            m2 *= qual_mask(col, sp);
                        }
                        wu = wu.max(m2);
                    }
                    key *= wu;
                }
                if *sinv {
                    key = 1.0 - key;
                }
                key *= *sop;
                if key <= 0.001 {
                    continue;
                }
                let mut a2 = adjust_g(col, sp, guide_at(nx, ny), veil_at(nx, ny));
                local_ops(&mut a2, col, sp, nx, ny, aspect, subj, sky, veg);
                for c in 0..3 {
                    adj[c] += (a2[c] - adj[c]) * key;
                }
            }
            // lens flare: core glow + horizontal streak + mirrored ghost ring
            if p.flare[2] > 0.0 {
                let (fx0, fy0, fs) = (p.flare[0], p.flare[1], p.flare[2]);
                let dx = nx - fx0;
                let dy = ny - fy0;
                let d2 = dx * dx + dy * dy;
                let core = (-d2 / 0.004).exp();
                let streak = (-dy * dy / (0.0004 + 0.02 * fs)).exp() * (-dx.abs() / 0.35).exp();
                let gx = 1.0 - fx0;
                let gy = 1.0 - fy0;
                let gd = ((nx - gx).hypot(ny - gy) - 0.10).abs();
                let ghost = (-gd * gd / 0.0008).exp();
                let colf = hue_to_rgb(p.flare[3]);
                let f = fs * (0.5 * core + 0.7 * streak + 0.35 * ghost);
                for c in 0..3 {
                    adj[c] += f * colf[c];
                }
            }
            if p.grain > 0.0 {
                // LR grain: Size = cell blockiness, Roughness = mix of
                // low-frequency clumps into the white noise.
                let cell = p.grain_size.max(1.0);
                for (c, seed) in adj.iter_mut().zip([0.0f32, 17.0, 43.0]) {
                    let hf = hash_px(dx, dy, seed);
                    let lf = if cell > 1.5 {
                        let fx = dx as f32 / cell;
                        let fy = dy as f32 / cell;
                        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                        let (tx, ty) = (fx.fract(), fy.fract());
                        let n00 = hash_px(x0, y0, seed);
                        let n10 = hash_px(x0 + 1, y0, seed);
                        let n01 = hash_px(x0, y0 + 1, seed);
                        let n11 = hash_px(x0 + 1, y0 + 1, seed);
                        let top = n00 + (n10 - n00) * tx;
                        let bot = n01 + (n11 - n01) * tx;
                        top + (bot - top) * ty
                    } else {
                        hf
                    };
                    let n = hf * (1.0 - p.grain_rough * 0.6) + lf * (p.grain_rough * 0.6);
                    *c += (n - 0.5) * p.grain * 0.12;
                }
            }
            if p.vignette != 0.0 {
                let vx = nx * 2.0 - 1.0;
                let vy = ny * 2.0 - 1.0;
                // LR post-crop vignette shape: midpoint = 50% radius,
                // feather = ramp width, roundness mixes frame-aligned
                // (chebyshev) distance toward circular.
                let (d, r0, r1) = if p.vig_shape[0] >= 0.0 {
                    let dc = (vx * vx + vy * vy).sqrt() * 0.7071;
                    let dr = vx.abs().max(vy.abs());
                    let w = (p.vig_shape[1] + 1.0) * 0.5;
                    let wid = p.vig_shape[2].max(0.02);
                    (
                        dr + (dc - dr) * w,
                        p.vig_shape[0] - wid * 0.5,
                        p.vig_shape[0] + wid * 0.5,
                    )
                } else {
                    ((vx * vx + vy * vy).sqrt() * 0.7071, 0.35, 1.05)
                };
                let f = 1.0 - p.vignette * sstep(r0, r1, d) * 0.9;
                for c in &mut adj {
                    *c *= f;
                }
            }
            let o = (dy * dw + dx) * 4;
            for c in 0..3 {
                out[o + c] = (srgb_encode(adj[c].clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
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

/// dst-normalized (nx,ny) → src-px (sx,sy): crop → keystone → unrotate →
/// unflip. Shared by the finish_linear pixel loop and brush-seed sampling.
#[allow(clippy::too_many_arguments)]
fn dst_to_src(
    nx: f32,
    ny: f32,
    cl: f32,
    ct: f32,
    ew: f32,
    eh: f32,
    cx: f32,
    cy: f32,
    sin: f32,
    cos: f32,
    key_v: f32,
    key_h: f32,
    flip: i32,
    w: usize,
    h: usize,
) -> (f32, f32) {
    let mut fx = cl + nx * ew - 0.5;
    let mut fy = ct + ny * eh - 0.5;
    if key_v != 0.0 || key_h != 0.0 {
        fx = cx + (fx - cx) * (1.0 + key_v * (ny * 2.0 - 1.0));
        fy = cy + (fy - cy) * (1.0 + key_h * (nx * 2.0 - 1.0));
    }
    let px = fx - cx;
    let py = fy - cy;
    fx = cx + px * cos + py * sin;
    fy = cy - px * sin + py * cos;
    match flip {
        3 => (w as f32 - 1.0 - fx, h as f32 - 1.0 - fy),
        6 => (fy, h as f32 - 1.0 - fx),
        5 => (w as f32 - 1.0 - fy, fx),
        _ => (fx, fy),
    }
}

/// Edge-aware brush seeds: for every stroke whose slot carries a tolerance,
/// sample the linear image under the stroke's first dab into its [r,g,b].
/// Runs over base + every stage (they share the base dst→src mapping).
#[allow(clippy::too_many_arguments)]
fn fill_brush_seeds(
    pc: &mut Params,
    lin: &[[f32; 3]],
    w: usize,
    h: usize,
    flip: i32,
    cl: f32,
    ct: f32,
    ew: f32,
    eh: f32,
    cx: f32,
    cy: f32,
    sin: f32,
    cos: f32,
) {
    fn fill_one(
        sp: &mut Params,
        lin: &[[f32; 3]],
        w: usize,
        h: usize,
        flip: i32,
        cl: f32,
        ct: f32,
        ew: f32,
        eh: f32,
        cx: f32,
        cy: f32,
        sin: f32,
        cos: f32,
        key_v: f32,
        key_h: f32,
    ) {
        let n = sp.brush_misc[0] as usize;
        for s in 0..n {
            if sp.brush_seeds.get(s).map_or(0.0, |sd| sd[3]) <= 0.0 {
                continue;
            }
            let s0 = sp.brush_strokes.get(s * 3 + 1).map_or(0.0, |e| e[0]) as usize;
            let Some(seg) = sp.brush_segs.get(s0) else {
                continue;
            };
            let (sx, sy) = dst_to_src(
                seg[0], seg[1], cl, ct, ew, eh, cx, cy, sin, cos, key_v, key_h, flip, w, h,
            );
            let c = bilinear(lin, w, h, sx, sy);
            if let Some(sd) = sp.brush_seeds.get_mut(s) {
                sd[0] = c[0];
                sd[1] = c[1];
                sd[2] = c[2];
            }
        }
    }
    let (kv, kh) = (pc.key_v, pc.key_h);
    fill_one(
        pc, lin, w, h, flip, cl, ct, ew, eh, cx, cy, sin, cos, kv, kh,
    );
    for (sp, _, _) in pc.stages.iter_mut() {
        fill_one(
            sp, lin, w, h, flip, cl, ct, ew, eh, cx, cy, sin, cos, kv, kh,
        );
    }
}

/// Affinity/LightCraft auto-mask similarity: how close `col` is to the
/// stroke's seed colour. Luma distance in EV + max-channel chroma distance,
/// each smoothstepped by `tol`.
fn edge_sim(col: [f32; 3], seed: [f32; 3], tol: f32) -> f32 {
    let lc = 0.2126 * col[0] + 0.7152 * col[1] + 0.0722 * col[2];
    let ls = 0.2126 * seed[0] + 0.7152 * seed[1] + 0.0722 * seed[2];
    if lc < 1e-4 || ls < 1e-4 {
        return 1.0; // too dark to judge — keep the paint
    }
    let dl = (lc / ls).abs().log2().abs();
    let dc = (col[0] - seed[0])
        .abs()
        .max((col[1] - seed[1]).abs())
        .max((col[2] - seed[2]).abs());
    (1.0 - sstep(0.5 * tol, tol, dl)) * (1.0 - sstep(0.25 * tol, 0.5 * tol, dc))
}

/// local spatial ops shared by the base grade and every serial stage:
/// dodge/burn lights, power windows and adjustment brushes.
/// `col` = pre-adjust sampled colour (masks that read source pixels).
fn local_ops(
    adj: &mut [f32; 3],
    col: [f32; 3],
    p: &Params,
    nx: f32,
    ny: f32,
    aspect: f32,
    subj: Option<&Matte>,
    sky: Option<&Matte>,
    veg: Option<&Matte>,
) {
    // dodge/burn radial lights (dst-normalized coords)
    for l in p.lights.iter().take(p.n_lights as usize) {
        let d = ((nx - l[0]).hypot(ny - l[1])) / l[2].max(1e-3);
        let f = (-d * d * 2.77).exp(); // gaussian falloff
        adj[0] *= 1.0 + l[3] * f * 0.5;
        adj[1] *= 1.0 + l[3] * f * 0.5;
        adj[2] *= 1.0 + l[3] * f * 0.5;
    }
    // power windows: local ev/sat/temp inside the mask
    for (wi, win) in p.wins.iter().take(p.n_wins as usize).enumerate() {
        let mut mask = window_mask(
            win,
            &p.refines[wi],
            nx,
            ny,
            col,
            subj,
            sky,
            veg,
            &p.wins[..p.n_wins as usize],
            &p.wmats,
        ) * win[10];
        // link_q: gate the window by the HSL qualifier matte
        if (win[0] as i32) & 8 != 0 {
            mask *= qual_mask(col, p);
        }
        if mask <= 0.001 {
            continue;
        }
        let evg = (2.0f32).powf(win[7] * mask);
        let l = 0.2126 * adj[0] + 0.7152 * adj[1] + 0.0722 * adj[2];
        for c in 0..3 {
            adj[c] = adj[c] * evg;
            adj[c] = l + (adj[c] - l) * (1.0 + win[8] * mask);
        }
        adj[0] += win[9] * mask * 0.08;
        adj[2] -= win[9] * mask * 0.08;
        local_extra(adj, mask, &win[12..21]);
        // masked point colours (LR LocalPointColors): transform the pixel
        // colour inside the mask, folded into the local gain
        let lp = &p.lpcs[wi];
        for s in 0..(lp[0] as usize).min(2) {
            let o = 1 + s * 20;
            let sw = PcSwatch {
                src: [lp[o], lp[o + 1], lp[o + 2]],
                shift: [lp[o + 3], lp[o + 4], lp[o + 5]],
                amt: lp[o + 6],
                hr: [lp[o + 7], lp[o + 8], lp[o + 9], lp[o + 10]],
                sr: [lp[o + 11], lp[o + 12], lp[o + 13], lp[o + 14]],
                lr: [lp[o + 15], lp[o + 16], lp[o + 17], lp[o + 18]],
            };
            let tgt = pc_apply(*adj, &sw, lp[o + 19]);
            for c in 0..3 {
                adj[c] += (tgt[c] - adj[c]) * mask;
            }
        }
    }
    // adjustment brushes: same local ev/sat/temp inside stroke masks
    let n_str = p.brush_misc[0] as usize;
    if n_str == 0 {
        return;
    }
    let link_bits = p.brush_misc[1] as u32;
    let (pax, pay) = (nx * aspect, ny);
    let mut pm = [0.0f32; 4];
    let mut em = [0.0f32; 4];
    for s in 0..n_str {
        let e0 = p.brush_strokes[s * 3];
        let e1 = p.brush_strokes[s * 3 + 1];
        let bb = p.brush_strokes[s * 3 + 2];
        let rad = e0[0];
        if pax < bb[0] * aspect - rad || pax > bb[2] * aspect + rad {
            continue;
        }
        if pay < bb[1] - rad || pay > bb[3] + rad {
            continue;
        }
        let inner = rad * (1.0 - e0[1]);
        let mut w = 0.0f32;
        let (s0, s1) = (e1[0] as usize, (e1[0] + e1[1]) as usize);
        for g in s0..s1.min(p.brush_segs.len()) {
            let seg = p.brush_segs[g];
            let ax = seg[0] * aspect;
            let bx = seg[2] * aspect;
            let (vx, vy) = (bx - ax, seg[3] - seg[1]);
            let len2 = (vx * vx + vy * vy).max(1e-9);
            let t = (((pax - ax) * vx + (pay - seg[1]) * vy) / len2).clamp(0.0, 1.0);
            let dx = pax - ax - t * vx;
            let dy = pay - seg[1] - t * vy;
            let d = (dx * dx + dy * dy).sqrt();
            w = w.max(1.0 - sstep(inner, rad, d));
        }
        // edge-aware stroke: gate the painted weight by colour similarity
        // to the seed sampled under the stroke's first dab
        if let Some(sd) = p.brush_seeds.get(s) {
            if sd[3] > 0.0 {
                w *= edge_sim(col, [sd[0], sd[1], sd[2]], sd[3]);
            }
        }
        let li = (e0[3] as usize).min(3);
        if e0[2] >= 0.0 {
            pm[li] = pm[li].max(w * e0[2]);
        } else {
            em[li] = em[li].max(w * -e0[2]);
        }
    }
    for (li, lp) in p.brush_layers.iter().enumerate() {
        let mut mask = (pm[li] - em[li]).clamp(0.0, 1.0) * lp[3];
        if mask <= 0.001 {
            continue;
        }
        if link_bits & (1u32 << li) != 0 {
            mask *= qual_mask(col, p);
        }
        if mask <= 0.001 {
            continue;
        }
        let evg = (2.0f32).powf(lp[0] * mask);
        let l = 0.2126 * adj[0] + 0.7152 * adj[1] + 0.0722 * adj[2];
        for c in 0..3 {
            adj[c] = adj[c] * evg;
            adj[c] = l + (adj[c] - l) * (1.0 + lp[1] * mask);
        }
        adj[0] += lp[2] * mask * 0.08;
        adj[2] -= lp[2] * mask * 0.08;
        let ex = p.brush_layers2[li];
        local_extra(adj, mask, &ex);
    }
}

/// extended LR local adjustments inside a window/brush mask:
/// ex = [contrast, clarity, dehaze, tint, highlights, shadows, whites,
/// blacks] — luma-pivot contrast, midtone-weighted clarity, dehaze
/// density, green/magenta tint axis and tone-band offsets mirroring
/// the global tone stage's shaping weights.
fn local_extra(adj: &mut [f32; 3], mask: f32, ex: &[f32]) {
    let (con, cla, deh, tin) = (ex[0], ex[1], ex[2], ex[3]);
    let (hi, sh, wh, bl) = (ex[4], ex[5], ex[6], ex[7]);
    let tex = ex.get(8).copied().unwrap_or(0.0);
    if tin != 0.0 {
        adj[1] -= tin * mask * 0.06;
    }
    if con != 0.0
        || cla != 0.0
        || deh != 0.0
        || hi != 0.0
        || sh != 0.0
        || wh != 0.0
        || bl != 0.0
        || tex != 0.0
    {
        let l = (0.2126 * adj[0] + 0.7152 * adj[1] + 0.0722 * adj[2]).clamp(0.0, 1.0);
        let mid = 4.0 * l * (1.0 - l);
        let k = (con + cla * mid * 0.8 + tex * mid * 0.6 + deh * 0.2) * mask * 0.9;
        for c in adj.iter_mut() {
            *c = l + (*c - l) * (1.0 + k);
        }
        if deh != 0.0 {
            let f = deh * mask;
            for c in adj.iter_mut() {
                *c *= 1.0 + 0.08 * f;
                *c -= 0.015 * f * (1.0 - l);
            }
        }
        // tone-band offsets: same band weights as build_tone_lut
        if sh != 0.0 {
            let s = sh * mask * 0.10 * (1.0 - sstep(0.0, 0.66, l)) * (1.0 - l / 1.6).max(0.0);
            for c in adj.iter_mut() {
                *c += s;
            }
        }
        if hi != 0.0 {
            let d = (1.0 - hi * mask * 2.19 * sstep(0.60, 1.49, l)).max(0.15);
            for c in adj.iter_mut() {
                *c /= d;
            }
        }
        if wh != 0.0 {
            let g = 1.0 + wh * mask * 0.35 * sstep(0.5, 1.0, l);
            for c in adj.iter_mut() {
                *c *= g;
            }
        }
        if bl != 0.0 {
            let lw = 1.0 - sstep(0.0, 0.4, l);
            if bl > 0.0 {
                for c in adj.iter_mut() {
                    *c += bl * mask * 0.06 * lw;
                }
            } else {
                for c in adj.iter_mut() {
                    *c *= 1.0 + bl * mask * 0.25 * lw;
                }
            }
        }
    }
}

/// power-window mask value at dst-normalized (nx,ny)
/// packed [kind(0/1) + 2=invert + 4=lum + 8=link_q + 16=subject + 32=sky,
/// a,b,c,d, rot, soft, ev, sat, temp, strength]
/// `col` = the sampled pre-adjust colour (lum range + qualifier gating need it)
/// soft-ellipse mask value for the AI-gated window kinds (part/veg/nsky):
/// [cx,cy,rx,ry,rot,soft] with the 50% radius at w[11] when set.
fn ell_mask(w: &[f32], nx: f32, ny: f32) -> f32 {
    let rot = w[5].to_radians();
    let dx = nx - w[1];
    let dy = ny - w[2];
    let rx = w[3].max(0.005);
    let ry = w[4].max(0.005);
    let ux = (dx * rot.cos() + dy * rot.sin()) / rx;
    let uy = (-dx * rot.sin() + dy * rot.cos()) / ry;
    let d = (ux * ux + uy * uy).sqrt();
    let soft = w[6].clamp(0.0, 0.95);
    let mid = if w[11] > 0.001 {
        w[11].clamp(0.01, 0.99)
    } else {
        1.0 - soft * 0.5
    };
    let r0 = (mid - soft * 0.5).max(0.0);
    let r1 = (mid + soft * 0.5).min(1.0);
    1.0 - sstep(r0, r1.max(r0 + 0.01), d)
}

/// LR color-range matte: feathered distance in hue/sat space around a
/// sampled colour, tolerance driven by ColorAmount (0..1).
fn color_matte(col: [f32; 3], h: f32, s: f32, tol: f32) -> f32 {
    let (ph, ps, _) = rgb_to_hsv(col);
    let dh = hue_dist(ph, h);
    let d = (dh * dh + (ps - s) * (ps - s)).sqrt();
    let t = 0.04 + tol.clamp(0.0, 1.0) * 0.26;
    1.0 - sstep(t, t + 0.08, d)
}

/// LR range-mask refinement folded into a window:
/// [mode, kind, p0..p3]; mode 1=subtract / 2=intersect, kind 1=color
/// (h,s,tol,inv) / 2=lum (lo,hi,feather,inv). Applied to the base mask.
fn refine_mul(ref_: &[f32; 6], mask: f32, col: [f32; 3]) -> f32 {
    let mode = ref_[0] as i32;
    if mode == 0 || mask <= 0.0 {
        return mask;
    }
    let m = if ref_[1] as i32 == 2 {
        let l = 0.2126 * col[0] + 0.7152 * col[1] + 0.0722 * col[2];
        let f = ref_[4].max(0.005);
        sstep(ref_[2] - f, ref_[2] + f, l) * (1.0 - sstep(ref_[3] - f, ref_[3] + f, l))
    } else {
        color_matte(col, ref_[2], ref_[3], ref_[4])
    };
    let m = if ref_[5] > 0.5 { 1.0 - m } else { m };
    if mode == 2 {
        mask * m
    } else {
        mask * (1.0 - m)
    }
}

fn window_mask(
    w: &[f32],
    ref_: &[f32; 6],
    nx: f32,
    ny: f32,
    col: [f32; 3],
    subj: Option<&Matte>,
    sky: Option<&Matte>,
    veg: Option<&Matte>,
    wins: &[[f32; 24]],
    wmats: &[WMatte],
) -> f32 {
    let base = w[0] as i32;
    let kind = base % 2;
    let inv = base & 2 != 0;
    let lum = base & 4 != 0;
    let mask = if base & 1024 != 0 {
        // real LR AI-mask bitmap (extracted catalog blob): affine-map the
        // post-crop normalized coords into the bbox and bilinear-sample.
        // Pixels outside the TIFF bbox are mask=0.
        wmats
            .get(w[21] as usize)
            .map(|t| {
                let u = nx * t.ux + ny * t.uy + t.uc;
                let v = nx * t.vx + ny * t.vy + t.vc;
                if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                    matte_bil(&t.m, u, v)
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0)
    } else if base & 16 != 0 {
        // AI subject matte: bilinear sample in normalized dst coords
        subj.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0)
    } else if base & 64 != 0 {
        // LR person-part mask (MaskSubType=3): subject matte gated by a
        // soft ellipse prior positioned at the named body region
        // (approximation — we have no per-part segmentation model).
        ell_mask(w, nx, ny) * subj.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0)
    } else if base & 128 != 0 {
        // LR landscape "Vegetation" mask: heuristic veg matte × region prior
        ell_mask(w, nx, ny) * veg.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0)
    } else if base & 256 != 0 {
        // LR landscape semantic mask (MaskSubType=0, non-sky classes):
        // (1 - sky matte) gated by a region prior for the named class.
        // Semantic classes are disjoint in LR — normalize by the sum of
        // all nsky region priors so overlapping priors share weight.
        let ell = ell_mask(w, nx, ny);
        let mut cover = 0.0;
        for o in wins.iter() {
            if (o[0] as i32) & 256 != 0 && o[10] > 0.0 {
                cover += ell_mask(o, nx, ny);
            }
        }
        ell / cover.max(1.0) * (1.0 - sky.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0))
    } else if base & 32 != 0 {
        // heuristic sky matte (LR sky AI masks)
        sky.map(|m| matte_bil(m, nx, ny)).unwrap_or(0.0)
    } else if base & 512 != 0 {
        // standalone LR color-range mask component
        color_matte(col, w[1], w[2], w[3])
    } else if lum {
        // luminance range: p=[lo,hi,lo_feather,hi_feather] over display luma
        let l = 0.2126 * col[0] + 0.7152 * col[1] + 0.0722 * col[2];
        let (lo, hi) = (w[1], w[2]);
        let lf = w[3].max(0.005);
        let hf = w[4].max(0.005);
        sstep(lo - lf, lo + lf, l) * (1.0 - sstep(hi - hf, hi + hf, l))
    } else if kind == 1 {
        // gradient: [x1,y1,x2,y2,mid,soft] — full cover before the p1..p2
        // span; the 1→0 ramp centres at t=mid (0.5 unless overridden).
        let (ax, ay, bx, by) = (w[1], w[2], w[3], w[4]);
        let soft = w[6].max(0.02);
        let mid = if w[11] > 0.001 { w[11] } else { 0.5 };
        let dx = bx - ax;
        let dy = by - ay;
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let t = ((nx - ax) * dx + (ny - ay) * dy) / len2;
        1.0 - sstep(mid - soft * 0.5, mid + soft * 0.5, t)
    } else {
        // circle/ellipse: [cx,cy,rx,ry,rot,soft] — mid moves the 50% radius.
        let rot = w[5].to_radians();
        let dx = nx - w[1];
        let dy = ny - w[2];
        let rx = w[3].max(0.005);
        let ry = w[4].max(0.005);
        let ux = (dx * rot.cos() + dy * rot.sin()) / rx;
        let uy = (-dx * rot.sin() + dy * rot.cos()) / ry;
        let d = (ux * ux + uy * uy).sqrt();
        let soft = w[6].clamp(0.0, 0.95);
        let mid = if w[11] > 0.001 {
            w[11].clamp(0.01, 0.99)
        } else {
            1.0 - soft * 0.5
        };
        let r0 = (mid - soft * 0.5).max(0.0);
        let r1 = (mid + soft * 0.5).min(1.0);
        1.0 - sstep(r0, r1.max(r0 + 0.01), d)
    };
    let mask = refine_mul(ref_, mask, col);
    if inv {
        1.0 - mask
    } else {
        mask
    }
}

/// heuristic sky matte for LR "sky" AI masks (MaskSubType=2): a product
/// of the blue-cyan hue gate, saturation, brightness and an upward
/// vertical prior, sampled at reduced resolution then box-blurred.
/// Without it, sky-masked corrections would silently apply to nothing.
fn sky_matte(lin: &[[f32; 3]], w: usize, h: usize) -> Matte {
    let mw = w.min(384).max(2);
    let mh = h.min(384).max(2);
    let mut d = vec![0.0f32; mw * mh];
    for my in 0..mh {
        for mx in 0..mw {
            let sx = (((mx as f32 + 0.5) / mw as f32) * w as f32) as usize % w;
            let sy = (((my as f32 + 0.5) / mh as f32) * h as f32) as usize % h;
            let c = lin[sy * w + sx];
            let (hh, s, v) = rgb_to_hsv(c);
            let deg = hh * 360.0;
            let mut dh = (deg - 217.5).abs();
            if dh > 180.0 {
                dh = 360.0 - dh;
            }
            let hg = 1.0 - sstep(37.5, 60.0, dh);
            // chromatic blue OR bright near-neutral (overcast sky)
            let sg =
                sstep(0.08, 0.30, s).max(sstep(0.55, 0.8, v) * (1.0 - sstep(0.06, 0.18, s)) * 0.6);
            let lg = sstep(0.12, 0.45, v);
            let vert = 0.35 + 0.65 * (1.0 - my as f32 / mh as f32);
            d[my * mw + mx] = (hg * sg * lg * vert).clamp(0.0, 1.0);
        }
    }
    // two light box-blur passes so the matte isn't blocky
    for _ in 0..2 {
        let mut b = vec![0.0f32; mw * mh];
        for y in 0..mh {
            for x in 0..mw {
                let mut s = 0.0;
                let mut n = 0.0;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && nx < mw as i32 && ny < mh as i32 {
                            s += d[ny as usize * mw + nx as usize];
                            n += 1.0;
                        }
                    }
                }
                b[y * mw + x] = s / n;
            }
        }
        d = b;
    }
    Matte {
        w: mw,
        h: mh,
        data: d.iter().map(|&v| (v * 65535.0) as u16).collect(),
    }
}

/// heuristic vegetation matte for LR landscape "Vegetation" masks
/// (MaskSubType=0): green-dominant hue band × saturation × brightness,
/// no vertical prior (vegetation appears anywhere in frame).
fn veg_matte(lin: &[[f32; 3]], w: usize, h: usize) -> Matte {
    let mw = w.min(384).max(2);
    let mh = h.min(384).max(2);
    let mut d = vec![0.0f32; mw * mh];
    for my in 0..mh {
        for mx in 0..mw {
            let sx = (((mx as f32 + 0.5) / mw as f32) * w as f32) as usize % w;
            let sy = (((my as f32 + 0.5) / mh as f32) * h as f32) as usize % h;
            let (hh, s, v) = rgb_to_hsv(lin[sy * w + sx]);
            let deg = hh * 360.0;
            let mut dh = (deg - 120.0).abs();
            if dh > 180.0 {
                dh = 360.0 - dh;
            }
            let hg = 1.0 - sstep(45.0, 75.0, dh);
            let sg = sstep(0.15, 0.45, s);
            let lg = sstep(0.05, 0.25, v);
            d[my * mw + mx] = (hg * sg * lg).clamp(0.0, 1.0);
        }
    }
    for _ in 0..2 {
        let mut b = vec![0.0f32; mw * mh];
        for y in 0..mh {
            for x in 0..mw {
                let mut s = 0.0;
                let mut n = 0.0;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && nx < mw as i32 && ny < mh as i32 {
                            s += d[ny as usize * mw + nx as usize];
                            n += 1.0;
                        }
                    }
                }
                b[y * mw + x] = s / n;
            }
        }
        d = b;
    }
    Matte {
        w: mw,
        h: mh,
        data: d.iter().map(|&v| (v * 65535.0) as u16).collect(),
    }
}

/// lens blur source: a matte-res (<=192) gaussian-disc blurred copy of the
/// linear image. Radius scales with amount — mirrors gpu.rs lbl_main.
/// LR Lens Blur bokeh kernel — gaussian disc with shape variants:
/// aspect squashes the disc into an ellipse, shape=1 is a ring, positive
/// spherical aberration hardens the disc rim ("bubble"), HighlightsBoost
/// over-weights bright taps so speculars bloom into bokeh discs.
/// Mirrored by gpu.rs lbl_main — keep the math identical.
fn lens_blur_img(
    lin: &[[f32; 3]],
    w: usize,
    h: usize,
    amt: f32,
    o: [f32; 5],
    o2: [f32; 4],
) -> (Vec<[f32; 3]>, usize, usize) {
    let (boost, thr, aspect, shape, sa) = (o[0], o[1], o[2], o[3], o[4]);
    let (cat, cat_scale, rot, shape_detail) = (o2[0], o2[1], o2[2], o2[3]);
    let ang = rot * std::f32::consts::PI;
    let (rc, rs) = (ang.cos(), ang.sin());
    let mw = w.min(192).max(2);
    let mh = h.min(192).max(2);
    let r = (amt * 10.0).max(2.0);
    let r2 = r * r;
    let ri = r.ceil() as i32;
    // cat-eye: discs squash radially toward the frame edges; the scale
    // sets where the squash starts (LR CatEyeScale 0..100 → 0..1)
    let cat_start = (1.0 - cat_scale * 0.9).max(0.05);
    let ring_w = 0.15 / (1.0 + shape_detail * 2.0);
    let mut out = vec![[0.0f32; 3]; mw * mh];
    for my in 0..mh {
        for mx in 0..mw {
            let (cx, cy) = (
                (mx as f32 + 0.5) / mw as f32 - 0.5,
                (my as f32 + 0.5) / mh as f32 - 0.5,
            );
            let rd = (cx * cx + cy * cy).sqrt() * 1.4142;
            let edge = sstep(cat_start, 1.0, rd);
            let q = (1.0 - cat * edge * 0.85).max(0.15);
            let (dir_c, dir_s) = if rd > 1e-4 {
                (cx / (rd / 1.4142), cy / (rd / 1.4142))
            } else {
                (1.0f32, 0.0f32)
            };
            let mut acc = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for dy in -ri..=ri {
                for dx in -ri..=ri {
                    // rotate the kernel, then squash y by bokeh aspect
                    let rx = dx as f32 * rc - dy as f32 * rs;
                    let ry = dx as f32 * rs + dy as f32 * rc;
                    let ry = ry * (1.0 + aspect.clamp(-0.8, 0.8));
                    // cat-eye: compress the radial component
                    let d_rad = rx * dir_c + ry * dir_s;
                    let d_tan = -rx * dir_s + ry * dir_c;
                    let d2 = d_rad * d_rad / (q * q) + d_tan * d_tan;
                    if d2 > r2 {
                        continue;
                    }
                    let nx2 = mx as i32 + dx;
                    let ny2 = my as i32 + dy;
                    if nx2 < 0 || ny2 < 0 || nx2 >= mw as i32 || ny2 >= mh as i32 {
                        continue;
                    }
                    let mut wg = (-d2 / (0.45 * r2)).exp();
                    if shape > 0.5 {
                        // ring bokeh: band at ~0.72r, width from shape detail
                        let dr = d2.sqrt();
                        let dd = (dr - 0.72 * r) / (ring_w * r);
                        wg = (-dd * dd).exp();
                    } else if sa > 0.0 {
                        // hard-edged "bubble" discs: boost the rim
                        wg *= 1.0 + sa * 1.5 * sstep(0.55, 0.95, d2 / r2);
                    } else {
                        wg *= 1.0 + sa * 0.4;
                    }
                    let sx = (((nx2 as f32 + 0.5) / mw as f32) * w as f32) as usize % w;
                    let sy = (((ny2 as f32 + 0.5) / mh as f32) * h as f32) as usize % h;
                    let mut c = lin[sy * w + sx];
                    if boost > 0.001 {
                        let l = (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]).clamp(0.0, 1.0);
                        let bs = 1.0 + boost * 2.0 * sstep(thr, 1.0, l);
                        for k in 0..3 {
                            c[k] *= bs;
                        }
                    }
                    for k in 0..3 {
                        acc[k] += c[k] * wg;
                    }
                    wsum += wg;
                }
            }
            for k in 0..3 {
                out[my * mw + mx][k] = acc[k] / wsum.max(1e-6);
            }
        }
    }
    (out, mw, mh)
}

/// load an extracted LR AI-mask bitmap (PNG written next to the sidecar
/// during `lrcat` import) as a u16 luma matte
fn load_mask_bitmap(path: &str) -> Option<Matte> {
    let img = image::open(path).ok()?.to_luma16();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some(Matte {
        data: img.into_raw(),
        w: w as usize,
        h: h as usize,
    })
}

/// decode a PatchMatch offset-map PNG (16-bit RGB) → per-target-px
/// absolute source position in lin px. ch0,ch1 normalize into the spot's
/// search rect; LR's pm coords live in whole-image space so subtract the
/// frame origin.
fn load_pm_map(s: &crate::recipe::PmSpot) -> Option<(usize, usize, Vec<[f32; 2]>)> {
    let img = image::open(&s.file).ok()?.to_rgb16();
    let (pw, ph) = img.dimensions();
    if pw == 0 || ph == 0 {
        return None;
    }
    let (sw, sh) = (s.search[2] - s.search[0], s.search[3] - s.search[1]);
    let (pw, ph) = (pw as usize, ph as usize);
    let raw: Vec<[f32; 2]> = img
        .into_raw()
        .chunks_exact(3)
        .map(|px| {
            [
                (px[0] as f32 / 65535.0) * sw + s.search[0] - s.frame_org[0],
                (px[1] as f32 / 65535.0) * sh + s.search[1] - s.frame_org[1],
            ]
        })
        .collect();
    // 3×3 median on the offset field — isolated map outliers point at
    // unrelated content and show up as speckle (LR's gradient-domain apply
    // ignores them); coherent fragments survive the median.
    let mut src = raw.clone();
    let mut win = [0.0f32; 18];
    for y in 0..ph {
        for x in 0..pw {
            let mut n = 0;
            for oy in -1i32..=1 {
                for ox in -1i32..=1 {
                    let (xx, yy) = (
                        (x as i32 + ox).clamp(0, pw as i32 - 1) as usize,
                        (y as i32 + oy).clamp(0, ph as i32 - 1) as usize,
                    );
                    let v = raw[yy * pw + xx];
                    win[n] = v[0];
                    win[9 + n] = v[1];
                    n += 1;
                }
            }
            for ch in 0..2 {
                let mut w8 = [0.0f32; 9];
                w8.copy_from_slice(&win[ch * 9..ch * 9 + 9]);
                w8.sort_by(|a, b| a.total_cmp(b));
                src[y * pw + x][ch] = w8[4];
            }
        }
    }
    Some((pw, ph, src))
}

/// bilinear sample of the AI subject matte at normalized coords; 0 when empty
/// Fold multi-member LR corrections into combined mattes. Returns
/// (leader_window_index → (combined matte, union origin in mask space),
/// per-window skip flags). A group folds only when every member resolved
/// a bitmap — otherwise the members keep their independent windows.
fn fold_mask_groups(
    windows: &[crate::recipe::PowerWindow],
    loaded: &[Option<Matte>],
) -> (
    std::collections::HashMap<usize, (Matte, [f32; 2])>,
    [bool; 16],
) {
    let mut skip_win = [false; 16];
    let mut merged = std::collections::HashMap::new();
    let mut groups: std::collections::BTreeMap<u32, Vec<usize>> = Default::default();
    for (i, w) in windows.iter().enumerate() {
        if w.group != 0 && i < loaded.len() && loaded[i].is_some() {
            groups.entry(w.group).or_default().push(i);
        }
    }
    for idxs in groups.values() {
        if idxs.len() < 2 || !idxs.iter().all(|&i| loaded[i].is_some()) {
            continue;
        }
        let (aw, ah) = {
            let w0 = &windows[idxs[0]];
            (w0.mask_area[0], w0.mask_area[1])
        };
        // member union bbox in leader mask space (scale orgs if a member
        // was stored against a different WholeImageArea)
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &i in idxs {
            let w = &windows[i];
            let m = loaded[i].as_ref().unwrap();
            let sx = aw / w.mask_area[0].max(1.0);
            let sy = ah / w.mask_area[1].max(1.0);
            x0 = x0.min(w.mask_org[0] * sx);
            y0 = y0.min(w.mask_org[1] * sy);
            x1 = x1.max((w.mask_org[0] + m.w as f32) * sx);
            y1 = y1.max((w.mask_org[1] + m.h as f32) * sy);
        }
        let cw = (x1 - x0).ceil().max(1.0) as usize;
        let ch = (y1 - y0).ceil().max(1.0) as usize;
        let mut acc = vec![0u16; cw * ch];
        // application order: base/add members union first regardless of
        // list order, then subtract/intersect members in list order
        let order: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| windows[i].blend == 0)
            .chain(idxs.iter().copied().filter(|&i| windows[i].blend != 0))
            .collect();
        for (k, &i) in order.iter().enumerate() {
            let w = &windows[i];
            let m = loaded[i].as_ref().unwrap();
            let sx = aw / w.mask_area[0].max(1.0);
            let sy = ah / w.mask_area[1].max(1.0);
            for y in 0..ch {
                for x in 0..cw {
                    // canvas px → member px → normalized sample
                    // (pixel-centre convention, matching matte_bil)
                    let mx = ((x0 + x as f32) / sx - w.mask_org[0] + 0.5) / m.w as f32;
                    let my = ((y0 + y as f32) / sy - w.mask_org[1] + 0.5) / m.h as f32;
                    let mut v = if (0.0..1.0).contains(&mx) && (0.0..1.0).contains(&my) {
                        matte_bil(m, mx, my)
                    } else {
                        0.0
                    };
                    if w.invert {
                        v = 1.0 - v;
                    }
                    let a = acc[y * cw + x] as f32 / 65535.0;
                    let nv = if k == 0 || w.blend == 0 {
                        a + v - a * v // add: screen union
                    } else if w.blend == 1 {
                        a * (1.0 - v) // subtract
                    } else {
                        a * v // intersect
                    };
                    acc[y * cw + x] = (nv * 65535.0).clamp(0.0, 65535.0) as u16;
                }
            }
        }
        merged.insert(
            idxs[0],
            (
                Matte {
                    w: cw,
                    h: ch,
                    data: acc,
                },
                [x0, y0],
            ),
        );
        for &i in &idxs[1..] {
            skip_win[i] = true;
        }
    }
    (merged, skip_win)
}

fn matte_bil(m: &Matte, nx: f32, ny: f32) -> f32 {
    if m.w < 2 || m.h < 2 || m.data.len() < m.w * m.h {
        return 0.0;
    }
    let fx = nx * m.w as f32 - 0.5;
    let fy = ny * m.h as f32 - 0.5;
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let at = |x: i32, y: i32| -> f32 {
        let x = x.clamp(0, m.w as i32 - 1) as usize;
        let y = y.clamp(0, m.h as i32 - 1) as usize;
        m.data[y * m.w + x] as f32 / 65535.0
    };
    let a = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
    let b = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
    a * (1.0 - ty) + b * ty
}

/// single-channel bilinear sample (for CA-corrected per-channel taps)
fn bilin_ch(lin: &[[f32; 3]], w: usize, h: usize, sx: f32, sy: f32, ch: usize) -> f32 {
    let x0 = sx.floor() as i32;
    let y0 = sy.floor() as i32;
    if x0 < 0 || y0 < 0 || x0 + 1 >= w as i32 || y0 + 1 >= h as i32 {
        if x0 >= 0 && y0 >= 0 && (x0 as usize) < w && (y0 as usize) < h {
            return lin[y0 as usize * w + x0 as usize][ch];
        }
        return 0.0;
    }
    let tx = sx - x0 as f32;
    let ty = sy - y0 as f32;
    let (x0, y0) = (x0 as usize, y0 as usize);
    let a = lin[y0 * w + x0][ch] * (1.0 - tx) + lin[y0 * w + x0 + 1][ch] * tx;
    let b = lin[(y0 + 1) * w + x0][ch] * (1.0 - tx) + lin[(y0 + 1) * w + x0 + 1][ch] * tx;
    a * (1.0 - ty) + b * ty
}

/// Measure lateral chromatic aberration on a linear RGB buffer: for each
/// strong radial edge, find where the R / B edge sits along the radial
/// direction relative to G (centroid of the radial derivative profile).
/// Returns per-channel scale factors k_r, k_b such that sampling channel C
/// at radius r·(1−k_C) removes the measured displacement — ~0 on CA-free
/// images, so this never *adds* fringing the way a fixed correction does.
fn estimate_ca_px(lin: &[[f32; 3]], w: usize, h: usize) -> (f32, f32) {
    let cx = w as f32 * 0.5;
    let cy = h as f32 * 0.5;
    let half = w.max(h) as f32 * 0.5;
    let step = (w.max(h) / 700).max(2);
    // (r_px, delta_px) per channel 0=R 1=B
    let mut obs = [Vec::<(f32, f32)>::new(), Vec::<(f32, f32)>::new()];
    for y in (6..h.saturating_sub(6)).step_by(step) {
        for x in (6..w.saturating_sub(6)).step_by(step) {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let r_px = dx.hypot(dy);
            if r_px < half * 0.25 {
                continue; // CA vanishes toward the optical centre
            }
            let (ux, uy) = (dx / r_px, dy / r_px);
            let at =
                |ch: usize, o: f32| bilin_ch(lin, w, h, x as f32 + ux * o, y as f32 + uy * o, ch);
            // weak-edge gate on green first (cheap)
            if (at(1, 1.0) - at(1, -1.0)).abs() < 0.015 {
                continue;
            }
            // centroid of |d/dr| over s in [-3,3]
            let centroid = |ch: usize| {
                let (mut num, mut den) = (0.0f32, 0.0f32);
                let mut s = -2.5f32;
                while s <= 3.0 {
                    let d = (at(ch, s + 0.5) - at(ch, s - 0.5)).abs();
                    num += s * d;
                    den += d;
                    s += 0.5;
                }
                if den < 0.02 {
                    None
                } else {
                    Some((num / den, den))
                }
            };
            let Some((pg, dg)) = centroid(1) else {
                continue;
            };
            for (ch, slot) in [(0usize, 0usize), (2usize, 1usize)] {
                if let Some((pc, dc)) = centroid(ch) {
                    let d = pc - pg;
                    // edge must be sharp in both channels and the offset sane
                    if d.abs() < 2.5 && dc > dg * 0.15 {
                        obs[slot].push((r_px, d));
                    }
                }
            }
        }
    }
    let fit = |v: &mut Vec<(f32, f32)>| -> f32 {
        if v.len() < 100 {
            return 0.0;
        }
        // bin by radius, median per bin, then slope through origin
        let mut bins: [Vec<f32>; 8] = Default::default();
        let mut rmax = 1.0f32;
        for &(r, _) in v.iter() {
            rmax = rmax.max(r);
        }
        for &(r, d) in v.iter() {
            bins[((r / rmax) * 7.999) as usize].push(d);
        }
        let (mut num, mut den, mut nb) = (0.0f64, 0.0f64, 0usize);
        for (i, b) in bins.iter_mut().enumerate() {
            if b.len() < 5 {
                continue;
            }
            b.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = b[b.len() / 2];
            let rm = (i as f32 + 0.5) / 8.0 * rmax;
            num += (med * rm) as f64 * b.len() as f64;
            den += (rm * rm) as f64 * b.len() as f64;
            nb += 1;
        }
        if nb < 3 || den == 0.0 {
            return 0.0;
        }
        ((num / den) as f32).clamp(-0.02, 0.02)
    };
    let (kr, kb) = (fit(&mut obs[0]), fit(&mut obs[1]));
    (kr, kb)
}

/// CA estimate for the mosaic path (GPU): cheap sparse demosaic via
/// `demosaic_pixel`, then the shared edge estimator.
pub fn estimate_ca_mosaic(m: &Mosaic) -> (f32, f32) {
    let norm = norm_factors(m);
    // ~1MP measurement grid, keeping CFA phase alignment
    let stride = ((m.w.max(m.h) as f32 / 1000.0).ceil() as usize).max(1) * m.cfa.h.max(1);
    let vw = m.w / stride;
    let vh = m.h / stride;
    if vw < 16 || vh < 16 {
        return (0.0, 0.0);
    }
    let mut lin = vec![[0.0f32; 3]; vw * vh];
    for vy in 0..vh {
        for vx in 0..vw {
            lin[vy * vw + vx] = demosaic_pixel(m, vx, vy, stride, &norm);
        }
    }
    estimate_ca_px(&lin, vw, vh)
}

/// beauty (skin-masked smoothing) + deband (flat-gradient smoothing)
fn beauty_deband(lin: &[[f32; 3]], w: usize, h: usize, beauty: f32, deband: f32) -> Vec<[f32; 3]> {
    let chans: Vec<Vec<f32>> = (0..3).map(|c| lin.iter().map(|p| p[c]).collect()).collect();
    let mut out = vec![[0.0f32; 3]; lin.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let c = lin[i];
            let m = [
                box3(&chans[0], w, h, x, y),
                box3(&chans[1], w, h, x, y),
                box3(&chans[2], w, h, x, y),
            ];
            let skin = if beauty > 0.0 { skin_mask(c) } else { 0.0 };
            let flat = {
                let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                let lm = 0.2126 * m[0] + 0.7152 * m[1] + 0.0722 * m[2];
                1.0 - sstep(0.004, 0.03, (l - lm).abs())
            };
            let wgt = (beauty * skin + deband * flat).min(1.0);
            for ch in 0..3 {
                out[i][ch] = c[ch] * (1.0 - wgt) + m[ch] * wgt;
            }
        }
    }
    out
}

/// lens glow: extract highlights, wide box blur, add back
fn glow_lin(lin: &[[f32; 3]], w: usize, h: usize, amt: f32) -> Vec<[f32; 3]> {
    let hi: Vec<f32> = lin
        .iter()
        .map(|c| {
            let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            sstep(0.55, 0.9, l)
        })
        .collect();
    let mut out = Vec::with_capacity(lin.len());
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut m = [0.0f32; 3];
            let mut n = 0u32;
            for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        let j = ny as usize * w + nx as usize;
                        for c in 0..3 {
                            m[c] += lin[j][c] * hi[j];
                        }
                        n += 1;
                    }
                }
            }
            let f = amt * 0.8 / n.max(1) as f32;
            out.push([
                lin[i][0] + m[0] * f,
                lin[i][1] + m[1] * f,
                lin[i][2] + m[2] * f,
            ]);
        }
    }
    out
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
    // same integer hash as gpu.rs `hash` so grain is identical on both paths
    let mut h = (x as u32)
        .wrapping_mul(2654435761)
        .wrapping_add((y as u32).wrapping_mul(2246822519))
        .wrapping_add(((seed + 1.0) as u32).wrapping_mul(3266489917));
    h = (h ^ (h >> 13)).wrapping_mul(1103515245);
    h ^= h >> 16;
    h as f32 * (1.0 / 4294967296.0)
}

/// Video-style scopes computed from rendered rgba8.
/// `wave`: per-channel column histograms, wave[c*65536 + v*256 + col]
/// `vec`: YCbCr chroma plane, vec[cr*256 + cb]
/// `cie`: CIE xy chromaticity plane, cie[yi*256 + xi]
pub fn scopes(rgba8: &[u8], w: u32, h: u32, wave: &mut [u32], vec: &mut [u32], cie: &mut [u32]) {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return;
    }
    // stride so we touch at most ~512K pixels regardless of input size
    let step = ((w * h) as f64 / 524288.0).sqrt().ceil().max(1.0) as usize;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = (y * w + x) * 4;
            let (r, g, b) = (
                rgba8[i] as f32 / 255.0,
                rgba8[i + 1] as f32 / 255.0,
                rgba8[i + 2] as f32 / 255.0,
            );
            let col = x * 255 / w;
            for (c, v) in [r, g, b].iter().enumerate() {
                let bin = (v.clamp(0.0, 1.0) * 255.0) as usize;
                wave[c * 65536 + bin * 256 + col] += 1;
            }
            // BT.601 chroma for the vectorscope
            let cb = (-0.168736 * r - 0.331264 * g + 0.5 * b) * 255.0 + 128.0;
            let cr = (0.5 * r - 0.418688 * g - 0.081312 * b) * 255.0 + 128.0;
            let cbi = cb.clamp(0.0, 255.0) as usize;
            let cri = cr.clamp(0.0, 255.0) as usize;
            vec[cri * 256 + cbi] += 1;
            // sRGB -> CIE XYZ -> xy chromaticity
            let lr = if r <= 0.04045 {
                r / 12.92
            } else {
                ((r + 0.055) / 1.055).powf(2.4)
            };
            let lg = if g <= 0.04045 {
                g / 12.92
            } else {
                ((g + 0.055) / 1.055).powf(2.4)
            };
            let lb = if b <= 0.04045 {
                b / 12.92
            } else {
                ((b + 0.055) / 1.055).powf(2.4)
            };
            let xx = 0.4124 * lr + 0.3576 * lg + 0.1805 * lb;
            let yy = 0.2126 * lr + 0.7152 * lg + 0.0722 * lb;
            let zz = 0.0193 * lr + 0.1192 * lg + 0.9505 * lb;
            let s = xx + yy + zz;
            if s > 1e-6 {
                let xi = (xx / s * 255.0).clamp(0.0, 255.0) as usize;
                let yi = (yy / s * 255.0).clamp(0.0, 255.0) as usize;
                cie[yi * 256 + xi] += 1;
            }
        }
    }
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
    // cap, not target: small inputs keep their size
    if w.max(h) <= max_px {
        return (w.max(1), h.max(1));
    }
    let s = max_px as f32 / w.max(h) as f32;
    (
        ((w as f32 * s).round() as u32).max(1),
        ((h as f32 * s).round() as u32).max(1),
    )
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
/// guided-filter luma NR (single-pass "guided-lite"): q = mean + a·(l−mean)
/// with a = var/(var+eps) — flat areas get pulled to the mean, edges keep
/// their value. Applied as a per-channel exp2 gain so detail survives.
/// Identical math to gpu.rs `nr_main` luma path — keep in sync.
fn guided_nr_lum(lin: &[[f32; 3]], w: usize, h: usize, amt: f32) -> Vec<[f32; 3]> {
    let guide: Vec<f32> = lin
        .iter()
        .map(|c| (0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2] + 0.01).log2())
        .collect();
    let rad = (1.0 + amt * 4.0).round() as i32;
    let eps = 0.002 + amt * amt * 0.25;
    let mut out = vec![[0.0f32; 3]; lin.len()];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let mut mean = 0.0f32;
            let mut sq = 0.0f32;
            let mut n = 0u32;
            for dy in -rad..=rad {
                for dx in -rad..=rad {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        let v = guide[ny as usize * w + nx as usize];
                        mean += v;
                        sq += v * v;
                        n += 1;
                    }
                }
            }
            mean /= n.max(1) as f32;
            let var = (sq / n.max(1) as f32 - mean * mean).max(0.0);
            let a = var / (var + eps);
            let q = mean + a * (guide[i] - mean);
            let g = ((q - guide[i]) * amt).exp2();
            for c in 0..3 {
                out[i][c] = lin[i][c] * g;
            }
        }
    }
    out
}

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
    sharp_mask: f32,
    sharp_radius: f32,
    sharp_detail: f32,
    texture: f32,
) -> Vec<[f32; 3]> {
    let chans: Vec<Vec<f32>> = (0..3).map(|c| lin.iter().map(|p| p[c]).collect()).collect();
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
            let lum =
                (0.2126 * lin[i][0] + 0.7152 * lin[i][1] + 0.0722 * lin[i][2]).clamp(0.0, 1.0);
            let mid = 4.0 * lum * (1.0 - lum);
            // LR "Masking": the unsharp delta itself is the edge signal —
            // scale it down where the luma delta is flat.
            let sd = if sharpen != 0.0 {
                // radius: interpolate between the integer box radii
                let rf = sharp_radius.clamp(1.0, 3.0);
                let ra = rf.floor() as i32;
                let rb = (ra + 1).min(3);
                let rt = rf - ra as f32;
                let mut d = [0.0f32; 3];
                for c in 0..3 {
                    let da = lin[i][c] - boxn(&chans[c], x, y, ra);
                    let db = lin[i][c] - boxn(&chans[c], x, y, rb);
                    let dcoarse = da + (db - da) * rt;
                    // detail: blend the coarse-band and fine 3x3 deltas
                    d[c] = if sharp_detail > 0.0 {
                        let dfine = lin[i][c] - boxn(&chans[c], x, y, 1);
                        dcoarse * (1.0 - sharp_detail) + dfine * sharp_detail
                    } else {
                        dcoarse
                    };
                }
                let edge = (0.2126 * d[0] + 0.7152 * d[1] + 0.0722 * d[2]).abs();
                let mscale = 1.0 - sharp_mask * (1.0 - sstep(0.006, 0.05, edge));
                d.map(|v| v * mscale)
            } else {
                [0.0; 3]
            };
            for c in 0..3 {
                let mut v = lin[i][c];
                if sharpen != 0.0 {
                    v += sharpen * 0.8 * sd[c];
                }
                if clarity != 0.0 {
                    v += clarity * 0.6 * mid * (lin[i][c] - boxn(&chans[c], x, y, 2));
                }
                // LR Texture: same midtone-weighted local contrast as clarity
                // but at a finer (3x3) scale and lower gain
                if texture != 0.0 {
                    v += texture * 0.45 * mid * (lin[i][c] - boxn(&chans[c], x, y, 1));
                }
                out[i][c] = v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Auto CA: a channel magnified radially by (1+k) reads back as k.
    /// Concentric-ring edges give radial gradient everywhere; R displaced
    /// outward by k·r_px must yield ca_r ≈ k, and a clean image ≈ 0.
    #[test]
    fn estimate_ca_recovers_known_shift() {
        let (w, h) = (400usize, 300usize);
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        // square wave: flat plateaus with impulsive radial edges every 12px
        let ring = |r: f32| -> f32 {
            if (r / 24.0).fract() < 0.5 {
                0.2
            } else {
                0.9
            }
        };
        let mut lin = vec![[0.0f32; 3]; w * h];
        for y in 0..h {
            for x in 0..w {
                let r = (x as f32 - cx).hypot(y as f32 - cy);
                // R sampled at a slightly smaller radius = magnified outward
                lin[y * w + x] = [ring(r * 0.994), ring(r), ring(r)];
            }
        }
        let (kr, kb) = estimate_ca_px(&lin, w, h);
        assert!((kr - 0.006).abs() < 0.002, "kr={kr}");
        assert!(kb.abs() < 0.002, "kb={kb}");
    }

    /// LR person-part masks (kind "part", flag 64): the subject matte is
    /// gated by a soft ellipse prior — full matte → mask ≈ ellipse, zero
    /// matte → mask 0 even inside the prior.
    #[test]
    fn part_mask_gates_subject_matte_by_region() {
        let m = Matte {
            w: 8,
            h: 8,
            data: vec![65535u16; 64],
        };
        // flag 64, centre (.5,.3), radii (.1,.07), rot 0, soft .4, mid 0
        let mut w = [0.0f32; 20];
        w[0] = 64.0;
        w[1] = 0.5;
        w[2] = 0.3;
        w[3] = 0.1;
        w[4] = 0.07;
        w[6] = 0.4;
        w[10] = 1.0;
        // centre of the prior → mask ≈ 1
        assert!(
            window_mask(
                &w,
                &[0.0; 6],
                0.5,
                0.3,
                [0.5; 3],
                Some(&m),
                None,
                None,
                &[],
                &[]
            ) > 0.9
        );
        // far outside the ellipse → ≈ 0
        assert!(
            window_mask(
                &w,
                &[0.0; 6],
                0.5,
                0.8,
                [0.5; 3],
                Some(&m),
                None,
                None,
                &[],
                &[]
            ) < 0.05
        );
        // no subject matte → no mask at all
        assert_eq!(
            window_mask(
                &w,
                &[0.0; 6],
                0.5,
                0.3,
                [0.5; 3],
                None,
                None,
                None,
                &[],
                &[]
            ),
            0.0
        );
    }

    /// LR catalog AI-mask bitmap (flag 1024): the WMatte affine maps
    /// post-crop nx,ny into the stored TIFF bbox; outside it the mask is 0
    /// and the heuristic kind bits are ignored entirely.
    #[test]
    fn window_mask_bitmap_matte() {
        // 2x2 matte: left column 0, right column full
        let wm = WMatte {
            m: Matte {
                w: 2,
                h: 2,
                data: vec![0, 65535, 0, 65535],
            },
            ux: 1.0,
            uy: 0.0,
            uc: 0.0,
            vx: 0.0,
            vy: 1.0,
            vc: 0.0,
        };
        let mut w = [0.0f32; 24];
        w[0] = 1024.0 + 32.0; // bitmap + a leftover "sky" bit (ignored)
        w[21] = 0.0;
        let call = |nx, ny| {
            window_mask(
                &w,
                &[0.0; 6],
                nx,
                ny,
                [0.5; 3],
                None,
                None,
                None,
                &[w],
                std::slice::from_ref(&wm),
            )
        };
        assert!(call(0.9, 0.5) > 0.9, "right side of bitmap");
        assert!(call(0.1, 0.5) < 0.1, "left side of bitmap");
        // rotated affine (90°: u = ny): the same matte gates by ny instead
        let wm_rot = WMatte {
            m: wm.m.clone(),
            ux: 0.0,
            uy: 1.0,
            uc: 0.0,
            vx: -1.0,
            vy: 0.0,
            vc: 1.0,
        };
        let call_rot = |nx, ny| {
            window_mask(
                &w,
                &[0.0; 6],
                nx,
                ny,
                [0.5; 3],
                None,
                None,
                None,
                &[w],
                std::slice::from_ref(&wm_rot),
            )
        };
        assert!(call_rot(0.5, 0.9) > 0.9, "bottom edge maps u→1");
        assert!(call_rot(0.5, 0.1) < 0.1, "top edge maps u→0");
        // affine pushing the sample outside [0,1] → mask 0 (bbox edge)
        let wm_off = WMatte {
            m: wm.m.clone(),
            ux: 1.0,
            uy: 0.0,
            uc: -2.0, // bbox starts off-frame right
            vx: 0.0,
            vy: 1.0,
            vc: 0.0,
        };
        assert_eq!(
            window_mask(
                &w,
                &[0.0; 6],
                0.5,
                0.5,
                [0.5; 3],
                None,
                None,
                None,
                &[w],
                std::slice::from_ref(&wm_off),
            ),
            0.0
        );
        // invert flag still applies on top of the bitmap
        let mut wi = w;
        wi[0] += 2.0;
        assert!(
            window_mask(
                &wi,
                &[0.0; 6],
                0.1,
                0.5,
                [0.5; 3],
                None,
                None,
                None,
                &[w],
                std::slice::from_ref(&wm),
            ) > 0.9
        );
    }

    /// A correction's mask members fold by MaskBlendMode before the local
    /// adjustment applies once: base sky matte minus a subject matte
    /// (blend 1) leaves a hole where the subject is; the non-leader
    /// window is skipped so it cannot double-apply.
    #[test]
    fn fold_mask_groups_add_subtract() {
        use crate::recipe::PowerWindow;
        let mk = |blend: i32, invert: bool| PowerWindow {
            mask_file: "x.png".into(),
            mask_area: [4.0, 4.0],
            group: 1,
            blend,
            invert,
            ..Default::default()
        };
        // member 0: full 4x4 matte at origin (0,0); member 1: 2x2 matte
        // at origin (1,1) — subtracted
        let mut w0 = mk(0, false);
        w0.mask_org = [0.0, 0.0];
        let mut w1 = mk(1, false);
        w1.mask_org = [1.0, 1.0];
        let windows = vec![w0, w1];
        let loaded = vec![
            Some(Matte {
                w: 4,
                h: 4,
                data: vec![65535; 16],
            }),
            Some(Matte {
                w: 2,
                h: 2,
                data: vec![65535; 4],
            }),
        ];
        let (merged, skip) = fold_mask_groups(&windows, &loaded);
        assert!(skip[1], "subject member folded away");
        let (m, org) = merged.get(&0).expect("leader keeps merged matte");
        assert_eq!((m.w, m.h, *org), (4, 4, [0.0, 0.0]));
        for y in 0..4 {
            for x in 0..4 {
                let hole = (1..3).contains(&x) && (1..3).contains(&y);
                let v = m.data[y * 4 + x] as f32 / 65535.0;
                if hole {
                    assert!(v < 0.05, "subject subtracted at {x},{y}: {v}");
                } else {
                    assert!(v > 0.95, "sky kept at {x},{y}: {v}");
                }
            }
        }
        // inverted subject member: subtract(1 - matte) = keep only subject
        let mut w1i = mk(1, true);
        w1i.mask_org = [1.0, 1.0];
        let windows = vec![windows[0].clone(), w1i];
        let (merged, _) = fold_mask_groups(&windows, &loaded);
        let (m, _) = merged.get(&0).unwrap();
        assert!(
            m.data[0] as f32 / 65535.0 < 0.05,
            "outside inverted subject = 0"
        );
        assert!(
            m.data[1 * 4 + 1] as f32 / 65535.0 > 0.95,
            "inside inverted subject = 1"
        );
        // ungrouped windows never fold
        let mut solo = mk(0, false);
        solo.group = 0;
        let (merged, skip) = fold_mask_groups(&[solo.clone(), solo], &loaded);
        assert!(merged.is_empty() && !skip[0] && !skip[1]);
    }

    /// LR PatchMatch remove: the defect is replaced by the patchmap's
    /// source pixels (frequency-separated — target keeps its low-freq,
    /// so a small defect inside a larger patch vanishes against a
    /// smooth background).
    #[test]
    fn pm_heal_uses_patchmap_sources() {
        let (w, h) = (32, 32);
        // smooth horizontal gradient scene
        let mut lin: Vec<[f32; 3]> = (0..w * h)
            .map(|i| [0.2 + (i % w) as f32 * 0.01; 3])
            .collect();
        // 2x2 bright defect at (12..14, 12..14)
        for y in 12..14 {
            for x in 12..14 {
                lin[y * w + x] = [0.95; 3];
            }
        }
        let (pw, ph) = (8, 8);
        // patch px (x,y) pulls from (x+14, y) — 4 px to the right
        let src: Vec<[f32; 2]> = (0..pw * ph)
            .map(|i| [10.0 + (i % pw) as f32 + 4.0, 10.0 + (i / pw) as f32])
            .collect();
        let pm = PmMap {
            src,
            pw,
            ph,
            dst: [10.0, 10.0, 18.0, 18.0],
            ell: [14.0, 14.0, 4.0, 4.0],
            opacity: 1.0,
        };
        pm_heal_lin(&mut lin, w, h, &[pm], [32.0, 32.0]);
        // defect centre now follows the source region (v ≈ 0.34 + small
        // low-freq offset), nowhere near the 0.95 defect
        let c = lin[13 * w + 13];
        for v in c {
            assert!(v < 0.6, "defect healed toward source level, got {v}");
        }
        // a pixel far outside the ellipse is untouched
        assert_eq!(lin[2 * w + 2], [0.22; 3]);
        // empty map list is a no-op
        let mut lin2 = vec![[0.3f32; 3]; 4];
        pm_heal_lin(&mut lin2, 2, 2, &[], [0.0, 0.0]);
        assert_eq!(lin2[0], [0.3; 3]);
    }

    /// LR Point Colors: 19-float rows decode in the alphabetical layout
    /// (the LrDevelopController swatch dump), sentinel rows are skipped,
    /// and the trapezoid mask weights the shift by proximity to the
    /// sampled colour.
    #[test]
    fn point_color_normalize_and_apply() {
        // the community-dumped swatch: SrcHue=4.12705 (0..6), SrcSat=.92,
        // SrcLum=.84, amount .5, ranges around the sample
        let mut v = [-1.0f32; 19];
        v[0] = 0.333;
        v[1] = 0.0;
        v[2] = 0.667;
        v[3] = 1.0; // HueRange LF,LN,UF,UN
        v[4] = 0.0; // HueShift
        v[5] = 0.745889;
        v[6] = 0.195889;
        v[7] = 1.0;
        v[8] = 1.0; // LumRange
        v[9] = 0.0; // LumScale
        v[10] = 0.5; // RangeAmount
        v[11] = 0.740754;
        v[12] = 0.190754;
        v[13] = 1.0;
        v[14] = 1.0; // SatRange
        v[15] = 0.5; // SatScale
        v[16] = 4.12705;
        v[17] = 0.839618;
        v[18] = 0.920754;
        let sw = pc_normalize(&v).unwrap();
        assert!((sw.src[0] - 4.12705 / 6.0).abs() < 1e-5);
        assert!((sw.src[1] - 0.920754).abs() < 1e-5);
        assert!((sw.src[2] - 0.839618).abs() < 1e-5);
        assert!((sw.amt - 0.5).abs() < 1e-6);
        // apply: a saturated pixel near the sample gets its saturation
        // scaled; a desaturated pixel is untouched
        let near = pc_apply(
            hsv_to_rgb(0.69, 0.9, 0.85),
            &PcSwatch {
                shift: [0.0, 0.5, 0.0],
                amt: 0.5,
                ..sw
            },
            0.0,
        );
        let (h2, s2, _) = rgb_to_hsv(near);
        assert!(s2 > 0.9, "s2 {s2}");
        let _ = h2;
        let grey = pc_apply([0.5, 0.5, 0.5], &sw, 0.0);
        assert!((grey[0] - 0.5).abs() < 1e-4);
        // sentinel row
        assert!(pc_normalize(&[-1.0; 19]).is_none());
        // logical-order row (src first, natural ranges) also decodes
        let mut b = [-1.0f32; 19];
        b[0] = 3.0;
        b[1] = 0.5;
        b[2] = 0.5;
        b[6] = 0.5;
        for i in [7usize, 11, 15] {
            b[i] = 0.0;
            b[i + 1] = 0.25;
            b[i + 2] = 0.75;
            b[i + 3] = 1.0;
        }
        let sw2 = pc_normalize(&b).unwrap();
        assert!((sw2.src[0] - 0.5).abs() < 1e-5);
        // real row from Adobe's "Adaptive - Landscape/Autumn01" preset
        // (layout B: src(h06,s,l) + shifts(h,s,l) + amount + three ranges)
        let v2: [f32; 19] = [
            0.648015, 0.915783, 0.651444, 0.0, 0.05, 0.11, 0.5, 0.0, 0.17, 0.67, 1.0, 0.185782,
            0.735783, 1.0, 1.0, 0.097473, 0.647473, 1.0, 1.0,
        ];
        let sw3 = pc_normalize(&v2).unwrap();
        assert!((sw3.src[0] - 0.648015 / 6.0).abs() < 1e-5);
        assert!((sw3.src[1] - 0.915783).abs() < 1e-5);
        assert!((sw3.src[2] - 0.651444).abs() < 1e-5);
        assert!((sw3.shift[1] - 0.05).abs() < 1e-6);
        assert!((sw3.shift[2] - 0.11).abs() < 1e-6);
        // src colour sits inside its own sat/lum full zones
        assert!(sw3.sr[1] <= sw3.src[1] && sw3.src[1] <= sw3.sr[3]);
        assert!(sw3.lr[1] <= sw3.src[2] && sw3.src[2] <= sw3.lr[3]);
        // Color Variance: var<0 pulls in-range colours toward the "after"
        // swatch colour, var>0 pushes them away; var=0 is a plain shift.
        let swv = PcSwatch {
            src: [0.1, 0.8, 0.6],
            shift: [0.0, 0.0, 0.0],
            amt: 0.5,
            hr: [0.0, 0.0, 1.0, 1.0],
            sr: [0.0, 0.0, 1.0, 1.0],
            lr: [0.0, 0.0, 1.0, 1.0],
        };
        let px = hsv_to_rgb(0.2, 0.8, 0.6); // in-range, off the swatch src
        let a = hsv_to_rgb(0.1, 0.8, 0.6); // the "after" colour (no shift)
        let out_neg = pc_apply(px, &swv, -0.5);
        let out_pos = pc_apply(px, &swv, 0.5);
        for c in 0..3 {
            let d_src = (px[c] - a[c]).abs();
            assert!(
                (out_neg[c] - a[c]).abs() < d_src + 1e-6,
                "var<0 should pull toward the after colour"
            );
            assert!(
                (out_pos[c] - a[c]).abs() > d_src - 1e-6,
                "var>0 should push away from the after colour"
            );
        }
    }

    #[test]
    fn trap01_shape() {
        assert_eq!(trap01(0.0, 0.2, 0.4, 0.6, 0.8), 0.0);
        assert_eq!(trap01(0.5, 0.2, 0.4, 0.6, 0.8), 1.0);
        assert!((trap01(0.3, 0.2, 0.4, 0.6, 0.8) - 0.5).abs() < 1e-6);
        assert!((trap01(0.7, 0.2, 0.4, 0.6, 0.8) - 0.5).abs() < 1e-6);
        // degenerate: zero-width outer edge = step
        assert_eq!(trap01(0.3, 0.4, 0.4, 0.6, 0.6), 0.0);
        assert_eq!(trap01(0.5, 0.4, 0.4, 0.6, 0.6), 1.0);
    }

    /// minimal mosaic fixture for build_params tests
    fn fake_mosaic(make: &str, iso: f32) -> Mosaic {
        Mosaic {
            data: Vec::new(),
            raw_w: 0,
            raw_h: 0,
            left: 0,
            top: 0,
            w: 0,
            h: 0,
            black: [0.0; 4],
            maximum: 16383,
            baseline_exposure: 0.0,
            linear_max: [0; 4],
            cam_mul: [1.0; 4],
            cam_xyz: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0; 3]],
            rgb_cam: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            pre_mul: [1.0; 4],
            cfa: crate::decode::CfaPattern {
                w: 2,
                h: 2,
                cells: vec![0, 1, 1, 2],
            },
            info: crate::decode::CameraInfo {
                make: make.to_string(),
                model: String::new(),
                lens: String::new(),
                iso,
                shutter: 0.0,
                aperture: 0.0,
                focal: 0.0,
                timestamp: 0,
                flip: 0,
            },
            dng_ops: None,
            linear: None,
        }
    }

    /// Sony expanded-LOW ISO: ACR pulls the baseline by log2(iso/100) — the
    /// sensor exposes at native ISO and the file is tagged down. Verified
    /// against LR previews of ISO50 files (MAE ~4x better with the pull).
    /// Gated to Sony so native-ISO64 bodies (Nikon D850, Fuji GFX) keep 1.0.
    #[test]
    fn iso_pull_sony_expanded_low() {
        let r = Recipe::default();
        let s50 = build_params(&fake_mosaic("SONY", 50.0), &r, None);
        assert!(
            (s50.exposure_mul - 0.5).abs() < 0.01,
            "{}",
            s50.exposure_mul
        );
        // native base ISO and non-Sony sub-100 are untouched
        let s100 = build_params(&fake_mosaic("SONY", 100.0), &r, None);
        assert!(
            (s100.exposure_mul - 1.0).abs() < 0.01,
            "{}",
            s100.exposure_mul
        );
        let n64 = build_params(&fake_mosaic("NIKON", 64.0), &r, None);
        assert!(
            (n64.exposure_mul - 1.0).abs() < 0.01,
            "{}",
            n64.exposure_mul
        );
    }

    /// ACR PV2012 tone compensation: catalog images get an empirical
    /// ours→LR transfer folded into the recipe LUT — strong toe lift, mild
    /// shoulder. PV11 gets it full strength, PV15 half, and it tapers with
    /// |Exposure2012| (a large user push already relocates the scene).
    #[test]
    fn pv2012_compensation_blends_into_lut() {
        let m = fake_mosaic("SONY", 100.0);
        let toe_idx = 2; // low end of the 256-entry lut
        let lut_at = |r: &Recipe| build_params(&m, r, None).lut[toe_idx];
        let base = lut_at(&Recipe::default()); // pv=0 → untouched
        let mut r11 = Recipe::default();
        r11.process_version = 11.0;
        let mut r15 = Recipe::default();
        r15.process_version = 15.4;
        let lift11 = lut_at(&r11) - base;
        let lift15 = lut_at(&r15) - base;
        assert!(lift11 > 0.01, "pv11 toe not lifted: {lift11}");
        assert!(
            lift15 > 0.0 && lift15 < lift11,
            "pv15 weaker: {lift15} vs {lift11}"
        );
        // no exposure taper: the blend is constant over |Exposure| — fits
        // showed attenuation loses the compensation exactly where the big
        // pushes need it
        let mut r11e = r11.clone();
        r11e.exposure = 2.0;
        let lift11e = lut_at(&r11e) - base;
        assert!(
            (lift11e - lift11).abs() < 1e-6,
            "exposure must not taper the blend: {lift11e} vs {lift11}"
        );
    }

    /// LR "Linear" maps to an endpoint-only curve [[0,0],[1,1]] — the
    /// Catmull-Rom endpoint clamp must NOT bow it dark; the lut is the
    /// identity ramp. Regression test for the ~20% toe-darkening bug.
    #[test]
    fn endpoint_only_curve_is_identity() {
        let lut = curve_lut(&[[0.0, 0.0], [1.0, 1.0]], |x| x, 1.0);
        for i in [0usize, 64, 128, 192, 255] {
            let want = i as f32 / 255.0;
            assert!(
                (lut[i] - want).abs() < 1e-3,
                "lut[{i}]={} want {want}",
                lut[i]
            );
        }
        // a curve WITH interior points still interpolates
        let curved = curve_lut(&[[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]], |x| x, 1.0);
        assert!(
            (curved[128] - 0.25).abs() < 0.05,
            "interior point not honored: {}",
            curved[128]
        );
    }

    /// diagnostic (needs the local RAW collection): stage means through the
    /// profile pipeline on a dark Sony file — lin (ProPhoto) -> adjust ->
    /// srgb — so a stage that loses light is visible by name.
    #[test]
    #[ignore = "slow diagnostic: full-res demosaic of a local raw"]
    fn stage_means_sony() {
        let p = std::env::var("SAFELIGHT_STAGE_RAW")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::PathBuf::from(r"G:\カメラ\RAW_alpha7R3\2023\0419\DSC00151.ARW")
            });
        let p = p.as_path();
        if !p.exists() {
            return;
        }
        let Ok(Decoded::Mosaic(m)) = crate::decode::decode(p) else {
            return;
        };
        let mut r = crate::recipe::Recipe::default();
        if let Ok(pr) = std::env::var("SAFELIGHT_STAGE_PROFILE") {
            r.profile = pr;
        } else {
            r.profile = "Adobe Color".to_string();
        }
        r.curve = vec![[0.0, 0.0], [1.0, 1.0]];
        let prm = build_params(&m, &r, None);
        let d = crate::decode::Decoded::Mosaic(m);
        let (lin, _w, _h) = lin_base(&d, &r).expect("lin_base");
        let (mut s_lin, mut s_adj, mut s_enc, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u64);
        let (mut s_pp, mut s_tone, mut s_look) = (0.0f64, 0.0f64, 0.0f64);
        for px in lin.iter().step_by(613) {
            let mean = |v: [f32; 3]| (v[0] + v[1] + v[2]) as f64 / 3.0;
            s_lin += mean(*px);
            let mut x = *px;
            for c in &mut x {
                *c *= prm.exposure_mul;
            }
            if let Some(lt) = &prm.profile_look {
                x = lt.apply(x);
            }
            s_look += mean(x);
            if !prm.profile_tone_lut.is_empty() {
                for c in &mut x {
                    *c = lut_at(&prm.profile_tone_lut, c.clamp(0.0, 1.0));
                }
            }
            s_tone += mean(x);
            if prm.post_profile != [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] {
                x = [
                    prm.post_profile[0][0] * x[0]
                        + prm.post_profile[0][1] * x[1]
                        + prm.post_profile[0][2] * x[2],
                    prm.post_profile[1][0] * x[0]
                        + prm.post_profile[1][1] * x[1]
                        + prm.post_profile[1][2] * x[2],
                    prm.post_profile[2][0] * x[0]
                        + prm.post_profile[2][1] * x[1]
                        + prm.post_profile[2][2] * x[2],
                ];
            }
            s_pp += mean(x);
            let y = adjust(*px, &prm);
            s_adj += mean(y);
            s_enc += mean([
                srgb_encode(y[0].clamp(0.0, 1.0)),
                srgb_encode(y[1].clamp(0.0, 1.0)),
                srgb_encode(y[2].clamp(0.0, 1.0)),
            ]);
            n += 1;
        }
        let n = n.max(1) as f64;
        eprintln!(
            "pivot={:.3} exp_mul={:.3} lut[0]={:.4} lut[64]={:.4} lut[128]={:.4} lut[255]={:.4} has_tone={} tone_lut[32]={:.4} tone_lut[128]={:.4} ptone[64]={:.4} ptone[128]={:.4}",
            prm.pivot,
            prm.exposure_mul,
            prm.lut[0],
            prm.lut[64],
            prm.lut[128],
            prm.lut[255],
            prm.has_tone,
            prm.tone_lut[32],
            prm.tone_lut[128],
            prm.profile_tone_lut.get(64).copied().unwrap_or(-1.0),
            prm.profile_tone_lut.get(128).copied().unwrap_or(-1.0),
        );
        eprintln!(
            "lin={:.5} look={:.5} tone={:.5} pp={:.5} adj={:.5} srgb={:.5}",
            s_lin / n,
            s_look / n,
            s_tone / n,
            s_pp / n,
            s_adj / n,
            s_enc / n
        );
    }
}
