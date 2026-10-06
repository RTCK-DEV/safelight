use serde::{Deserialize, Serialize};

/// White balance mode for development.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WbMode {
    /// camera as-shot multipliers
    AsShot,
    /// neutralize on scene average (grey world)
    Auto,
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
    /// luminance noise reduction 0..1
    pub noise_luma: f32,
    /// straighten angle in degrees, -10..10
    pub rotation_deg: f32,
    /// mid-tone local contrast -1..1
    pub clarity: f32,
    /// corner darkening -1..1 (positive darkens)
    pub vignette: f32,
    /// film grain 0..1
    pub grain: f32,
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
            noise_luma: 0.0,
            rotation_deg: 0.0,
            clarity: 0.0,
            vignette: 0.0,
            grain: 0.0,
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
}

/// Sidecar file contents stored next to each asset as `<stem>.araware.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sidecar {
    pub version: u32,
    pub rating: i32,
    pub label: String,
    pub recipe: Recipe,
}

impl Default for Sidecar {
    fn default() -> Self {
        Sidecar {
            version: 1,
            rating: 0,
            label: String::new(),
            recipe: Recipe::default(),
        }
    }
}

pub fn sidecar_path_for(asset: &std::path::Path) -> std::path::PathBuf {
    let mut p = asset.to_path_buf();
    let stem = asset
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("asset")
        .to_string();
    p.set_file_name(format!("{stem}.araware.json"));
    p
}
