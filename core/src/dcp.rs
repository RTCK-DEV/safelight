//! Adobe DNG Camera Profile (.dcp) support — the same color pipeline
//! Lightroom/Camera Raw run: interpolated dual-illuminant matrices,
//! ProfileHueSatMap (hue/sat/value shift table in camera space),
//! ProfileLookTable (same table applied in the working space),
//! ProfileToneCurve and BaselineExposure.
//!
//! A .dcp is a TIFF container of DNG-spec tags; this module parses it
//! directly (no libraw involvement). Profiles are discovered in a
//! safelight profile dir (`~/.safelight/profiles` or SAFELIGHT_PROFILE_DIR)
//! plus, when present, Lightroom's own CameraProfiles resource trees —
//! e.g. `C:\Program Files\Adobe\Adobe Lightroom Classic\Resources\
//! CameraProfiles` on Windows or the app bundle Resources on macOS —
//! so an installed LrC hands us Adobe Standard + Camera Matching data
//! for every supported camera.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type Mat3 = [[f32; 3]; 3];

/// HSV delta table (DNG ProfileHueSatMap / ProfileLookTable).
/// Indexed by (hue, sat, val); each entry = [hue_shift_deg, sat_scale, val_scale].
#[derive(Debug, Clone)]
pub struct HsMap {
    /// [hue_divs, sat_divs, val_divs]
    pub dims: [u32; 3],
    /// dng_hue_sat_map storage order: value outermost, hue middle,
    /// saturation fastest — idx = (v * hue_divs + h) * sat_divs + s.
    pub data: Vec<[f32; 3]>,
    /// ProfileHueSatMapEncoding / ProfileLookTableEncoding (0xC7A3 / 0xC7A4):
    /// true when the table is declared sRGB-encoded. Per dng_reference.cpp
    /// RefBaselineHueSatMap, only the V coordinate is gamma-encoded for the
    /// table lookup and the scaled V is decoded back to linear afterwards —
    /// hue/sat coords stay linear. Sony camera-matching DCPs all ship
    /// LookTableEncoding=1; sampling them at linear V reads the wrong cells.
    pub srgb_encoded: bool,
}

fn hsm_srgb_encode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.0031308 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

fn hsm_srgb_decode(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

impl HsMap {
    /// rgb -> hsv (H 0..1 cyclic, S/V 0..1, V = max channel)
    fn hsv(rgb: [f32; 3]) -> [f32; 3] {
        let mx = rgb[0].max(rgb[1]).max(rgb[2]);
        let mn = rgb[0].min(rgb[1]).min(rgb[2]);
        let d = mx - mn;
        let h = if d <= 1e-9 {
            0.0
        } else if mx == rgb[0] {
            (60.0 * ((rgb[1] - rgb[2]) / d) + 360.0) % 360.0
        } else if mx == rgb[1] {
            60.0 * ((rgb[2] - rgb[0]) / d) + 120.0
        } else {
            60.0 * ((rgb[0] - rgb[1]) / d) + 240.0
        } / 360.0;
        let s = if mx > 1e-9 { d / mx } else { 0.0 };
        [h, s, mx]
    }

    fn rgb_from_hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
        let h = (h - h.floor()) * 6.0;
        let i = h.floor() as i32;
        let f = h - i as f32;
        let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
        match i.rem_euclid(6) {
            0 => [v, t, p],
            1 => [q, v, p],
            2 => [p, v, t],
            3 => [p, q, v],
            4 => [t, p, v],
            _ => [v, p, q],
        }
    }

    /// trilinear sample: (hue 0..1 cyclic, sat 0..1, val 0..1) -> [dH(deg), sS, vS]
    pub fn lookup(&self, h: f32, s: f32, v: f32) -> [f32; 3] {
        let (hd, sd, vd) = (
            self.dims[0] as usize,
            self.dims[1] as usize,
            self.dims[2] as usize,
        );
        if self.data.is_empty() || hd == 0 || sd == 0 || vd == 0 {
            return [0.0, 1.0, 1.0];
        }
        // hue wraps; sat/val clamp to grid 0..divs-1
        let hf = (h - h.floor()) * hd as f32;
        let h0 = hf.floor() as usize;
        let h1 = (h0 + 1) % hd;
        let ht = hf - h0 as f32;
        let sf = (s.clamp(0.0, 1.0) * (sd - 1) as f32).min((sd - 1) as f32 - 1e-6);
        let s0 = sf.floor() as usize;
        let s1 = (s0 + 1).min(sd - 1);
        let st = sf - s0 as f32;
        let vf = (v.clamp(0.0, 1.0) * (vd - 1) as f32).min((vd - 1) as f32 - 1e-6);
        let v0 = vf.floor() as usize;
        let v1 = (v0 + 1).min(vd - 1);
        let vt = vf - v0 as f32;
        let at = |h: usize, s: usize, v: usize| -> [f32; 3] { self.data[(v * hd + h) * sd + s] };
        let mut out = [0.0f32; 3];
        for c in 0..3 {
            // hue shift interpolates with wraparound-aware shortest path
            let c00 = {
                let a = at(h0, s0, v0)[c] * (1.0 - st) + at(h0, s1, v0)[c] * st;
                let b = at(h0, s0, v1)[c] * (1.0 - st) + at(h0, s1, v1)[c] * st;
                a * (1.0 - vt) + b * vt
            };
            let c11 = {
                let a = at(h1, s0, v0)[c] * (1.0 - st) + at(h1, s1, v0)[c] * st;
                let b = at(h1, s0, v1)[c] * (1.0 - st) + at(h1, s1, v1)[c] * st;
                a * (1.0 - vt) + b * vt
            };
            let v0v = c00;
            let v1v = c11;
            if c == 0 {
                // hue shift: interpolate on the circle
                let mut d = v1v - v0v;
                if d > 180.0 {
                    d -= 360.0;
                }
                if d < -180.0 {
                    d += 360.0;
                }
                out[c] = v0v + d * ht;
            } else {
                out[c] = v0v + (v1v - v0v) * ht;
            }
        }
        out
    }

    /// apply the map: input rgb -> shifted/scaled rgb
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let [h, s, v] = Self::hsv(rgb);
        // note: neutrals (s≈0) must NOT early-out — the map's val scale
        // still applies to them (Adobe Standard lifts shadows ~1.2×)
        //
        // sRGB-encoded tables (dng_reference.cpp RefBaselineHueSatMap):
        // the V coordinate is sRGB-encoded before the table lookup when
        // valDivisions >= 2, valScale multiplies the ENCODED v, and the
        // result is decoded back — so v' = dec(enc(v) * scale). For a 2.5D
        // map (vd < 2) the encode is skipped but the decode still applies.
        let enc = self.srgb_encoded;
        let mut v_enc = v;
        if enc && self.dims[2] > 1 {
            v_enc = hsm_srgb_encode(v);
        }
        let d = self.lookup(h, s, v_enc);
        let h2 = h + d[0] / 360.0;
        let s2 = (s * d[1]).clamp(0.0, 1.0);
        let v2_enc = (v_enc * d[2]).clamp(0.0, 1.0);
        let v2 = if enc {
            hsm_srgb_decode(v2_enc)
        } else {
            v2_enc.max(0.0)
        };
        Self::rgb_from_hsv(h2, s2, v2)
    }
}

