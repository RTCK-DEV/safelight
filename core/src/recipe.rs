use serde::{Deserialize, Serialize};

/// White balance mode for development.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WbMode {
    /// camera as-shot multipliers
    AsShot,
    /// neutralize on scene average (grey world)
    Auto,
    /// eyedropper: neutralize the point in `wb_pick`
    Pick,
    /// explicit kelvin/tint offsets relative to as-shot
    Manual,
}

impl Default for WbMode {
    fn default() -> Self {
        WbMode::AsShot
    }
}

/// Non-destructive development recipe. All ranges are normalized:
/// tone sliders -1.0..1.0, exposure in EV stops.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    pub exposure: f32,
    pub contrast: f32,
    /// >0 recovers/compresses highlights, <0 boosts them
    pub highlights: f32,
    /// >0 lifts shadows
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    /// relative WB shift, -1..1 (warm/cool)
    pub temperature: f32,
    /// relative WB shift, -1..1 (green/magenta)
    pub tint: f32,
    pub wb_mode: WbMode,
    /// tone curve control points (x,y in 0..1); empty = linear
    pub curve: Vec<[f32; 2]>,
    /// unsharp mask amount 0..1
    pub sharpen: f32,
    /// sharpen edge masking 0..1 (LR "Masking"): attenuates the unsharp
    /// delta on flat areas, leaving edges fully sharpened
    #[serde(default)]
    pub sharp_mask: f32,
    /// sharpen radius in 3x3-box units 0.5..3.0 (LR 0.5..3.0, default 1.0)
    #[serde(default = "default_one")]
    pub sharp_radius: f32,
    /// sharpen detail 0..1: blends the sharpening band between the
    /// coarse-radius delta (0) and the fine 3x3 delta (1). LR default 25
    #[serde(default)]
    pub sharp_detail: f32,
    /// luminance noise reduction 0..1
    pub noise_luma: f32,
    /// straighten angle in degrees, -10..10
    pub rotation_deg: f32,
    /// mid-tone local contrast -1..1
    pub clarity: f32,
    /// corner darkening -1..1 (positive darkens)
    pub vignette: f32,
    /// post-crop vignette midpoint: normalized radius of the 50% mark,
    /// 0..1 (LR 0..100). -1 = unset → legacy fixed ramp
    #[serde(default = "neg_one")]
    pub vig_mid: f32,
    /// post-crop vignette roundness -1..1: -1 = frame-following square,
    /// +1 = circle (LR -100..100). -2 = unset
    #[serde(default = "neg_two")]
    pub vig_round: f32,
    /// post-crop vignette feather 0..1: transition width (LR 0..100).
    /// -1 = unset
    #[serde(default = "neg_one")]
    pub vig_feather: f32,
    /// film grain 0..1
    pub grain: f32,
    /// grain particle size 0..1 → 1..5 px cells (LR GrainSize 0..100)
    #[serde(default)]
    pub grain_size: f32,
    /// grain roughness 0..1: blends low-frequency clumpy noise into the
    /// white noise (LR GrainRoughness 0..100)
    #[serde(default)]
    pub grain_rough: f32,
    // ---- grading (DaVinci-style lift/gamma/gain + split tone + look) ----
    /// per-channel shadow offset, typically -0.25..0.25
    pub lift: [f32; 3],
    /// per-channel midtone exponent multiplier, typically 0.5..2
    pub gamma: [f32; 3],
    /// per-channel highlight multiplier, typically 0.5..2
    pub gain: [f32; 3],
    /// split tone: shadow hue (0..1) + amount (0..1)
    pub shadow_hue: f32,
    pub shadow_sat: f32,
    /// split tone: highlight hue + amount
    pub highlight_hue: f32,
    pub highlight_sat: f32,
    /// cinematic preset: "none"|"teal_orange"|"film_fade"|"bleach"|"noir"|"matte"
    pub look: String,
    // ---- automatic correction ----
    /// histogram-driven exposure compensation
    pub auto_exposure: bool,
    /// histogram percentile white/black point stretch
    pub auto_contrast: bool,
    // ---- retouch ----
    /// crop fractions [left, top, right, bottom] 0..0.9 (post-rotation frame)
    pub crop: [f32; 4],
    /// spot heal marks [cx, cy, radius, _] in normalized post-flip frame coords
    pub spots: Vec<[f32; 4]>,
    /// dodge/burn radial lights [cx, cy, radius, ev] in normalized frame coords
    pub lights: Vec<[f32; 4]>,
    // ---- DaVinci color page extras ----
    /// WB eyedropper point (frame-normalized); used when wb_mode="pick"
    pub wb_pick: [f32; 2],
    /// global offset wheel, -0.25..0.25
    pub offset: [f32; 3],
    /// 3-way midtone tint: hue (0..1) + amount
    pub midtone_hue: f32,
    pub midtone_sat: f32,
    /// LR SplitToningBalance -1..1: shifts the shadow↔highlight crossover
    #[serde(default)]
    pub grade_balance: f32,
    /// LR ColorGradeBlending 0..1 (LR 0..100): range overlap width (0.5=LR50)
    #[serde(default = "grade_blend_default")]
    pub grade_blend: f32,
    /// per-channel custom curves (same [[x,y]] format as `curve`)
    pub curve_r: Vec<[f32; 2]>,
    pub curve_g: Vec<[f32; 2]>,
    pub curve_b: Vec<[f32; 2]>,
    /// hue-domain curves, x = input hue 0..1
    pub hue_hue: Vec<[f32; 2]>, // y = output hue 0..1
    pub hue_sat: Vec<[f32; 2]>, // y = sat gain (1 = neutral)
    pub hue_lum: Vec<[f32; 2]>, // y = luma gain (1 = neutral)
    pub lum_sat: Vec<[f32; 2]>, // y = sat gain vs input luma
    pub sat_sat: Vec<[f32; 2]>, // y = output sat (remap)
    /// HSL qualifier (secondary): soft windows in each H/S/L channel
    pub qh: [f32; 3], // hue [center, half_width, soft] 0..1 cycle
    pub qs: [f32; 3],           // sat [lo, hi, soft]
    pub ql: [f32; 3],           // lum [lo, hi, soft]
    /// inside-mask adjustments [hue_shift, sat_gain, lum_gain, temp]
    pub qadj: [f32; 4],
    pub q_invert: bool,
    /// matte finesse: [clean_black, clean_white] remap of the key 0..1
    pub q_clean: [f32; 2],
    /// matte finesse: blur radius (dilates HSL soft edges) 0..1
    pub q_blur: f32,
    /// highlight mode: preview the key — qualified in colour, rest grey
    pub q_show: bool,
    /// power windows: parametric spatial masks carrying local adjustments (max 4)
    pub windows: Vec<PowerWindow>,
    // ---- HDR wheels + raw gamma controls ----
    /// zone adjustments [hue_tint(0..1), tint_amt, ev, sat] applied per luma band
    pub z_dark: [f32; 4],
    pub z_shadow: [f32; 4],
    pub z_light: [f32; 4],
    pub z_global: [f32; 4],
    /// contrast pivot point, 0..1 (DaVinci pivot / raw midpoint)
    pub pivot: f32,
    /// highlight rolloff compression, 0..2 (1 = linear)
    pub highlight_rolloff: f32,
    /// shadow rolloff, 0..2 (1 = linear)
    pub shadow_rolloff: f32,
    /// LR defringe: [amount 0..1, hue_lo_deg, hue_hi_deg] for the
    /// purple band (desaturates saturated pixels in the hue range)
    #[serde(default = "defringe_pu_default")]
    pub defringe_pu: [f32; 3],
    /// same for the green band
    #[serde(default = "defringe_gr_default")]
    pub defringe_gr: [f32; 3],
    /// LR camera calibration: [shadow_tint, red_hue, red_sat, green_hue,
    /// green_sat, blue_hue, blue_sat] all normalized -1..1. Applied as a
    /// 3x3 chroma-rotation matrix + shadow green/magenta shift.
    #[serde(default)]
    pub calib: [f32; 7],
    /// LR Texture: fine-scale midtone detail contrast, -1..1
    #[serde(default)]
    pub texture: f32,
    /// LR Lens Blur amount 0..1 (depth-of-field defocus; blended by
    /// 1-subject matte as the focus proxy)
    #[serde(default)]
    pub lens_blur: f32,
    /// LR Lens Blur bokeh options: [highlight_boost, highlight_thresh,
    /// aspect(-1..1 squash), shape(0=disc,1=ring), spherical_aberration]
    #[serde(default)]
    pub lens_blur_opts: [f32; 5],
    /// LR Lens Blur bokeh options 2: [cat-eye amount, cat-eye scale,
    /// bokeh rotation, bokeh shape detail] — all 0..1
    #[serde(default)]
    pub lens_blur_opts2: [f32; 4],
    // ---- channel mixer / clone stamp / beauty ----
    /// RGB mixer: 3x3 row-major (identity default)
    pub mixer: [f32; 9],
    /// monochrome conversion weights [r,g,b]; all-zero = off
    pub mono: [f32; 3],
    /// clone stamps [sx, sy, dx, dy, radius, _] frame-normalized (max 8)
    pub clones: Vec<[f32; 6]>,
    /// skin smoothing amount 0..1 (edge-aware, skin-hue weighted)
    pub beauty: f32,
    // ---- restoration + light fx ----
    /// chroma noise reduction 0..1 (spatial, pairs with noise_luma)
    pub noise_chroma: f32,
    /// haze removal 0..1 (dark-channel-prior veil subtraction)
    pub dehaze: f32,
    /// chromatic aberration fix -0.5..0.5 (radial R/B scale)
    pub ca_fix: f32,
    /// debanding amount 0..1 (smooths quantized gradients)
    pub deband: f32,
    /// lens glow (bloom on highlights) 0..1
    pub glow: f32,
    /// lens flare [cx, cy, strength, hue] frame-normalized
    pub flare: [f32; 4],
    // ---- darktable tone equalizer + LUT import + keystone ----
    /// per-zone exposure in EV for log2-luma zones centered at -4..+4 EV
    /// (index 0 = deepest shadows, 8 = brightest highlights), -4..4 each
    pub zones_ev: [f32; 9],
    /// WB eyedropper sample radius as a fraction of the frame, 0.002..0.2
    pub wb_pick_size: f32,
    /// path to a .cube 3D LUT file (empty = none); applied display-referred
    /// at the end of the adjust chain
    pub lut_file: String,
    /// 0..1 blend amount of the LUT
    pub lut_amount: f32,
    /// decoded Adobe `crs:Table_*` RGB LUT (from Look/enhanced profiles),
    /// applied display-referred like `lut_file`. Not serialized — the
    /// encoded payload lives in the source XMP.
    #[serde(skip)]
    pub rgb_lut: Option<crate::lut::CubeLut>,
    /// decoded Adobe `crs:LookTable` HSV map (dng_look_table); replaces the
    /// DCP profile's look table when the XMP carries one.
    #[serde(skip)]
    pub look_table: Option<crate::dcp::HsMap>,
    /// keystone correction: vertical trapezoid warp -0.4..0.4
    /// (>0 widens the bottom — fixes converging verticals shot from below)
    pub key_v: f32,
    /// keystone correction: horizontal trapezoid warp -0.4..0.4
    pub key_h: f32,
    /// lens profile correction amount 0..1. Defaults to 1 — profile
    /// corrections on by default like Lightroom/darktable. When a lensfun
    /// profile matches the file's EXIF lens this corrects geometric
    /// distortion, lateral CA and vignetting at that strength.
    #[serde(default = "default_one")]
    pub lens_corr: f32,
    /// crs:LensProfileName — a preset's explicit lens profile choice,
    /// overriding the EXIF lens when matching the lens db.
    #[serde(default)]
    pub lens_name: String,
    /// adjustment-brush layers (LR-style): each layer carries its own
    /// ev/sat/temp + opacity and a list of painted strokes, applied
    /// inside the stroked mask. Up to 4 layers are honoured.
    #[serde(default)]
    pub brushes: Vec<BrushLayer>,
    /// AI denoise blend 0..1 (SCUNet real-world denoise). The denoised
    /// base is produced once by `safelight_ai_denoise_prepare` and cached in
    /// `<photo>.safelight.aidn.png`; this field just blends it in.
    #[serde(default)]
    pub ai_denoise: f32,
    /// serial correction stages (DaVinci serial nodes): each stage runs the
    /// full adjust chain again on top of the previous result, gated by its
    /// own qualifier/window key and opacity. Max 4 honoured.
    #[serde(default)]
    pub stages: Vec<Stage>,
    /// ColorSlice (DaVinci): 7 fixed hue wedges Red/Skin/Yellow/Green/Cyan/
    /// Blue/Magenta, each [hue_shift, sat_delta, lum_delta, enabled].
    #[serde(default)]
    pub color_slice: [[f32; 4]; 7],
    /// ColorWarper (DaVinci Hue-Sat): control points [h, s, dh, ds, radius],
    /// max 8 honoured.
    #[serde(default)]
    pub warper: Vec<[f32; 5]>,
    /// LR Point Color swatches: raw 19-float rows as serialized in
    /// `crs:PointColors` — [SrcHue(0..6), SrcSat, SrcLum, HueShift,
    /// SatScale, LumScale, RangeAmount, HueRange(LN,LF,UF,UN),
    /// SatRange(...), LumRange(...)].
    /// Entries of all -1 are LR's "empty swatch" sentinel.
    #[serde(default)]
    pub point_colors: Vec<[f32; 19]>,
    /// `crs:ColorVariance`: per-swatch variance (-1..1, index-aligned with
    /// point_colors). Negative pulls the selected colours toward the
    /// "after" swatch colour; positive pushes them away.
    #[serde(default)]
    pub point_color_vars: Vec<f32>,
    /// camera profile: "Adobe Standard", "Camera Standard", a .dcp path,
    /// or "" for the built-in libraw matrix. Resolved against
    /// ~/.safelight/profiles and (when installed) Lightroom's CameraProfiles.
    #[serde(default)]
    pub profile: String,
    /// LR profile Amount slider (crs:Amount inside <crs:Look>) — scales the
    /// resolved profile's baked-in grade; 1 = full.
    #[serde(default = "default_one")]
    pub profile_amount: f32,
    /// Lightroom process version of the develop settings (crs:ProcessVersion,
    /// catalog processVersion). 0 = unset/current. Matters because ACR's
    /// base tone scale is version-dependent: PV11 (PV2012-era) uses an
    /// image-adaptive tone mapping that PV15.x renders replaced.
    #[serde(default)]
    pub process_version: f32,
    /// absolute white balance in kelvin (Lightroom crs:Temperature).
    /// 0 = unset; when >0 it replaces the as-shot/auto/pick multipliers
    /// entirely (the camera-neutral is derived from the illuminant's
    /// chromaticity through the active color matrix).
    #[serde(default)]
    pub temp_abs: f32,
    /// absolute tint along the magenta(+)/green(-) axis, LR scale ±150.
    #[serde(default)]
    pub tint_abs: f32,
    /// skip the default filmic base tone curve (Adobe's renderer always
    /// applies a medium-contrast display transform on scene-linear data;
    /// we match that by default). Set true for unshaped linear output.
    #[serde(default)]
    pub linear_output: bool,
}

