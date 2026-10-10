//! Lightroom-compatible XMP sidecar IO.
//!
//! `write_sidecar` also emits `<photo>.xmp` mirroring the recipe in the
//! `crs:` (Camera Raw Settings) namespace so Lightroom Classic / Camera Raw /
//! darktable can read our basic adjustments — and we read the same fields
//! back when a folder contains LR-authored XMP without an `.safelight.json`.
//!
//! Round-trip fidelity: LR fields cover only a subset of the recipe and
//! rescale our [-1,1] ranges to LR's integer [-100,+100] convention, so the
//! full recipe is also embedded verbatim in `<ara:Recipe>` — safelight reads
//! that first and treats crs: only as a fallback for foreign files.

use crate::recipe::{Recipe, Sidecar};
use std::path::{Path, PathBuf};

pub fn xmp_path_for(asset: &Path) -> PathBuf {
    asset.with_extension("xmp")
}

/// darktable's convention (`photo.RAF.xmp` — extension kept); Lightroom
/// uses the stem-level `photo.xmp`, which is what we write.
fn xmp_path_for_darktable(asset: &Path) -> PathBuf {
    let mut p = asset.as_os_str().to_os_string();
    p.push(".xmp");
    PathBuf::from(p)
}

/// Read whichever sidecar spelling exists: ours/LR (`stem.xmp`) first,
/// then darktable's (`stem.ext.xmp`).
pub fn find_xmp(asset: &Path) -> Option<PathBuf> {
    let lr = xmp_path_for(asset);
    if lr.exists() {
        return Some(lr);
    }
    let dt = xmp_path_for_darktable(asset);
    if dt.exists() {
        return Some(dt);
    }
    None
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// -1..1 recipe field -> LR integer string (+/- prefix on positives)
fn lr(v: f32, scale: f32) -> String {
    let n = (v * scale).round() as i32;
    format!("{n:+}")
}

/// Serialize a sidecar to LR-readable XMP. The `ara:` block carries the
/// exact recipe JSON for lossless round-trips through our own files.
pub fn write_xmp(asset: &Path, sc: &Sidecar) -> std::io::Result<()> {
    let r = &sc.recipe;
    let mut a = String::with_capacity(4096);
    a.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    a.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"safelight\">\n");
    a.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    a.push_str("  <rdf:Description rdf:about=\"\"\n");
    a.push_str("   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n");
    a.push_str("   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n");
    a.push_str("   xmlns:sl=\"https://safelight.app/ns/1.0/\"\n");
    a.push_str("   crs:Version=\"15.0\" crs:ProcessVersion=\"11.0\" crs:RawFileName=\"");
    a.push_str(&esc(&asset
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()));
    a.push_str("\"\n");
    // WB: LR stores absolute Kelvin/Tint for "As Shot", incremental for custom
    if r.temp_abs > 0.0 {
        a.push_str(&format!(
            "   crs:WhiteBalance=\"Custom\" crs:Temperature=\"{:.0}\" crs:Tint=\"{:+.0}\"\n",
            r.temp_abs,
            r.tint_abs.clamp(-150.0, 150.0)
        ));
    } else {
        a.push_str(&format!(
            "   crs:WhiteBalance=\"Custom\" crs:IncrementalTemperature=\"{}\" crs:IncrementalTint=\"{}\"\n",
            lr(r.temperature, 100.0),
            lr(r.tint, 100.0)
        ));
    }
    if !r.profile.is_empty() {
        a.push_str(&format!("   crs:CameraProfile=\"{}\"\n", esc(&r.profile)));
    }
    a.push_str(&format!(
        "   crs:Exposure2012=\"{:+.2}\" crs:Contrast2012=\"{}\"\n",
        r.exposure,
        lr(r.contrast, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Highlights2012=\"{}\" crs:Shadows2012=\"{}\" crs:Whites2012=\"{}\" crs:Blacks2012=\"{}\"\n",
        lr(r.highlights, 100.0),
        lr(r.shadows, 100.0),
        lr(r.whites, 100.0),
        lr(r.blacks, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Clarity2012=\"{}\" crs:Dehaze=\"{}\" crs:Texture=\"0\"\n",
        lr(r.clarity, 100.0),
        lr(r.dehaze, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Vibrance=\"{}\" crs:Saturation=\"{}\"\n",
        lr(r.vibrance, 100.0),
        lr(r.saturation, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Sharpness=\"{}\" crs:LuminanceSmoothing=\"{}\" crs:ColorNoiseReduction=\"{}\"\n",
        lr(r.sharpen, 150.0),
        lr(r.noise_luma, 100.0),
        lr(r.noise_chroma, 100.0)
    ));
    if r.crop != [0.0, 0.0, 0.0, 0.0] {
        a.push_str(&format!(
            "   crs:HasCrop=\"true\" crs:CropTop=\"{:.6}\" crs:CropLeft=\"{:.6}\" crs:CropBottom=\"{:.6}\" crs:CropRight=\"{:.6}\" crs:CropAngle=\"{:.2}\"\n",
            r.crop[1], r.crop[0], 1.0 - r.crop[3], 1.0 - r.crop[2], r.rotation_deg
        ));
    } else if r.rotation_deg.abs() > 0.01 {
        a.push_str(&format!("   crs:CropAngle=\"{:.2}\"\n", r.rotation_deg));
    }
    a.push_str(&format!(
        "   crs:PostCropVignetteAmount=\"{}\" crs:GrainAmount=\"{}\"\n",
        lr(r.vignette, 100.0),
        lr(r.grain, 100.0)
    ));
    if sc.rating != 0 {
        a.push_str(&format!("   xmp:Rating=\"{}\"\n", sc.rating.clamp(-1, 5)));
    }
    if !sc.label.is_empty() {
        a.push_str(&format!("   xmp:Label=\"{}\"\n", esc(&sc.label)));
    }
    a.push_str("   ara:Format=\"1\">\n");
    // LR-readable local corrections: gradient/circle windows -> the legacy
    // correction lists LR still reads on import.
    let local_attrs = |w: &crate::recipe::PowerWindow| {
        format!(
            "crs:LocalExposure2012=\"{:+.2}\" crs:LocalSaturation=\"{:.2}\" crs:LocalTemperature=\"{:.0}\"",
            w.ev,
            w.sat * 100.0,
            w.temp * 100.0
        )
    };
    let grads: Vec<_> = r
        .windows
        .iter()
        .filter(|w| w.enabled && w.kind == "gradient")
        .collect();
    if !grads.is_empty() {
        a.push_str("   <crs:GradientBasedCorrections><rdf:Seq>\n");
        for w in grads {
            let mid = (1.0 - w.mid.clamp(0.01, 0.99)) * 100.0;
            a.push_str(&format!(
                "    <rdf:li crs:What=\"Correction\" crs:CorrectionAmount=\"{:.3}\" crs:CorrectionActive=\"true\" {}>\n     <crs:CorrectionMasks><rdf:Seq>\n      <rdf:li crs:What=\"Mask/Gradient\" crs:MaskValue=\"1\" crs:ZeroX=\"{:.6}\" crs:ZeroY=\"{:.6}\" crs:FullX=\"{:.6}\" crs:FullY=\"{:.6}\" crs:Top=\"0\" crs:Bottom=\"1\" crs:Left=\"0\" crs:Right=\"1\" crs:Angle=\"0\" crs:Midpoint=\"{:.0}\" crs:Feather=\"{:.0}\" crs:Flipped=\"false\" crs:Version=\"1\"/>\n     </rdf:Seq></crs:CorrectionMasks>\n    </rdf:li>\n",
                w.opacity.clamp(0.0, 1.0),
                local_attrs(w),
                w.p[2],
                w.p[3],
                w.p[0],
                w.p[1],
                mid,
                w.p[5] * 100.0
            ));
        }
        a.push_str("   </rdf:Seq></crs:GradientBasedCorrections>\n");
    }
    let circs: Vec<_> = r
        .windows
        .iter()
        .filter(|w| w.enabled && w.kind == "circle")
        .collect();
    if !circs.is_empty() {
        a.push_str("   <crs:CircularGradientBasedCorrections><rdf:Seq>\n");
        for w in circs {
            let mid = (1.0 - w.mid.clamp(0.01, 0.99)) * 100.0;
            a.push_str(&format!(
                "    <rdf:li crs:What=\"Correction\" crs:CorrectionAmount=\"{:.3}\" crs:CorrectionActive=\"true\" {}>\n     <crs:CorrectionMasks><rdf:Seq>\n      <rdf:li crs:What=\"Mask/CircularGradient\" crs:MaskValue=\"1\" crs:Top=\"{:.6}\" crs:Bottom=\"{:.6}\" crs:Left=\"{:.6}\" crs:Right=\"{:.6}\" crs:Angle=\"{:.1}\" crs:Midpoint=\"{:.0}\" crs:Roundness=\"0\" crs:Feather=\"{:.0}\" crs:Flipped=\"{}\" crs:Version=\"1\"/>\n     </rdf:Seq></crs:CorrectionMasks>\n    </rdf:li>\n",
                w.opacity.clamp(0.0, 1.0),
                local_attrs(w),
                w.p[1] - w.p[3],
                w.p[1] + w.p[3],
                w.p[0] - w.p[2],
                w.p[0] + w.p[2],
                w.p[4],
                mid,
                w.p[5] * 100.0,
                w.invert
            ));
        }
        a.push_str("   </rdf:Seq></crs:CircularGradientBasedCorrections>\n");
    }
    // full-fidelity recipe for our own round-trip
    if let Ok(js) = serde_json::to_string(&sc.recipe) {
        a.push_str(&format!("   <ara:Recipe>{}</ara:Recipe>\n", esc(&js)));
    }
    if !sc.keywords.is_empty() {
        a.push_str("   <dc:subject xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><rdf:Bag>");
        for k in &sc.keywords {
            a.push_str(&format!("<rdf:li>{}</rdf:li>", esc(k)));
        }
        a.push_str("</rdf:Bag></dc:subject>\n");
    }
    a.push_str("  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n");
    std::fs::write(xmp_path_for(asset), a)
}

/// Some fields exist under both spellings in the wild (LR writes
/// `crs:Saturation`, other tools `crs:Saturation2012`) — try both.
fn attr_any(body: &str, names: &[&str]) -> Option<f32> {
    names.iter().find_map(|n| attr(body, n))
}

fn attr(body: &str, name: &str) -> Option<f32> {
    // crs:Exposure2012="+0.55" attribute form
    let pat = format!("{name}=\"");
    let i = body.find(&pat)? + pat.len();
    let j = body[i..].find('"')? + i;
    body[i..j].parse().ok()
}

fn attr_str(body: &str, name: &str) -> Option<String> {
    let pat = format!("{name}=\"");
    let i = body.find(&pat)? + pat.len();
    let j = body[i..].find('"')? + i;
    Some(body[i..j].to_string())
}

/// Parse an XMP sidecar (attribute or element form). Prefers the embedded
/// `<ara:Recipe>` JSON when present (our own files round-trip losslessly);
/// otherwise maps the crs:/xmp: fields it finds into a partial recipe.
/// XMP packet embedded in the asset itself: DNG/TIFF tag 0x2BC, or a
/// JPEG APP1 `http://ns.adobe.com/xap/1.0/` segment. Lightroom stores
/// develop settings there when it can't (or doesn't) write a sidecar.
fn embedded_xmp(asset: &Path) -> Option<String> {
    let bytes = std::fs::read(asset).ok()?;
    // TIFF/DNG: XMLPacket tag in any IFD
    if bytes.starts_with(b"II") || bytes.starts_with(b"MM") {
        if let Some(pkt) = crate::dngops::tag_bytes(asset, 0x2BC) {
            if let Ok(s) = String::from_utf8(pkt) {
                if s.contains("xmpmeta") || s.contains("crs:") {
                    return Some(s);
                }
            }
        }
        return None;
    }
    // JPEG: walk APP1 segments for the XMP namespace header
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut p = 2usize;
    while p + 4 <= bytes.len() && bytes[p] == 0xFF {
        let (marker, len) = (bytes[p + 1], u16::from_be_bytes([bytes[p + 2], bytes[p + 3]]) as usize);
        let seg_end = (p + 2 + len).min(bytes.len());
        if marker == 0xE1 {
            let seg = &bytes[p + 4..seg_end];
            if seg.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                return String::from_utf8(seg[29..].to_vec()).ok();
            }
        }
        if len < 2 || marker == 0xDA {
            break;
        }
        p += 2 + len;
    }
    None
}

fn parse_xmp_body(body: &str) -> Sidecar {
    let mut sc = Sidecar::default();
    // lossless path: our own embedded recipe
    if let (Some(i), Some(j)) = (body.find("<ara:Recipe>"), body.find("</ara:Recipe>")) {
        let js = unescape(body[i + 12..j].trim());
        if let Ok(r) = serde_json::from_str::<Recipe>(&js) {
            sc.recipe = r;
        }
    } else {
        sc.recipe = crs_to_recipe(body);
    }
    if let Some(v) = attr(body, "xmp:Rating") {
        sc.rating = v.round() as i32;
    }
    if let Some(l) = attr_str(body, "xmp:Label") {
        sc.label = l;
    }
    sc
}

pub fn read_xmp(asset: &Path) -> Option<Sidecar> {
    if let Some(p) = find_xmp(asset) {
        return std::fs::read_to_string(p)
            .ok()
            .map(|b| parse_xmp_body(&b));
    }
    // no sidecar: Lightroom may have written into the file itself
    embedded_xmp(asset).map(|b| parse_xmp_body(&b))
}

/// map crs:* (Camera Raw Settings) fields in an XMP document — Lightroom
/// develop preset or sidecar — into a Recipe. Unknown fields are ignored;
/// this is a best-effort transfer, not a full PV2012 emulation.
pub fn crs_to_recipe(body: &str) -> Recipe {
    let mut sc = Sidecar::default();
    {
        let r = &mut sc.recipe;
        // white balance: absolute Kelvin/Tint or incremental shifts
        match attr_str(&body, "crs:WhiteBalance").as_deref() {
            Some("As Shot") => r.wb_mode = crate::recipe::WbMode::AsShot,
            Some("Auto") => r.wb_mode = crate::recipe::WbMode::Auto,
            _ => {}
        }
        if let Some(v) = attr(&body, "crs:Temperature") {
            r.temp_abs = v;
        }
        if let Some(v) = attr(&body, "crs:Tint") {
            r.tint_abs = v;
        }
        if let Some(v) = attr(&body, "crs:IncrementalTemperature") {
            r.temperature = v / 100.0;
        }
        if let Some(v) = attr(&body, "crs:IncrementalTint") {
            r.tint = v / 100.0;
        }
        if let Some(p) = attr_str(&body, "crs:CameraProfile") {
            // LR writes display names; "Default Profile" = Adobe Standard
            r.profile = match p.as_str() {
                "Default Profile" | "Default" => "Adobe Standard".to_string(),
                _ => p,
            };
        }
        if let Some(v) = attr_any(&body, &["crs:Exposure2012", "crs:Exposure"]) {
            r.exposure = v;
        }
        if let Some(v) = attr_any(&body, &["crs:Contrast2012", "crs:Contrast"]) {
            r.contrast = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Highlights2012", "crs:Highlight2012"]) {
            r.highlights = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Shadows2012", "crs:Shadow2012"]) {
            r.shadows = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Whites2012", "crs:Whites"]) {
            r.whites = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Blacks2012", "crs:Blacks"]) {
            r.blacks = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Clarity2012", "crs:Clarity"]) {
            r.clarity = v / 100.0;
        }
        if let Some(v) = attr(&body, "crs:Texture") {
            // LR texture ~ clarity at a finer scale; contribute a fraction
            // of it to clarity so LR-authored files still read sensibly.
            r.clarity = (r.clarity + v / 100.0 * 0.5).clamp(-1.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:Dehaze") {
            r.dehaze = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Vibrance", "crs:Vibrance2012"]) {
            r.vibrance = v / 100.0;
        }
        if let Some(v) = attr_any(&body, &["crs:Saturation", "crs:Saturation2012"]) {
            r.saturation = v / 100.0;
        }
        if let Some(v) = attr(&body, "crs:Sharpness") {
            r.sharpen = (v / 150.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:LuminanceSmoothing") {
            r.noise_luma = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:ColorNoiseReduction") {
            r.noise_chroma = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:PostCropVignetteAmount") {
            r.vignette = v / 100.0;
        }
        if let Some(v) = attr(&body, "crs:GrainAmount") {
            r.grain = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:CropAngle") {
            r.rotation_deg = v;
        }
        if attr_str(&body, "crs:HasCrop").as_deref() == Some("true") {
            let l = attr(&body, "crs:CropLeft").unwrap_or(0.0);
            let t = attr(&body, "crs:CropTop").unwrap_or(0.0);
            let b = attr(&body, "crs:CropBottom").unwrap_or(1.0);
            let rr = attr(&body, "crs:CropRight").unwrap_or(1.0);
            r.crop = [l, t, 1.0 - rr, 1.0 - b];
        }
        // lens corrections
        if let Some(v) = attr(&body, "crs:LensProfileEnable") {
            r.lens_corr = v.clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:AutoLateralCA") {
            r.ca_fix = if v >= 0.5 { 0.3 } else { 0.0 };
        }
        // legacy split toning + PV2012 color grading share our
        // shadow/mid/highlight hue-sat fields
        let sh = attr(&body, "crs:SplitToningShadowHue")
            .or_else(|| attr(&body, "crs:ColorGradeShadowHue"));
        let ss = attr(&body, "crs:SplitToningShadowSaturation")
            .or_else(|| attr(&body, "crs:ColorGradeShadowSat"));
        if let Some(v) = sh {
            r.shadow_hue = v / 360.0;
        }
        if let Some(v) = ss {
            r.shadow_sat = (v / 100.0).clamp(0.0, 1.0);
        }
        let hh = attr(&body, "crs:SplitToningHighlightHue")
            .or_else(|| attr(&body, "crs:ColorGradeHighlightHue"));
        let hs = attr(&body, "crs:SplitToningHighlightSaturation")
            .or_else(|| attr(&body, "crs:ColorGradeHighlightSat"));
        if let Some(v) = hh {
            r.highlight_hue = v / 360.0;
        }
        if let Some(v) = hs {
            r.highlight_sat = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:ColorGradeMidtoneHue") {
            r.midtone_hue = v / 360.0;
        }
        if let Some(v) = attr(&body, "crs:ColorGradeMidtoneSat") {
            r.midtone_sat = (v / 100.0).clamp(0.0, 1.0);
        }
        // PV2012 parametric tone curve -> sampled into our point curve
        if let Some(curve_pts) = parametric_curve(&body) {
            r.curve = curve_pts;
        }
        // explicit point curves (rdf:Seq of "x, y" strings)
        if let Some(pts) = curve_seq(&body, "crs:ToneCurvePV2012") {
            r.curve = pts;
        }
        if let Some(pts) = curve_seq(&body, "crs:ToneCurvePV2012Red") {
            r.curve_r = pts;
        }
        if let Some(pts) = curve_seq(&body, "crs:ToneCurvePV2012Green") {
            r.curve_g = pts;
        }
        if let Some(pts) = curve_seq(&body, "crs:ToneCurvePV2012Blue") {
            r.curve_b = pts;
        }
        // HSL: 8 adobe bands -> hue_hue / hue_sat / hue_lum point curves
        hsl_adjustments(&body, r);
        // grayscale mixer
        if attr_str(&body, "crs:ConvertToGrayscale").as_deref() == Some("True")
            || attr_str(&body, "crs:ConvertToGrayscale").as_deref() == Some("true")
        {
            let mut w = [1.0f32; 3];
            let band = |name: &str| attr(&body, name).unwrap_or(0.0) / 100.0;
            let (rd, or_, ye, gr, aq, bl, pu, ma) = (
                band("crs:GrayMixerRed"),
                band("crs:GrayMixerOrange"),
                band("crs:GrayMixerYellow"),
                band("crs:GrayMixerGreen"),
                band("crs:GrayMixerAqua"),
                band("crs:GrayMixerBlue"),
                band("crs:GrayMixerPurple"),
                band("crs:GrayMixerMagenta"),
            );
            w[0] += rd * 0.6 + or_ * 0.3 + ma * 0.1;
            w[1] += gr * 0.5 + ye * 0.3 + aq * 0.2;
            w[2] += bl * 0.6 + pu * 0.3 + aq * 0.1;
            r.mono = w;
        }
        // local corrections: graduated/radial filters -> power windows,
        // brush dabs -> adjustment-brush layers.
        local_corrections(&body, r);
    }
    sc.recipe
}

/// `crs:NAME="v"` attribute OR `<crs:NAME>v</crs:NAME>` element form.
fn field_str(body: &str, name: &str) -> Option<String> {
    if let Some(v) = attr_str(body, name) {
        return Some(v);
    }
    let open = format!("<{name}>");
    let i = body.find(&open)? + open.len();
    let e = body[i..].find(&format!("</{name}>"))? + i;
    Some(body[i..e].trim().to_string())
}

fn field(body: &str, name: &str) -> Option<f32> {
    field_str(body, name)?.parse().ok()
}

fn field_any(body: &str, names: &[&str]) -> Option<f32> {
    names.iter().find_map(|n| field(body, n))
}

fn truthy(v: Option<String>) -> bool {
    matches!(v.as_deref(), Some("true") | Some("True") | Some("1"))
}

/// depth-aware split into top-level `<rdf:li>` elements (self-closing or
/// paired); each returned item is the element's full text.
fn li_blocks(seq: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < seq.len() {
        let rest = &seq[i..];
        if rest.starts_with("</rdf:li>") {
            depth -= 1;
            i += 9;
            if depth == 0 {
                items.push(seq[start..i].to_string());
            }
        } else if rest.starts_with("<rdf:li") {
            let gt = rest.find('>').map(|x| x + i).unwrap_or(seq.len() - 1);
            let selfclose = seq[..gt].ends_with('/');
            if depth == 0 && selfclose {
                items.push(seq[i..=gt].to_string());
            } else if depth == 0 {
                start = i;
                depth = 1;
            } else if !selfclose {
                depth += 1;
            }
            i = gt + 1;
        } else {
            i += 1;
        }
    }
    items
}

/// top-level `<rdf:li>` items inside `<tag>…</tag>` — li_blocks itself is
/// depth-aware, so the whole element body can be scanned directly.
fn seq_items(body: &str, tag: &str) -> Vec<String> {
    let Some(ti) = body.find(&format!("<{tag}")) else {
        return Vec::new();
    };
    let Some(gt) = body[ti..].find('>') else {
        return Vec::new();
    };
    let si = ti + gt + 1;
    let Some(ei) = body[si..].find(&format!("</{tag}>")) else {
        return Vec::new();
    };
    li_blocks(&body[si..si + ei])
}

/// every `<rdf:li>` in `item` carrying `crs:What="Mask/{what}"`.
fn mask_blocks(item: &str, what: &str) -> Vec<String> {
    let pat = format!("Mask/{what}");
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = item[from..].find(&pat) {
        let pos = from + rel;
        if let Some(ls) = item[..pos].rfind("<rdf:li") {
            out.extend(li_blocks(&item[ls..]).into_iter().next());
        }
        from = pos + pat.len();
    }
    out
}

/// LR local corrections (graduated filter, radial filter, adjustment
/// brush) → our power windows and brush layers. Handles both the legacy
/// per-kind lists and the unified LR11+ MaskGroupBasedCorrections.
fn local_corrections(body: &str, r: &mut Recipe) {
    use crate::recipe::{BrushLayer, BrushStroke, PowerWindow};
    for tag in [
        "crs:GradientBasedCorrections",
        "crs:CircularGradientBasedCorrections",
        "crs:PaintBasedCorrections",
        "crs:MaskGroupBasedCorrections",
    ] {
        for item in seq_items(body, tag) {
            let what = field_str(&item, "crs:What").unwrap_or_default();
            if !what.contains("Correction") {
                continue;
            }
            if let Some(a) = field_str(&item, "crs:CorrectionActive") {
                if a == "false" || a == "False" || a == "0" {
                    continue;
                }
            }
            let amount = field(&item, "crs:CorrectionAmount")
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            let ev = field_any(&item, &["crs:LocalExposure2012", "crs:LocalExposure"])
                .unwrap_or(0.0)
                .clamp(-4.0, 4.0);
            let mut sat = field(&item, "crs:LocalSaturation").unwrap_or(0.0);
            if sat.abs() > 1.5 {
                sat /= 100.0;
            }
            let sat = sat.clamp(-1.0, 1.0);
            let temp = (field(&item, "crs:LocalTemperature").unwrap_or(0.0) / 100.0)
                .clamp(-1.0, 1.0);
            let feather_of = |mb: &str| {
                (field(mb, "crs:Feather").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0)
            };
            // LR midpoint (0..100): 0 = at the zero-effect boundary.
            let mid_of = |mb: &str| {
                (1.0 - field(mb, "crs:Midpoint").unwrap_or(50.0) / 100.0)
                    .clamp(0.01, 0.99)
            };
            let win = |kind: &str, p: [f32; 6], mid: f32, invert: bool| PowerWindow {
                kind: kind.into(),
                p,
                ev,
                sat,
                temp,
                invert,
                enabled: true,
                opacity: amount,
                link_q: false,
                mid,
            };
            // linear gradient: full effect at (FullX,FullY), zero at
            // (ZeroX,ZeroY); "Flipped" swaps the two ends.
            for mb in mask_blocks(&item, "Gradient") {
                let mut zx = field(&mb, "crs:ZeroX").unwrap_or(0.5);
                let mut zy = field(&mb, "crs:ZeroY").unwrap_or(0.5);
                let mut fx = field(&mb, "crs:FullX").unwrap_or(0.5);
                let mut fy = field(&mb, "crs:FullY").unwrap_or(0.5);
                if truthy(field_str(&mb, "crs:Flipped")) {
                    std::mem::swap(&mut zx, &mut fx);
                    std::mem::swap(&mut zy, &mut fy);
                }
                let soft = feather_of(&mb);
                let mid = mid_of(&mb);
                r.windows
                    .push(win("gradient", [fx, fy, zx, zy, 0.0, soft], mid, false));
            }
            // radial filter: ellipse bounding box + rotation; Flipped /
            // MaskInverted applies the adjustment outside the ellipse.
            for mb in mask_blocks(&item, "CircularGradient") {
                let t = field(&mb, "crs:Top").unwrap_or(0.25);
                let b = field(&mb, "crs:Bottom").unwrap_or(0.75);
                let l = field(&mb, "crs:Left").unwrap_or(0.25);
                let rt = field(&mb, "crs:Right").unwrap_or(0.75);
                let ang = field(&mb, "crs:Angle").unwrap_or(0.0);
                let flipped = truthy(field_str(&mb, "crs:Flipped"))
                    || truthy(field_str(&mb, "crs:MaskInverted"));
                let soft = feather_of(&mb);
                let mid = mid_of(&mb);
                r.windows.push(win(
                    "circle",
                    [
                        (l + rt) * 0.5,
                        (t + b) * 0.5,
                        (rt - l).abs() * 0.5,
                        (b - t).abs() * 0.5,
                        ang,
                        soft,
                    ],
                    mid,
                    flipped,
                ));
            }
            // brush: dab list + radius/flow → stroke(s) in one layer.
            let mut strokes = Vec::new();
            for mb in mask_blocks(&item, "Paint") {
                let radius = field(&mb, "crs:Radius").unwrap_or(0.05).clamp(0.005, 0.5);
                let flow = (field(&mb, "crs:Flow").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0);
                let mut pts = Vec::new();
                for dab in seq_items(&mb, "crs:Dabs") {
                    if let (Some(gt), Some(lt)) = (dab.find('>'), dab.rfind('<')) {
                        let mut it = dab[gt + 1..lt]
                            .split_whitespace()
                            .filter_map(|v| v.parse::<f32>().ok());
                        if let (Some(x), Some(y)) = (it.next(), it.next()) {
                            pts.push([x, y]);
                        }
                    }
                }
                if !pts.is_empty() {
                    strokes.push(BrushStroke {
                        pts,
                        radius,
                        soft: 0.5,
                        opacity: flow,
                        erase: false,
                    });
                }
            }
            if !strokes.is_empty() {
                r.brushes.push(BrushLayer {
                    enabled: true,
                    opacity: amount,
                    ev,
                    sat,
                    temp,
                    link_q: false,
                    edge_aware: false,
                    edge_tol: 0.5,
                    strokes,
                });
            }
        }
    }
}

/// `<crs:ToneCurvePV2012>`-style rdf:Seq of "x, y" pairs (0..255 ints) ->
/// normalized point list.
fn curve_seq(body: &str, tag: &str) -> Option<Vec<[f32; 2]>> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = body.find(&open)? + open.len();
    let j = body[i..].find(&close)? + i;
    let block = &body[i..j];
    let mut pts = Vec::new();
    let mut rest = block;
    while let Some(li) = rest.find("<rdf:li>") {
        let s = &rest[li + 8..];
        let end = s.find("</rdf:li>").unwrap_or(s.len());
        let txt = &s[..end];
        let mut it = txt.split(',').filter_map(|v| v.trim().parse::<f32>().ok());
        if let (Some(x), Some(y)) = (it.next(), it.next()) {
            pts.push([x / 255.0, y / 255.0]);
        }
        rest = &s[end.min(s.len())..];
    }
    if pts.len() >= 2 {
        pts.sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
        Some(pts)
    } else {
        None
    }
}

/// ACR PV2012 parametric tone curve: Shadows/Darks/Lights/Highlights
/// (-100..100) plus three split points (0..255, defaults 64/128/191).
/// Each slider shapes its zone through a smoothstep-integrated slope —
/// the curve stays C1-smooth and fixes the endpoints.
fn parametric_curve(body: &str) -> Option<Vec<[f32; 2]>> {
    let sh = attr(body, "crs:ParametricShadows")?;
    let dk = attr(body, "crs:ParametricDarks").unwrap_or(0.0);
    let lt = attr(body, "crs:ParametricLights").unwrap_or(0.0);
    let hi = attr(body, "crs:ParametricHighlights").unwrap_or(0.0);
    if sh == 0.0 && dk == 0.0 && lt == 0.0 && hi == 0.0 {
        return None;
    }
    let s1 = (attr(body, "crs:ParametricShadowSplit").unwrap_or(64.0) / 255.0).clamp(0.02, 0.96);
    let s2 = (attr(body, "crs:ParametricMidtoneSplit").unwrap_or(128.0) / 255.0).clamp(s1 + 0.02, 0.98);
    let s3 = (attr(body, "crs:ParametricHighlightSplit").unwrap_or(191.0) / 255.0).clamp(s2 + 0.01, 0.99);
    let g = [sh / 100.0, dk / 100.0, lt / 100.0, hi / 100.0];
    let edges = [0.0f32, s1, s2, s3, 1.0];
    // y(x) = x + sum_i g_i * sstep(t_i(x)) * zone_width * strength,
    // strength chosen so slider=+100 lifts the zone ~+25% mid-zone.
    let eval = |x: f32| -> f32 {
        let mut y = x;
        for (i, &gv) in g.iter().enumerate() {
            let (lo, hiw) = (edges[i], edges[i + 1]);
            if x > lo && hiw > lo {
                let t = ((x.min(hiw) - lo) / (hiw - lo)).clamp(0.0, 1.0);
                let w = t * t * (3.0 - 2.0 * t);
                y += gv * w * (hiw - lo) * 0.6;
            }
        }
        y.clamp(0.0, 1.0)
    };
    let mut pts = Vec::with_capacity(33);
    for i in 0..33 {
        let x = i as f32 / 32.0;
        pts.push([x, eval(x)]);
    }
    Some(pts)
}

/// crs:HueAdjustment*/SaturationAdjustment*/LuminanceAdjustment* over the
/// 8 adobe bands -> hue_hue / hue_sat / hue_lum point curves.
fn hsl_adjustments(body: &str, r: &mut crate::recipe::Recipe) {
    const BANDS: [(&str, f32); 8] = [
        ("Red", 0.0),
        ("Orange", 0.045),
        ("Yellow", 0.125),
        ("Green", 0.33),
        ("Aqua", 0.5),
        ("Blue", 0.66),
        ("Purple", 0.79),
        ("Magenta", 0.92),
    ];
    for (name, center) in BANDS {
        let hue_tag = format!("crs:HueAdjustment{name}");
        let sat_tag = format!("crs:SaturationAdjustment{name}");
        let lum_tag = format!("crs:LuminanceAdjustment{name}");
        if let Some(v) = attr(body, &hue_tag) {
            if v != 0.0 {
                // anchor neighbours so untouched hues aren't dragged along
                for (x, dy) in [(center - 0.07, 0.0), (center, v / 100.0 * 0.15), (center + 0.07, 0.0)] {
                    r.hue_hue.push([x - x.floor(), x - x.floor() + dy]);
                }
            }
        }
        if let Some(v) = attr(body, &sat_tag) {
            if v != 0.0 {
                for (x, g) in [(center - 0.07, 1.0), (center, 1.0 + v / 100.0), (center + 0.07, 1.0)] {
                    r.hue_sat.push([x - x.floor(), g.max(0.0)]);
                }
            }
        }
        if let Some(v) = attr(body, &lum_tag) {
            if v != 0.0 {
                for (x, g) in [(center - 0.07, 1.0), (center, 1.0 + v / 150.0), (center + 0.07, 1.0)] {
                    r.hue_lum.push([x - x.floor(), g.max(0.0)]);
                }
            }
        }
    }
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sl_recipe_lossless() {
        let dir = std::env::temp_dir().join(format!("xmp_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("photo.arw");
        std::fs::write(&asset, b"fake").unwrap();
        let mut sc = Sidecar::default();
        sc.recipe.exposure = 0.55;
        sc.recipe.contrast = -0.3;
        sc.recipe.windows = vec![crate::recipe::PowerWindow {
            kind: "subject".into(),
            p: [0.0; 6],
            ev: 0.8,
            sat: 0.3,
            temp: 0.1,
            invert: false,
            enabled: true,
            opacity: 1.0,
            link_q: false,
            mid: 0.0,
        }];
        sc.rating = 4;
        sc.label = "Red".into();
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:Exposure2012=\"+0.55\""));
        assert!(xml.contains("crs:Contrast2012=\"-30\""));
        assert!(xml.contains("xmp:Rating=\"4\""));
        let back = read_xmp(&asset).unwrap();
        assert!((back.recipe.exposure - 0.55).abs() < 1e-6);
        assert!((back.recipe.contrast + 0.3).abs() < 1e-6);
        assert_eq!(back.recipe.windows.len(), 1);
        assert_eq!(back.recipe.windows[0].kind, "subject");
        assert!((back.recipe.windows[0].ev - 0.8).abs() < 1e-6);
        assert_eq!(back.rating, 4);
        assert_eq!(back.label, "Red");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn foreign_xmp_maps_crs_fields() {
        let dir = std::env::temp_dir().join(format!("xmp_test_f_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("lr_edit.nef");
        std::fs::write(&asset, b"fake").unwrap();
        std::fs::write(
            xmp_path_for(&asset),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" crs:Exposure2012="+1.20" crs:Contrast2012="+15" crs:Highlights2012="-80" crs:Dehaze="+10" crs:Vibrance="+25" crs:HasCrop="true" crs:CropTop="0.1" crs:CropLeft="0.05" crs:CropBottom="0.9" crs:CropRight="0.95" crs:CropAngle="-2.5" xmp:Rating="3" xmp:Label="Blue"/></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let sc = read_xmp(&asset).unwrap();
        assert!((sc.recipe.exposure - 1.2).abs() < 1e-6);
        assert!((sc.recipe.contrast - 0.15).abs() < 1e-6);
        assert!((sc.recipe.highlights + 0.8).abs() < 1e-6);
        assert!((sc.recipe.dehaze - 0.1).abs() < 1e-6);
        assert!((sc.recipe.rotation_deg + 2.5).abs() < 1e-6);
        for (got, want) in sc.recipe.crop.iter().zip([0.05, 0.1, 0.05, 0.1]) {
            assert!((got - want).abs() < 1e-5, "crop {got} != {want}");
        }
        assert_eq!(sc.rating, 3);
        assert_eq!(sc.label, "Blue");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lr_local_corrections_map_to_windows_and_brushes() {
        let body = r##"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" crs:ProcessVersion="11.0">
 <crs:GradientBasedCorrections>
  <rdf:Seq>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalExposure2012="-1.20" crs:LocalSaturation="20" crs:LocalTemperature="-15">
    <crs:CorrectionMasks>
     <rdf:Seq>
      <rdf:li crs:What="Mask/Gradient" crs:MaskValue="1" crs:ZeroX="0.5" crs:ZeroY="0.60" crs:FullX="0.5" crs:FullY="0.20" crs:Top="0" crs:Bottom="1" crs:Left="0" crs:Right="1" crs:Angle="0" crs:Midpoint="50" crs:Feather="50" crs:Flipped="false" crs:Version="1"/>
     </rdf:Seq>
    </crs:CorrectionMasks>
   </rdf:li>
  </rdf:Seq>
 </crs:GradientBasedCorrections>
 <crs:CircularGradientBasedCorrections>
  <rdf:Seq>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="0.8" crs:CorrectionActive="true" crs:LocalExposure2012="+0.50">
    <crs:CorrectionMasks>
     <rdf:Seq>
      <rdf:li crs:What="Mask/CircularGradient" crs:MaskValue="1" crs:Top="0.3" crs:Bottom="0.7" crs:Left="0.4" crs:Right="0.8" crs:Angle="15" crs:Midpoint="25" crs:Roundness="0" crs:Feather="60" crs:Flipped="true" crs:Version="1"/>
     </rdf:Seq>
    </crs:CorrectionMasks>
   </rdf:li>
  </rdf:Seq>
 </crs:CircularGradientBasedCorrections>
 <crs:PaintBasedCorrections>
  <rdf:Seq>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalExposure2012="+0.30">
    <crs:CorrectionMasks>
     <rdf:Seq>
      <rdf:li crs:What="Mask/Paint" crs:MaskValue="1" crs:Radius="0.08" crs:Flow="80">
       <crs:Dabs><rdf:Seq><rdf:li>0.4 0.3</rdf:li><rdf:li>0.45 0.32</rdf:li><rdf:li>0.5 0.35</rdf:li></rdf:Seq></crs:Dabs>
      </rdf:li>
     </rdf:Seq>
    </crs:CorrectionMasks>
   </rdf:li>
  </rdf:Seq>
 </crs:PaintBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(body);
        assert_eq!(r.windows.len(), 2);
        let g = &r.windows[0];
        assert_eq!(g.kind, "gradient");
        // Full point at top, zero below — our window runs full→zero.
        assert!((g.p[0] - 0.5).abs() < 1e-6 && (g.p[1] - 0.2).abs() < 1e-6);
        assert!((g.p[2] - 0.5).abs() < 1e-6 && (g.p[3] - 0.6).abs() < 1e-6);
        assert!((g.ev + 1.2).abs() < 1e-6);
        assert!((g.sat - 0.2).abs() < 1e-6);
        assert!((g.temp + 0.15).abs() < 1e-6);
        assert!((g.mid - 0.5).abs() < 1e-6 && (g.p[5] - 0.5).abs() < 1e-6);
        let c = &r.windows[1];
        assert_eq!(c.kind, "circle");
        assert!((c.p[0] - 0.6).abs() < 1e-6 && (c.p[1] - 0.5).abs() < 1e-6);
        assert!((c.p[2] - 0.2).abs() < 1e-6 && (c.p[3] - 0.2).abs() < 1e-6);
        assert!((c.p[4] - 15.0).abs() < 1e-6);
        assert!(c.invert);
        assert!((c.opacity - 0.8).abs() < 1e-6);
        assert!((c.mid - 0.75).abs() < 1e-6 && (c.p[5] - 0.6).abs() < 1e-6);
        assert_eq!(r.brushes.len(), 1);
        let st = &r.brushes[0].strokes[0];
        assert_eq!(st.pts.len(), 3);
        assert!((st.pts[0][0] - 0.4).abs() < 1e-6 && (st.radius - 0.08).abs() < 1e-6);
        assert!((r.brushes[0].ev - 0.3).abs() < 1e-6);
    }

    #[test]
    fn write_xmp_emits_lr_local_corrections() {
        let dir = std::env::temp_dir().join(format!("xmp_test_w_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("img.arw");
        std::fs::write(&asset, b"fake").unwrap();
        let mut sc = Sidecar::default();
        sc.recipe.windows.push(crate::recipe::PowerWindow {
            kind: "gradient".into(),
            p: [0.5, 0.2, 0.5, 0.6, 0.0, 0.5],
            ev: -1.0,
            mid: 0.5,
            ..Default::default()
        });
        sc.recipe.windows.push(crate::recipe::PowerWindow {
            kind: "circle".into(),
            p: [0.6, 0.5, 0.2, 0.15, 10.0, 0.4],
            ev: 0.4,
            mid: 0.7,
            invert: true,
            ..Default::default()
        });
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:GradientBasedCorrections"));
        assert!(xml.contains("crs:What=\"Mask/Gradient\""));
        assert!(xml.contains("crs:FullX=\"0.500000\""));
        assert!(xml.contains("crs:CircularGradientBasedCorrections"));
        assert!(xml.contains("crs:What=\"Mask/CircularGradient\""));
        assert!(xml.contains("crs:Flipped=\"true\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