#[derive(Debug, Clone, Default)]
pub struct DcpProfile {
    pub name: String,
    pub copyright: String,
    /// XYZ -> camera under calibration illuminant 1/2 (SRATIONAL 3x3)
    pub color_matrix1: Option<Mat3>,
    pub color_matrix2: Option<Mat3>,
    /// camera -> XYZ D50 under illuminant 1/2
    pub forward_matrix1: Option<Mat3>,
    pub forward_matrix2: Option<Mat3>,
    /// per-camera calibration (multiplies ColorMatrix for WB estimation)
    pub cam_calib1: Option<Mat3>,
    pub cam_calib2: Option<Mat3>,
    /// AnalogBalance (usually identity)
    pub analog_balance: Option<Mat3>,
    /// DNG illuminant codes (17=A/2856K, 21=D65/6504K, 23=D50/5003K ...)
    pub illuminant1: u16,
    pub illuminant2: u16,
    pub hsm1: Option<HsMap>,
    pub hsm2: Option<HsMap>,
    pub look_table: Option<HsMap>,
    /// profile tone curve, (x,y) pairs 0..1
    pub tone_curve: Vec<(f32, f32)>,
    /// EV the profile expects applied to the raw data
    pub baseline_exposure: f32,
    /// DNG 1.7 BaselineExposureOffset (0xC7A5): profile-specific EV added
    /// to the file's BaselineExposure for the default render.
    pub baseline_exposure_offset: f32,
    /// camera model the profile is calibrated for (UniqueCameraModel)
    pub unique_model: String,
}

// ---- minimal TIFF reader ---------------------------------------------------

struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
    /// tag -> (type, count, value_offset_or_data)
    tags: HashMap<u16, (u16, u32, usize)>,
}

fn type_size(t: u16) -> usize {
    match t {
        1 | 2 | 6 | 7 => 1, // byte, ascii, sbyte, undefined
        3 | 8 => 2,         // short, sshort
        4 | 9 | 11 => 4,    // long, slong, float
        5 | 10 | 12 => 8,   // rational, srational, double
        _ => 0,
    }
}