/// one serial correction stage. `params` reuses the Recipe struct — only the
/// adjust-domain fields are honoured (tone/colour/curves/qualifier/windows/
/// brushes/lights); geometry, retouch, grain/vignette/flare and nested
/// `stages` are ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Stage {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// stage strength 0..1 (DaVinci node key output gain)
    #[serde(default = "default_one")]
    pub opacity: f32,
    /// invert the stage key (apply the grade outside the mask)
    #[serde(default)]
    pub invert: bool,
    #[serde(default)]
    pub params: Box<Recipe>,
}

impl Default for Stage {
    fn default() -> Self {
        Stage {
            name: String::new(),
            enabled: true,
            opacity: 1.0,
            invert: false,
            params: Box::new(Recipe::default()),
        }
    }
}

/// one painted stroke inside a brush layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushStroke {
    /// polyline points in frame-normalized (pre-crop) coordinates
    pub pts: Vec<[f32; 2]>,
    /// radius as a fraction of the frame height
    pub radius: f32,
    /// feather 0..1: mask is solid until radius*(1-soft), then ramps out
    pub soft: f32,
    /// stroke strength 0..1 (brush flow)
    pub opacity: f32,
    /// erase stroke: subtracts from the layer mask instead of adding
    pub erase: bool,
}

impl Default for BrushStroke {
    fn default() -> Self {
        BrushStroke {
            pts: Vec::new(),
            radius: 0.05,
            soft: 0.5,
            opacity: 1.0,
            erase: false,
        }
    }
}

/// a brush layer: shared adjustment applied inside its strokes' union mask.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushLayer {
    pub enabled: bool,
    /// layer strength 0..1
    pub opacity: f32,
    /// exposure offset -4..4 EV
    pub ev: f32,
    /// saturation offset -1..1
    pub sat: f32,
    /// warm(+)/cool(-) -1..1
    pub temp: f32,
    /// gate this layer's mask by the HSL qualifier matte
    pub link_q: bool,
    /// edge-aware masking (Affinity auto-mask): each stroke records the
    /// colour under its first dab and the mask only covers pixels inside
    /// `edge_tol` of it in luma/chroma distance
    pub edge_aware: bool,
    /// similarity tolerance 0.05..1.0 — larger admits more pixels
    pub edge_tol: f32,
    /// extended local adjustments (LR Local* fields), -1..1 each
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub clarity: f32,
    #[serde(default)]
    pub dehaze: f32,
    /// green(-)/magenta(+) axis -1..1
    #[serde(default)]
    pub tint: f32,
    /// tone-band offsets (LR Local{Highlights,Shadows,Whites,Blacks}), -1..1
    #[serde(default)]
    pub hi: f32,
    #[serde(default)]
    pub sh: f32,
    #[serde(default)]
    pub wh: f32,
    #[serde(default)]
    pub bl: f32,
    /// LR LocalTexture: fine-detail midtone contrast, -1..1
    #[serde(default)]
    pub texture: f32,
    pub strokes: Vec<BrushStroke>,
}