impl<'a> Tiff<'a> {
    fn parse(d: &'a [u8]) -> Option<Tiff<'a>> {
        if d.len() < 8 {
            return None;
        }
        let le = match &d[0..2] {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let rd = |o: usize, n: usize| -> Option<&'a [u8]> { d.get(o..o + n) };
        let u16 = |o: usize| -> u16 {
            let b = rd(o, 2).unwrap();
            if le {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        };
        let u32 = |o: usize| -> u32 {
            let b = rd(o, 4).unwrap();
            if le {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            }
        };
        let ifd = u32(4) as usize;
        let n = u16(ifd) as usize;
        let mut tags = HashMap::new();
        for i in 0..n {
            let o = ifd + 2 + i * 12;
            let (tag, typ, cnt) = (u16(o), u16(o + 2), u32(o + 4));
            let sz = type_size(typ) * cnt as usize;
            // value field: inline if <=4 bytes else an offset
            let voff = if sz <= 4 { o + 8 } else { u32(o + 8) as usize };
            if voff + sz <= d.len() {
                tags.insert(tag, (typ, cnt, voff));
            }
        }
        Some(Tiff { d, le, tags })
    }

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
    fn i32(&self, o: usize) -> i32 {
        self.u32(o) as i32
    }
    fn f32(&self, o: usize) -> f32 {
        f32::from_bits(self.u32(o))
    }
    fn f64(&self, o: usize) -> f64 {
        let b = &self.d[o..o + 8];
        f64::from_bits(if self.le {
            u64::from_le_bytes(b.try_into().unwrap())
        } else {
            u64::from_be_bytes(b.try_into().unwrap())
        })
    }
    fn rational(&self, o: usize) -> f32 {
        let (n, dn) = (self.u32(o), self.u32(o + 4));
        if dn == 0 {
            0.0
        } else {
            n as f32 / dn as f32
        }
    }
    fn srational(&self, o: usize) -> f32 {
        let (n, dn) = (self.i32(o), self.i32(o + 4));
        if dn == 0 {
            0.0
        } else {
            n as f32 / dn as f32
        }
    }

    /// read `cnt` numeric values of the tag as f32
    fn num_list(&self, tag: u16) -> Option<Vec<f32>> {
        let (typ, cnt, off) = *self.tags.get(&tag)?;
        let mut out = Vec::with_capacity(cnt as usize);
        for i in 0..cnt as usize {
            let o = off + i * type_size(typ);
            out.push(match typ {
                1 | 7 => self.d[o] as f32,
                3 => self.u16(o) as f32,
                4 => self.u32(o) as f32,
                5 => self.rational(o),
                8 => self.u16(o) as i16 as f32,
                9 => self.i32(o) as f32,
                10 => self.srational(o),
                11 => self.f32(o),
                12 => self.f64(o) as f32,
                _ => return None,
            });
        }
        Some(out)
    }

    fn str_tag(&self, tag: u16) -> Option<String> {
        let (typ, cnt, off) = *self.tags.get(&tag)?;
        if typ != 2 {
            return None;
        }
        let n = (cnt as usize).min(self.d.len() - off);
        let s = std::str::from_utf8(&self.d[off..off + n]).ok()?;
        Some(s.trim_end_matches('\0').to_string())
    }

    fn u32_tag(&self, tag: u16) -> Option<Vec<u32>> {
        self.num_list(tag)
            .map(|v| v.into_iter().map(|x| x.max(0.0) as u32).collect())
    }
}

fn mat3(v: Option<Vec<f32>>) -> Option<Mat3> {
    let v = v?;
    if v.len() < 9 {
        return None;
    }
    Some([[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]])
}

// DNG tag ids — two numberings exist: modern DNG 1.4 and the older
// Adobe draft layout that Lightroom's shipped .dcp files still use
// (profile-related tags shifted by 2). Resolve by tag type.
/// the PV2012+ baseline render tone scale -- dng_sdk `dng_tone_curve_acr3_default`
/// (dng_render.cpp), a 1025-entry table applied per channel in the profile's
/// working space when a DCP carries no ProfileToneCurve (e.g. every
/// `Adobe Standard.dcp`). Strong midtone lift with a long highlight
/// shoulder; values above 1.0 clamp per the reference Eval. Sampled at
/// 65 points and reconstructed with Catmull-Rom (max error <0.003).
pub const ACR_DEFAULT_CURVE: &[(f32, f32)] = &[
    (0.0000, 0.0000),
    (0.0156, 0.0149),
    (0.0312, 0.0400),
    (0.0469, 0.0706),
    (0.0625, 0.1043),
    (0.0781, 0.1406),
    (0.0938, 0.1790),
    (0.1094, 0.2193),
    (0.1250, 0.2596),
    (0.1406, 0.2983),
    (0.1562, 0.3353),
    (0.1719, 0.3705),
    (0.1875, 0.4039),
    (0.2031, 0.4355),
    (0.2188, 0.4655),
    (0.2344, 0.4939),
    (0.2500, 0.5207),
    (0.2656, 0.5461),
    (0.2812, 0.5701),
    (0.2969, 0.5928),
    (0.3125, 0.6144),
    (0.3281, 0.6348),
    (0.3438, 0.6543),
    (0.3594, 0.6727),
    (0.3750, 0.6904),
    (0.3906, 0.7071),
    (0.4062, 0.7231),
    (0.4219, 0.7383),
    (0.4375, 0.7529),
    (0.4531, 0.7668),
    (0.4688, 0.7801),
    (0.4844, 0.7927),
    (0.5000, 0.8049),
    (0.5156, 0.8165),
    (0.5312, 0.8275),
    (0.5469, 0.8381),
    (0.5625, 0.8482),
    (0.5781, 0.8579),
    (0.5938, 0.8672),
    (0.6094, 0.8761),
    (0.6250, 0.8845),
    (0.6406, 0.8926),
    (0.6562, 0.9003),
    (0.6719, 0.9077),
    (0.6875, 0.9148),
    (0.7031, 0.9215),
    (0.7188, 0.9279),
    (0.7344, 0.9340),
    (0.7500, 0.9399),
    (0.7656, 0.9454),
    (0.7812, 0.9507),
    (0.7969, 0.9557),
    (0.8125, 0.9604),
    (0.8281, 0.9649),
    (0.8438, 0.9692),
    (0.8594, 0.9732),
    (0.8750, 0.9770),
    (0.8906, 0.9806),
    (0.9062, 0.9840),
    (0.9219, 0.9871),
    (0.9375, 0.9901),
    (0.9531, 0.9929),
    (0.9688, 0.9954),
    (0.9844, 0.9978),
    (1.0000, 1.0000),
];

const T_UNIQ_MODEL: u16 = 0xC614;
const T_COLMAT1: u16 = 0xC621;
const T_COLMAT2: u16 = 0xC622;
const T_ANALOG: u16 = 0xC627;
const T_BASEEXP: u16 = 0xC62A;
const T_ILLUM1: u16 = 0xC65A;
const T_ILLUM2: u16 = 0xC65B;
const T_CALIB1: u16 = 0xC6F3;
const T_CALIB2: u16 = 0xC6F4; // in old files this slot holds a signature ASCII
const T_FWD1: u16 = 0xC714;
const T_FWD2: u16 = 0xC715;
const T_LOOKDIMS: u16 = 0xC725;
const T_LOOK: u16 = 0xC726;
// name: 0xC6FA (new) / 0xC6F8 (old adobe). hsm: dims 0xC6FB / 0xC6F9,
// data1 0xC6FC / 0xC6FA, data2 0xC6FD / 0xC6FB. tone curve 0xC6FE
// (new, FLOAT) — old files put copyright ASCII there instead.

impl DcpProfile {
    pub fn load(path: &Path) -> Option<DcpProfile> {
        let data = std::fs::read(path).ok()?;
        Self::parse(&data)
    }

    fn str_any(t: &Tiff<'_>, tags: &[u16]) -> Option<String> {
        tags.iter().find_map(|&tg| t.str_tag(tg))
    }
    pub fn parse(d: &[u8]) -> Option<DcpProfile> {
        let t = Tiff::parse(d)?;
        let mut p = DcpProfile {
            name: Self::str_any(&t, &[0xC6FA, 0xC6F8]).unwrap_or_default(),
            copyright: Self::str_any(&t, &[0xC700, 0xC6FE]).unwrap_or_default(),
            unique_model: t.str_tag(T_UNIQ_MODEL).unwrap_or_default(),
            ..Default::default()
        };
        p.color_matrix1 = mat3(t.num_list(T_COLMAT1));
        p.color_matrix2 = mat3(t.num_list(T_COLMAT2));
        p.forward_matrix1 = mat3(t.num_list(T_FWD1));
        p.forward_matrix2 = mat3(t.num_list(T_FWD2));
        // calib tags only count when they're actually matrices (old files
        // stash an ASCII signature at 0xC6F4 instead)
        p.cam_calib1 = mat3(t.num_list(T_CALIB1));
        p.cam_calib2 = mat3(t.num_list(T_CALIB2));
        p.analog_balance = mat3(t.num_list(T_ANALOG));
        p.illuminant1 = t
            .u32_tag(T_ILLUM1)
            .and_then(|v| v.first().copied())
            .unwrap_or(0) as u16;
        p.illuminant2 = t
            .u32_tag(T_ILLUM2)
            .and_then(|v| v.first().copied())
            .unwrap_or(0) as u16;
        p.baseline_exposure = t
            .num_list(T_BASEEXP)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0);
        p.baseline_exposure_offset = t
            .num_list(0xC7A5)
            .and_then(|v| v.first().copied())
            .unwrap_or(0.0);

        // 0xC7A3 ProfileHueSatMapEncoding / 0xC7A4 ProfileLookTableEncoding:
        // 0 = linear, 1 = sRGB-encoded V coordinate.
        let hsm_encoded = t
            .u32_tag(0xC7A3)
            .and_then(|v| v.first().copied())
            .unwrap_or(0)
            == 1;
        let read_hsm = |dims_tag: u16, d1: u16, d2: u16| -> (Option<HsMap>, Option<HsMap>) {
            let dims = t.u32_tag(dims_tag).unwrap_or_default();
            let mut a = None;
            let mut b = None;
            // dims must be a real (hue,sat,val) triple — a data tag read as
            // dims yields zeros/tiny values and a degenerate map that would
            // block the correct fallback layout
            let plausible = dims.len() >= 3
                && dims.iter().all(|&d| (1..=512).contains(&d))
                && dims[0] * dims[1] * dims[2] * 3 >= 3;
            if plausible {
                let want = (dims[0] * dims[1] * dims[2] * 3) as usize;
                for (tag, slot) in [(d1, &mut a), (d2, &mut b)] {
                    if let Some(vals) = t.num_list(tag) {
                        if vals.len() >= want {
                            let mut data = Vec::with_capacity(want / 3);
                            for c in vals[..want].chunks_exact(3) {
                                data.push([c[0], c[1], c[2]]);
                            }
                            *slot = Some(HsMap {
                                dims: [dims[0], dims[1], dims[2]],
                                data,
                                srgb_encoded: hsm_encoded,
                            });
                        }
                    }
                }
            }
            (a, b)
        };
        // modern numbering first, then the old adobe layout (-2)
        let (h1, h2) = read_hsm(0xC6FB, 0xC6FC, 0xC6FD);
        let (h1, h2) = if h1.is_none() && h2.is_none() {
            read_hsm(0xC6F9, 0xC6FA, 0xC6FB)
        } else {
            (h1, h2)
        };
        p.hsm1 = h1;
        p.hsm2 = h2;

        let look_encoded = t
            .u32_tag(0xC7A4)
            .and_then(|v| v.first().copied())
            .unwrap_or(0)
            == 1;
        let lt_dims = t.u32_tag(T_LOOKDIMS).unwrap_or_default();
        if lt_dims.len() >= 3 {
            let want = (lt_dims[0] * lt_dims[1] * lt_dims[2] * 3) as usize;
            if let Some(vals) = t.num_list(T_LOOK) {
                if vals.len() >= want {
                    let mut data = Vec::with_capacity(want / 3);
                    for c in vals[..want].chunks_exact(3) {
                        data.push([c[0], c[1], c[2]]);
                    }
                    p.look_table = Some(HsMap {
                        dims: [lt_dims[0], lt_dims[1], lt_dims[2]],
                        data,
                        srgb_encoded: look_encoded,
                    });
                }
            }
        }
        // ProfileToneCurve: old files use 0xC6FE (copyright ASCII when it
        // isn't numeric); modern Adobe profiles carry it at 0xC6FC — but
        // that slot is also HSM data1 in the pre-1.2 layout, so only take
        // it when the 0xC6FB dims tag is absent and the count is small
        // (curves are a few dozen pairs; HSM data runs into the thousands).
        for tag in [0xC6FEu16, 0xC6FC] {
            let Some(&(ty, cnt, _)) = t.tags.get(&tag) else {
                continue;
            };
            if ty == 2 || cnt > 2048 {
                continue;
            }
            if tag == 0xC6FC && t.tags.contains_key(&0xC6FB) {
                continue;
            }
            if let Some(vals) = t.num_list(tag) {
                if vals.len() >= 4 {
                    for c in vals.chunks_exact(2) {
                        p.tone_curve.push((c[0], c[1]));
                    }
                    break;
                }
            }
        }
        if p.name.is_empty() {
            p.name = p.unique_model.clone();
        }
        Some(p)
    }

    /// interpolation weight between illuminant1 (w=0) and illuminant2 (w=1)
    /// for a given scene white-balance estimate (camera-neutral multipliers,
    /// i.e. 1/WB in normalized form — pass the cam-space white point).
    /// Interpolation follows DNG spec: inverse correlated color temperature.
    pub fn illuminant_weight(&self, wb_neutral: [f32; 3]) -> f32 {
        if self.color_matrix2.is_none() || self.illuminant1 == self.illuminant2 {
            return 0.0;
        }
        let Some(cm1) = self.color_matrix1 else {
            return 0.0;
        };
        let Some(cm2) = self.color_matrix2 else {
            return 0.0;
        };
        // estimate CCT: map cam neutral -> XYZ via inverse of the interpolated
        // ColorMatrix (XYZ->cam), iterate the weight twice to converge.
        let t1 = illum_cct(self.illuminant1);
        let t2 = illum_cct(self.illuminant2);
        if t1 <= 0.0 || t2 <= 0.0 {
            return 0.0;
        }
        let mut w = 0.5f32;
        for _ in 0..3 {
            let m = lerp_mat(cm1, cm2, w);
            if let Some(inv) = invert(m) {
                let xyz = mat_mul_vec(inv, wb_neutral);
                let sum = xyz[0] + xyz[1] + xyz[2];
                if sum.abs() < 1e-6 {
                    break;
                }
                let (x, y) = (xyz[0] / sum, xyz[1] / sum);
                let t = xy_to_cct(x, y);
                if t <= 0.0 {
                    break;
                }
                // inverse CCT interpolation (DNG spec 6.5.5)
                w = (1.0 / t - 1.0 / t1) / (1.0 / t2 - 1.0 / t1);
                w = w.clamp(0.0, 1.0);
            }
        }
        w
    }

    /// camera -> XYZ D50 matrix for the given white balance.
    /// Uses interpolated ForwardMatrix when present; otherwise inverts the
    /// interpolated ColorMatrix (XYZ->cam under its illuminant) and adapts
    /// the result illuminant -> D50 via Bradford CAT.
    pub fn cam_to_xyz_d50(&self, wb_neutral: [f32; 3]) -> Option<Mat3> {
        let w = self.illuminant_weight(wb_neutral);
        match (self.forward_matrix1, self.forward_matrix2) {
            (Some(f1), Some(f2)) => Some(lerp_mat(f1, f2, w)),
            (Some(f1), None) => Some(f1),
            _ => {
                // invert color matrix (XYZ_illum -> cam) => cam -> XYZ_illum,
                // then Bradford-adapt illuminant -> D50.
                let cm = match (self.color_matrix1, self.color_matrix2) {
                    (Some(c1), Some(c2)) => lerp_mat(c1, c2, w),
                    (Some(c1), None) => c1,
                    _ => return None,
                };
                let inv = invert(cm)?;
                let illum = if w > 0.5 {
                    self.illuminant2
                } else {
                    self.illuminant1
                };
                let src = illum_xy(illum).unwrap_or(D65_XY);
                let adapt = bradford(src, D50_XY);
                Some(mat_mul(adapt, inv))
            }
        }
    }

    /// interpolation weight for a known CCT (kelvin) — used when the
    /// recipe specifies absolute temperature so the target illuminant
    /// is already known without WB estimation.
    pub fn weight_for_cct(&self, t: f32) -> f32 {
        let (t1, t2) = (illum_cct(self.illuminant1), illum_cct(self.illuminant2));
        if t1 <= 0.0 || t2 <= 0.0 || (t1 - t2).abs() < 1.0 {
            return 0.0;
        }
        ((1.0 / t - 1.0 / t1) / (1.0 / t2 - 1.0 / t1)).clamp(0.0, 1.0)
    }

    /// interpolated XYZ->cam ColorMatrix for a known CCT.
    pub fn color_matrix_at(&self, t: f32) -> Option<Mat3> {
        match (self.color_matrix1, self.color_matrix2) {
            (Some(a), Some(b)) => Some(lerp_mat(a, b, self.weight_for_cct(t))),
            (Some(a), None) => Some(a),
            _ => None,
        }
    }

    /// HueSatMap for the current WB interpolation weight.
    pub fn hue_sat_map(&self, wb_neutral: [f32; 3]) -> Option<&HsMap> {
        match (&self.hsm1, &self.hsm2) {
            (Some(h), None) => Some(h),
            (None, Some(h)) => Some(h),
            (Some(a), Some(b)) => {
                let w = self.illuminant_weight(wb_neutral);
                Some(if w < 0.5 { a } else { b })
            }
            _ => None,
        }
    }
}

// ---- matrix / colorimetry helpers -----------------------------------------

pub fn lerp_mat(a: Mat3, b: Mat3, t: f32) -> Mat3 {
    let mut m = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[i][j] + (b[i][j] - a[i][j]) * t;
        }
    }
    m
}