impl Default for BrushLayer {
    fn default() -> Self {
        BrushLayer {
            enabled: true,
            opacity: 1.0,
            ev: 0.0,
            sat: 0.0,
            temp: 0.0,
            link_q: false,
            edge_aware: false,
            edge_tol: 0.5,
            contrast: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            tint: 0.0,
            hi: 0.0,
            sh: 0.0,
            wh: 0.0,
            bl: 0.0,
            texture: 0.0,
            strokes: Vec::new(),
        }
    }
}

/// parametric spatial mask + local adjustment (DaVinci power window).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerWindow {
    /// "circle" (p=[cx,cy,rx,ry,rot_deg,soft]), "gradient" (p=[x1,y1,x2,y2,soft,0])
    /// or "lum" luminance range (p=[lo,hi,lo_feather,hi_feather,0,0])
    pub kind: String,
    pub p: [f32; 6],
    pub ev: f32,   // exposure offset in EV, -4..4
    pub sat: f32,  // saturation offset -1..1
    pub temp: f32, // warm(+)/cool(-) -1..1
    pub invert: bool,
    /// window on/off (DaVinci: per-window visibility eye)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// adjustment strength 0..1 (DaVinci window opacity)
    #[serde(default = "default_one")]
    pub opacity: f32,
    /// gate this window's mask by the HSL qualifier matte (intersect)
    #[serde(default)]
    pub link_q: bool,
    /// transition midpoint (LR-style): for "gradient" the 50% mark's
    /// position along the p1→p2 span, for "circle" the 50% mark's
    /// normalized radius. 0 = legacy default (0.5 / 1-soft/2).
    #[serde(default)]
    pub mid: f32,
    /// extended local adjustments (LR Local* fields), -1..1 each
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub clarity: f32,
    #[serde(default)]
    pub dehaze: f32,
    /// green(-)/magenta(+) axis -1..1
    #[serde(default)]
    pub tint: f32,
    /// tone-band offsets (LR Local{Highlights,Shadows,Whites,Blacks}), -1..1
    #[serde(default)]
    pub hi: f32,
    #[serde(default)]
    pub sh: f32,
    #[serde(default)]
    pub wh: f32,
    #[serde(default)]
    pub bl: f32,
    /// LR LocalTexture: fine-detail midtone contrast, -1..1
    #[serde(default)]
    pub texture: f32,
    /// LR range-mask refinement folded into this window (one slot —
    /// additional components keep their own windows in add mode).
    /// [mode, kind, p0..p3]: mode 1=subtract, 2=intersect; kind 1=color
    /// (h01, sat, tol, inv), kind 2=lum (lo, hi, feather, inv).
    #[serde(default)]
    pub refine: [f32; 6],
    /// LR LocalPointColors: masked point-colour swatches scoped to this
    /// window — raw 19-float rows + per-swatch variance (LocalColorVariance).
    #[serde(default)]
    pub pcs: Vec<([f32; 19], f32)>,
}

fn defringe_pu_default() -> [f32; 3] {
    [0.0, 246.0, 294.0]
}
fn defringe_gr_default() -> [f32; 3] {
    [0.0, 108.0, 132.0]
}
fn default_true() -> bool {
    true
}
fn default_one() -> f32 {
    1.0
}
fn neg_one() -> f32 {
    -1.0
}
fn neg_two() -> f32 {
    -2.0
}
fn grade_blend_default() -> f32 {
    0.5
}

impl Default for PowerWindow {
    fn default() -> Self {
        PowerWindow {
            kind: "circle".into(),
            p: [0.5, 0.5, 0.15, 0.15, 0.0, 0.2],
            ev: 0.0,
            sat: 0.0,
            temp: 0.0,
            link_q: false,
            invert: false,
            enabled: true,
            opacity: 1.0,
            mid: 0.0,
            contrast: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            tint: 0.0,
            hi: 0.0,
            sh: 0.0,
            wh: 0.0,
            bl: 0.0,
            texture: 0.0,
            refine: [0.0; 6],
            pcs: Vec::new(),
        }
    }
}

impl Default for Recipe {
    fn default() -> Self {
        Recipe {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            temperature: 0.0,
            tint: 0.0,
            wb_mode: WbMode::AsShot,
            curve: Vec::new(),
            sharpen: 0.0,
            sharp_mask: 0.0,
            sharp_radius: 1.0,
            sharp_detail: 0.25,
            noise_luma: 0.0,
            rotation_deg: 0.0,
            clarity: 0.0,
            vignette: 0.0,
            vig_mid: -1.0,
            vig_round: -2.0,
            vig_feather: -1.0,
            grain: 0.0,
            grain_size: 0.25,
            grain_rough: 0.5,
            lift: [0.0; 3],
            gamma: [1.0; 3],
            gain: [1.0; 3],
            shadow_hue: 0.55,
            shadow_sat: 0.0,
            highlight_hue: 0.08,
            highlight_sat: 0.0,
            look: String::new(),
            auto_exposure: false,
            auto_contrast: false,
            crop: [0.0; 4],
            spots: Vec::new(),
            lights: Vec::new(),
            wb_pick: [0.5, 0.5],
            offset: [0.0; 3],
            midtone_hue: 0.33,
            midtone_sat: 0.0,
            grade_balance: 0.0,
            grade_blend: 0.5,
            curve_r: Vec::new(),
            curve_g: Vec::new(),
            curve_b: Vec::new(),
            hue_hue: Vec::new(),
            hue_sat: Vec::new(),
            hue_lum: Vec::new(),
            lum_sat: Vec::new(),
            sat_sat: Vec::new(),
            qh: [0.0, 0.0, 0.05],
            qs: [0.0, 1.0, 0.05],
            ql: [0.0, 1.0, 0.05],
            qadj: [0.0; 4],
            q_invert: false,
            q_clean: [0.0, 1.0],
            q_blur: 0.0,
            q_show: false,
            windows: Vec::new(),
            z_dark: [0.0; 4],
            z_shadow: [0.0; 4],
            z_light: [0.0; 4],
            z_global: [0.0; 4],
            pivot: 0.18,
            highlight_rolloff: 1.0,
            shadow_rolloff: 1.0,
            defringe_pu: [0.0, 246.0, 294.0],
            defringe_gr: [0.0, 108.0, 132.0],
            calib: [0.0; 7],
            texture: 0.0,
            lens_blur: 0.0,
            lens_blur_opts: [0.0; 5],
            lens_blur_opts2: [0.0; 4],
            mixer: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            mono: [0.0; 3],
            clones: Vec::new(),
            beauty: 0.0,
            noise_chroma: 0.2,
            dehaze: 0.0,
            ca_fix: 0.0,
            deband: 0.0,
            glow: 0.0,
            flare: [0.0; 4],
            zones_ev: [0.0; 9],
            wb_pick_size: 0.025,
            lut_file: String::new(),
            lut_amount: 1.0,
            rgb_lut: None,
            look_table: None,
            key_v: 0.0,
            key_h: 0.0,
            lens_corr: 1.0,
            lens_name: String::new(),
            brushes: Vec::new(),
            ai_denoise: 0.0,
            stages: Vec::new(),
            color_slice: [[0.0; 4]; 7],
            warper: Vec::new(),
            point_colors: Vec::new(),
            point_color_vars: Vec::new(),
            profile: String::new(),
            profile_amount: 1.0,
            process_version: 0.0,
            temp_abs: 0.0,
            tint_abs: 0.0,
            linear_output: false,
        }
    }
}