pub fn invert(m: Mat3) -> Option<Mat3> {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-9 {
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

pub fn mat_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut m = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    m
}

pub fn mat_mul_vec(m: Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// kelvin -> white-point xy. Planckian locus below 4000 K, CIE D-series
/// daylight locus above (matches how Adobe derives WB from Temperature).
pub fn kelvin_to_xy(t: f32) -> [f32; 2] {
    let t = t.clamp(1500.0, 40000.0);
    let x = if t <= 4000.0 {
        // Planckian approximation (valid 1667..4000K)
        -0.2661239e9 / (t * t * t) - 0.2343589e6 / (t * t) + 0.8776956e3 / t + 0.179910
    } else {
        // CIE daylight locus (valid 4000..25000K)
        -3.0258469e9 / (t * t * t) + 2.1070379e6 / (t * t) + 0.2226347e3 / t + 0.240390
    };
    let y = if t <= 2222.0 {
        -1.1063814 * x * x * x - 1.34811020 * x * x + 2.18555832 * x - 0.20219683
    } else if t <= 4000.0 {
        -0.9549476 * x * x * x - 1.37418593 * x * x + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x * x * x - 5.87338670 * x * x + 3.75112997 * x - 0.37001483
    };
    [x, y]
}

pub fn xy_to_xyz(xy: [f32; 2]) -> [f32; 3] {
    [
        xy[0] / xy[1].max(1e-6),
        1.0,
        (1.0 - xy[0] - xy[1]) / xy[1].max(1e-6),
    ]
}

/// XYZ (D65) -> linear sRGB
pub const XYZ_TO_SRGB: Mat3 = [
    [3.2404542, -1.5371385, -0.4985314],
    [-0.9692660, 1.8760108, 0.0415560],
    [0.0556434, -0.2040259, 1.0572252],
];

/// DNG calibration illuminant -> approximate CCT (kelvin)
fn illum_cct(code: u16) -> f32 {
    match code {
        1 => 5500.0,  // daylight (old spec)
        3 => 3200.0,  // tungsten
        4 => 3400.0,  // warm fluorescent? -> flash per DNG: 4 = flash 5500
        9 => 5500.0,  // fine weather
        10 => 6500.0, // flash? DNG: 10=cloudy 6500
        11 => 7500.0, // shade
        12 => 5000.0, // D50? daylight fluorescent
        14 => 3000.0, // cool white fluorescent ~4100 actually; approx
        15 => 6500.0, // white fluorescent? approx
        16 => 3200.0, // warm white fluorescent ~3000
        17 => 2856.0, // standard light A
        18 => 2856.0, // standard light B (rare)
        19 => 2856.0, // standard light C (rare) ~6774 actually; seldom used
        20 => 5500.0, // D55
        21 => 6504.0, // D65
        22 => 7500.0, // D75
        23 => 5003.0, // D50
        24 => 5700.0, // ISO studio tungsten -> actually 3200
        26 => 5500.0, // ISO speedlight
        30 => 4000.0, // LED-ish approximations
        _ => 5500.0,
    }
}

/// DNG calibration illuminant -> white-point xy
fn illum_xy(code: u16) -> Option<[f32; 2]> {
    Some(match code {
        17 | 18 => [0.44757, 0.40745], // A
        21 => D65_XY,
        23 => D50_XY,
        20 => [0.33242, 0.34743], // D55
        22 => [0.29902, 0.31485], // D75
        3 => [0.423, 0.399],      // tungsten ~3200K
        _ => return None,
    })
}

pub const D50_XY: [f32; 2] = [0.34567, 0.35850];
pub const D65_XY: [f32; 2] = [0.31271, 0.32902];

fn xy_to_cct(x: f32, y: f32) -> f32 {
    // McCamy approximation (good 3000..20000K)
    let n = (x - 0.3320) / (0.1858 - y).max(1e-6);
    let cct = 449.0 * n * n * n + 3525.0 * n * n + 6823.3 * n + 5520.33;
    if cct.is_finite() && cct > 1000.0 && cct < 100000.0 {
        cct
    } else {
        0.0
    }
}

/// Bradford chromatic adaptation matrix src_xy -> dst_xy (XYZ space)
pub fn bradford(src: [f32; 2], dst: [f32; 2]) -> Mat3 {
    fn xyz_from_xy(xy: [f32; 2]) -> [f32; 3] {
        [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
    }
    const BRADFORD: Mat3 = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    let bradford_inv = invert(BRADFORD).unwrap();
    let s = mat_mul_vec(BRADFORD, xyz_from_xy(src));
    let d = mat_mul_vec(BRADFORD, xyz_from_xy(dst));
    let scale = [
        [d[0] / s[0], 0.0, 0.0],
        [0.0, d[1] / s[1], 0.0],
        [0.0, 0.0, d[2] / s[2]],
    ];
    mat_mul(bradford_inv, mat_mul(scale, BRADFORD))
}

/// XYZ D50 -> linear sRGB (D65) conversion matrix (Bradford-adapted).
pub fn xyz_d50_to_srgb() -> Mat3 {
    const XYZ_D65_TO_SRGB: Mat3 = [
        [3.2404542, -1.5371385, -0.4985314],
        [-0.9692660, 1.8760108, 0.0415560],
        [0.0556434, -0.2040259, 1.0572252],
    ];
    mat_mul(XYZ_D65_TO_SRGB, bradford(D50_XY, D65_XY))
}

// ---- profile lookup ---------------------------------------------------------

/// directories searched for .dcp files, in priority order.
pub fn profile_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(d) = std::env::var("SAFELIGHT_PROFILE_DIR") {
        out.push(PathBuf::from(d));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        out.push(PathBuf::from(home).join(".safelight/profiles"));
    }
    // Lightroom Classic install: user-installed Adobe profiles are fair game
    // to READ (they live on this machine, we never ship them).
    #[cfg(target_os = "windows")]
    {
        out.push(PathBuf::from(
            r"C:\Program Files\Adobe\Adobe Lightroom Classic\Resources\CameraProfiles",
        ));
        out.push(PathBuf::from(
            r"C:\Program Files\Adobe\Adobe Lightroom Classic\Resources\Settings",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Adobe Lightroom Classic/Adobe Lightroom Classic.app/Contents/Resources/CameraProfiles",
        ));
        out.push(PathBuf::from(
            "/Library/Application Support/Adobe/CameraRaw/CameraProfiles",
        ));
    }
    out
}

/// find a .dcp for `make`/`model` + profile name ("Adobe Standard",
/// "Camera Standard", ...). Matches on the UniqueCameraModel tag when the
/// filename doesn't carry it.
pub fn find_profile(make: &str, model: &str, name: &str) -> Option<PathBuf> {
    let model = model.trim();
    let mk = make.trim();
    // canonical adobe layout: "Adobe Standard/<Make> <Model> Adobe Standard.dcp"
    // and "Camera/<Make> <Model>/<Make> <Model> Camera <Look>.dcp".
    // match space-insensitively so "ILCE-7RM3" matches "ILCE - 7RM3" variants.
    let norm = |s: &str| s.to_lowercase().replace([' ', '_'], "");
    let name_n = norm(name);
    for dir in profile_dirs() {
        let mut candidates: Vec<PathBuf> = Vec::new();
        let want_file = format!("{} {} {}.dcp", mk, model, name);
        let subdir = format!("{} {}", mk, model);
        for root in [
            dir.clone(),
            dir.join("Adobe Standard"),
            dir.join("Camera").join(&subdir),
            dir.join("Camera"),
        ] {
            if !root.is_dir() {
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) != Some("dcp") {
                        continue;
                    }
                    let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if fname == want_file {
                        candidates.insert(0, p);
                        continue;
                    }
                    let fl = norm(fname);
                    if fl.contains(&norm(model)) && fl.contains(&name_n) {
                        candidates.push(p);
                    }
                }
            }
            // one level deeper (Camera/<Make> <Model>/*.dcp)
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let sub = e.path();
                    if !sub.is_dir()
                        || !norm(sub.file_name().and_then(|s| s.to_str()).unwrap_or(""))
                            .contains(&norm(model))
                    {
                        continue;
                    }
                    if let Ok(rd2) = std::fs::read_dir(&sub) {
                        for e2 in rd2.flatten() {
                            let p = e2.path();
                            if p.extension().and_then(|s| s.to_str()) != Some("dcp") {
                                continue;
                            }
                            let fl = norm(p.file_name().and_then(|s| s.to_str()).unwrap_or(""));
                            if fl.contains(&name_n) {
                                candidates.push(p);
                            }
                        }
                    }
                }
            }
        }
        if let Some(p) = candidates.into_iter().next() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_encoded_map_v_axis() {
        // dng_reference.cpp RefBaselineHueSatMap: an encoded map samples the
        // table at srgb_encode(v) and the valScale multiplies the encoded
        // value before decode — for dark pixels a scale of 1.0 at the
        // encoded coord is identity, while sampling at linear v would read
        // a different cell entirely.
        // 1 hue × 1 sat? need >=2 sat to interpolate; use 2x2x2 grid where
        // val scale differs by v cell: 0.5 at v=0 cell, 2.0 at v=1 cell.
        let mut data = Vec::new();
        for v in 0..2 {
            for _h in 0..2 {
                for _s in 0..2 {
                    data.push([0.0, 1.0, if v == 0 { 0.5 } else { 2.0 }]);
                }
            }
        }
        let linear = HsMap {
            dims: [2, 2, 2],
            data: data.clone(),
            srgb_encoded: false,
        };
        let enc = HsMap {
            dims: [2, 2, 2],
            data,
            srgb_encoded: true,
        };
        // dark neutral pixel: linear v=0.02 -> sample near v cell 0
        // (scale 0.5 -> v~0.01); encoded v = srgb(0.02)=0.156 -> still cell
        // 0-ish but valScale applies in encoded domain then decodes.
        let lin_out = linear.apply([0.02, 0.02, 0.02])[0];
        let enc_out = enc.apply([0.02, 0.02, 0.02])[0];
        assert!((lin_out - 0.01).abs() < 3e-3, "lin_out={lin_out}");
        let expected = {
            // v_enc = srgb_encode(0.02); the v-axis interpolant t = v_enc
            // blends cell scales .5 and 2.0: scale = .5 + 1.5*t; the scaled
            // encoded v then decodes back to linear
            let v_enc = 1.055 * 0.02f32.powf(1.0 / 2.4) - 0.055;
            let scale = 0.5 + 1.5 * v_enc;
            let v2 = (v_enc * scale).clamp(0.0, 1.0);
            if v2 <= 0.04045 {
                v2 / 12.92
            } else {
                ((v2 + 0.055) / 1.055).powf(2.4)
            }
        };
        assert!(
            (enc_out - expected).abs() < 1e-4,
            "enc_out={enc_out} expected={expected} lin_out={lin_out}"
        );
        assert!(enc_out != lin_out, "encoding must change the result");
    }

    #[test]
    fn unencoded_map_unchanged() {
        // identity map (hue shift 0, scales 1) is a no-op either way
        let data = vec![[0.0, 1.0, 1.0]; 8];
        for enc in [false, true] {
            let m = HsMap {
                dims: [2, 2, 2],
                data: data.clone(),
                srgb_encoded: enc,
            };
            let o = m.apply([0.3, 0.5, 0.2]);
            assert!((o[0] - 0.3).abs() < 1e-3 && (o[1] - 0.5).abs() < 1e-3);
        }
    }
}