impl Recipe {
    pub fn from_json(s: &str) -> Option<Recipe> {
        serde_json::from_str(s).ok()
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// LR `crs:Amount` / preset-strength scaling: every adjustment delta is
    /// interpolated toward its default by factor `a` (0 = neutral, 1 = full,
    /// up to 2 = boosted). Mask/brush geometry, WB pick, crop and transform
    /// (keystone/rotation/lens) are left alone — Amount does not reshape
    /// masks or straighten the horizon.
    pub fn scale(&mut self, a: f32) {
        if (a - 1.0).abs() < 1e-6 {
            return;
        }
        let s = |v: &mut f32| *v *= a;
        let s3 = |v: &mut [f32; 3]| v.iter_mut().for_each(|x| *x *= a);
        // basic tone/colour
        s(&mut self.exposure);
        s(&mut self.contrast);
        s(&mut self.highlights);
        s(&mut self.shadows);
        s(&mut self.whites);
        s(&mut self.blacks);
        s(&mut self.saturation);
        s(&mut self.vibrance);
        s(&mut self.temperature);
        s(&mut self.tint);
        // detail
        s(&mut self.sharpen);
        s(&mut self.noise_luma);
        s(&mut self.noise_chroma);
        s(&mut self.clarity);
        s(&mut self.texture);
        s(&mut self.dehaze);
        // effects
        s(&mut self.vignette);
        s(&mut self.grain);
        s(&mut self.glow);
        s(&mut self.beauty);
        s(&mut self.ca_fix);
        s(&mut self.deband);
        s(&mut self.lens_blur);
        s(&mut self.flare[2]);
        // lift/gamma/gain are multiplicative-1 defaults
        s3(&mut self.lift);
        s3(&mut self.offset);
        for v in self.gamma.iter_mut().chain(self.gain.iter_mut()) {
            *v = 1.0 + (*v - 1.0) * a;
        }
        // split toning / color grading sats + zone adjustments
        s(&mut self.shadow_sat);
        s(&mut self.highlight_sat);
        s(&mut self.midtone_sat);
        for z in [
            &mut self.z_dark,
            &mut self.z_shadow,
            &mut self.z_light,
            &mut self.z_global,
        ] {
            z[1] *= a;
            z[2] *= a;
            z[3] *= a;
        }
        for v in &mut self.qadj {
            *v *= a;
        }
        for v in &mut self.zones_ev {
            *v *= a;
        }
        // camera calibration + defringe amounts
        for v in &mut self.calib {
            *v *= a;
        }
        self.defringe_pu[0] *= a;
        self.defringe_gr[0] *= a;
        // channel mixer: scale the delta from identity
        for (i, v) in self.mixer.iter_mut().enumerate() {
            let id = if i % 4 == 0 { 1.0 } else { 0.0 };
            *v = id + (*v - id) * a;
        }
        // tone curves: scale the y-delta (master/rgb) or the 1.0-gain
        // (hue/lum/sat-domain curves)
        for pts in [
            &mut self.curve,
            &mut self.curve_r,
            &mut self.curve_g,
            &mut self.curve_b,
        ] {
            for p in pts.iter_mut() {
                p[1] = p[0] + (p[1] - p[0]) * a;
            }
        }
        for p in self.hue_hue.iter_mut() {
            p[1] = p[0] + (p[1] - p[0]) * a;
        }
        for pts in [
            &mut self.hue_sat,
            &mut self.hue_lum,
            &mut self.lum_sat,
            &mut self.sat_sat,
        ] {
            for p in pts.iter_mut() {
                p[1] = 1.0 + (p[1] - 1.0) * a;
            }
        }
        for sl in &mut self.color_slice {
            sl[0] *= a;
            sl[1] *= a;
            sl[2] *= a;
        }
        for w in &mut self.warper {
            w[2] *= a;
            w[3] *= a;
        }
        // embedded/file LUT contribution
        self.lut_amount *= a;
        // local adjustments: scale the deltas, keep the mask geometry
        for w in &mut self.windows {
            w.ev *= a;
            w.sat *= a;
            w.temp *= a;
            w.contrast *= a;
            w.clarity *= a;
            w.dehaze *= a;
            w.tint *= a;
            w.hi *= a;
            w.sh *= a;
            w.wh *= a;
            w.bl *= a;
            w.texture *= a;
        }
        for b in &mut self.brushes {
            b.opacity *= a;
            b.ev *= a;
            b.sat *= a;
            b.temp *= a;
            b.contrast *= a;
            b.clarity *= a;
            b.dehaze *= a;
            b.tint *= a;
            b.hi *= a;
            b.sh *= a;
            b.wh *= a;
            b.bl *= a;
            b.texture *= a;
        }
    }
}

/// A named snapshot of a recipe (DaVinci "grade version" / still).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GradeVersion {
    pub name: String,
    pub recipe: Recipe,
}

impl Default for GradeVersion {
    fn default() -> Self {
        GradeVersion {
            name: String::new(),
            recipe: Recipe::default(),
        }
    }
}

/// Sidecar file contents stored next to each asset as `<stem>.safelight.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidecar {
    pub version: u32,
    pub rating: i32,
    pub label: String,
    /// pick flag: -1 rejected, 0 unflagged, 1 picked (LR convention)
    pub flag: i32,
    /// user keywords/tags
    pub keywords: Vec<String>,
    pub recipe: Recipe,
    /// saved grade versions (DaVinci stills/versions)
    pub versions: Vec<GradeVersion>,
}

impl Default for Sidecar {
    fn default() -> Self {
        Sidecar {
            version: 1,
            rating: 0,
            label: String::new(),
            flag: 0,
            keywords: Vec::new(),
            recipe: Recipe::default(),
            versions: Vec::new(),
        }
    }
}

pub fn sidecar_path_for(asset: &std::path::Path) -> std::path::PathBuf {
    sidecar_path_for_v(asset, 0)
}

/// Sidecar for a virtual copy: slot 0 = master `<stem>.safelight.json`,
/// slot n = `<stem>.safelight.v{n}.json`.
pub fn sidecar_path_for_v(asset: &std::path::Path, vslot: u32) -> std::path::PathBuf {
    let mut p = asset.to_path_buf();
    let stem = asset
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string();
    let name = if vslot == 0 {
        format!("{stem}.safelight.json")
    } else {
        format!("{stem}.safelight.v{vslot}.json")
    };
    p.set_file_name(name);
    p
}

/// The same sidecar under the pre-rename `<stem>.araware.json` naming.
/// Reads fall back to this so existing libraries keep their edits; new
/// writes always go to the .safelight name.
pub fn legacy_sidecar_path_for_v(asset: &std::path::Path, vslot: u32) -> std::path::PathBuf {
    let mut p = asset.to_path_buf();
    let stem = asset
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string();
    let name = if vslot == 0 {
        format!("{stem}.araware.json")
    } else {
        format!("{stem}.araware.v{vslot}.json")
    };
    p.set_file_name(name);
    p
}

/// Path a reader should use: the .safelight sidecar if present, else the
/// araware-era one if present, else the .safelight path (nonexistent) so
/// callers' exists()/default handling still works.
pub fn read_sidecar_path_for_v(asset: &std::path::Path, vslot: u32) -> std::path::PathBuf {
    let p = sidecar_path_for_v(asset, vslot);
    if p.exists() {
        return p;
    }
    let l = legacy_sidecar_path_for_v(asset, vslot);
    if l.exists() {
        return l;
    }
    p
}
