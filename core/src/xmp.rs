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
use std::collections::HashMap;
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
    let mut look_block = None;
    a.push_str("<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
    a.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"safelight\">\n");
    a.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    a.push_str("  <rdf:Description rdf:about=\"\"\n");
    a.push_str("   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n");
    a.push_str("   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n");
    a.push_str("   xmlns:sl=\"https://safelight.app/ns/1.0/\"\n");
    a.push_str(&format!(
        "   crs:Version=\"15.0\" crs:ProcessVersion=\"{}\" crs:RawFileName=\"",
        if r.process_version > 0.0 {
            // keep the catalog's own version string (e.g. "15.4" vs "11.0")
            if r.process_version.fract() == 0.0 {
                format!("{:.1}", r.process_version)
            } else {
                r.process_version.to_string()
            }
        } else {
            "11.0".to_string()
        }
    ));
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
        // names that resolve to an enhanced .xmp profile round-trip as
        // <crs:Look> (how LR serializes the modern profile picker);
        // ordinary camera profiles stay crs:CameraProfile.
        if find_xmp_profile(&r.profile).is_some() {
            look_block = Some(format!(
                " <crs:Look>\n  <rdf:Description\n   crs:Name=\"{}\"\n   crs:Amount=\"{:.2}\"\n   crs:Stubbed=\"true\">\n  </rdf:Description>\n </crs:Look>\n",
                esc(&r.profile),
                r.profile_amount
            ));
        } else {
            a.push_str(&format!("   crs:CameraProfile=\"{}\"\n", esc(&r.profile)));
        }
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
        "   crs:Clarity2012=\"{}\" crs:Dehaze=\"{}\" crs:Texture=\"{}\"\n",
        lr(r.clarity, 100.0),
        lr(r.dehaze, 100.0),
        lr(r.texture, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Vibrance=\"{}\" crs:Saturation=\"{}\"\n",
        lr(r.vibrance, 100.0),
        lr(r.saturation, 100.0)
    ));
    a.push_str(&format!(
        "   crs:Sharpness=\"{}\" crs:SharpenEdgeMasking=\"{}\" crs:LuminanceSmoothing=\"{}\" crs:ColorNoiseReduction=\"{}\"\n",
        lr(r.sharpen, 150.0),
        lr(r.sharp_mask, 100.0),
        lr(r.noise_luma, 100.0),
        lr(r.noise_chroma, 100.0)
    ));
    if r.sharp_radius != 1.0 || r.sharp_detail > 0.0 {
        a.push_str(&format!(
            "   crs:SharpenRadius=\"{:.1}\" crs:SharpenDetail=\"{:.0}\"\n",
            r.sharp_radius,
            r.sharp_detail * 100.0
        ));
    }
    if r.key_v != 0.0 || r.key_h != 0.0 {
        a.push_str(&format!(
            "   crs:PerspectiveVertical=\"{:.0}\" crs:PerspectiveHorizontal=\"{:.0}\"\n",
            r.key_v * 250.0,
            r.key_h * 250.0
        ));
    }
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
    if r.grain_size != 0.5 || r.grain_rough != 0.5 {
        a.push_str(&format!(
            "   crs:GrainSize=\"{:.0}\" crs:GrainRoughness=\"{:.0}\"\n",
            r.grain_size * 100.0,
            r.grain_rough * 100.0
        ));
    }
    if r.defringe_pu[0] > 0.0 || r.defringe_gr[0] > 0.0 {
        a.push_str(&format!(
            "   crs:DefringePurpleAmount=\"{:.0}\" crs:DefringePurpleHueLo=\"{:.0}\" crs:DefringePurpleHueHi=\"{:.0}\" crs:DefringeGreenAmount=\"{:.0}\" crs:DefringeGreenHueLo=\"{:.0}\" crs:DefringeGreenHueHi=\"{:.0}\"\n",
            r.defringe_pu[0] * 100.0,
            ((r.defringe_pu[1] - 210.0) / 1.2).clamp(0.0, 100.0),
            ((r.defringe_pu[2] - 210.0) / 1.2).clamp(0.0, 100.0),
            r.defringe_gr[0] * 100.0,
            ((r.defringe_gr[1] - 60.0) / 1.2).clamp(0.0, 100.0),
            ((r.defringe_gr[2] - 60.0) / 1.2).clamp(0.0, 100.0)
        ));
    }
    if r.vig_mid >= 0.0 || r.vig_round > -2.0 || r.vig_feather >= 0.0 {
        a.push_str(&format!(
            "   crs:PostCropVignetteMidpoint=\"{:.0}\" crs:PostCropVignetteRoundness=\"{:.0}\" crs:PostCropVignetteFeather=\"{:.0}\"\n",
            if r.vig_mid >= 0.0 { r.vig_mid } else { 0.5 } * 100.0,
            if r.vig_round > -2.0 { r.vig_round } else { 0.0 } * 100.0,
            if r.vig_feather >= 0.0 { r.vig_feather } else { 0.5 } * 100.0
        ));
    }
    if r.calib.iter().any(|&v| v != 0.0) {
        a.push_str(&format!(
            "   crs:ShadowTint=\"{:.0}\" crs:RedHue=\"{:.0}\" crs:RedSaturation=\"{:.0}\" crs:GreenHue=\"{:.0}\" crs:GreenSaturation=\"{:.0}\" crs:BlueHue=\"{:.0}\" crs:BlueSaturation=\"{:.0}\"\n",
            r.calib[0] * 100.0,
            r.calib[1] * 100.0,
            r.calib[2] * 100.0,
            r.calib[3] * 100.0,
            r.calib[4] * 100.0,
            r.calib[5] * 100.0,
            r.calib[6] * 100.0
        ));
    }
    if r.shadow_sat > 0.0 || r.highlight_sat > 0.0 || r.midtone_sat > 0.0 {
        a.push_str(&format!(
            "   crs:ColorGradeShadowHue=\"{:.0}\" crs:ColorGradeShadowSat=\"{:.0}\" crs:ColorGradeMidtoneHue=\"{:.0}\" crs:ColorGradeMidtoneSat=\"{:.0}\" crs:ColorGradeHighlightHue=\"{:.0}\" crs:ColorGradeHighlightSat=\"{:.0}\" crs:SplitToningBalance=\"{:.0}\" crs:ColorGradeBlending=\"{:.0}\"\n",
            r.shadow_hue * 360.0,
            r.shadow_sat * 100.0,
            r.midtone_hue * 360.0,
            r.midtone_sat * 100.0,
            r.highlight_hue * 360.0,
            r.highlight_sat * 100.0,
            r.grade_balance * 100.0,
            r.grade_blend * 100.0
        ));
    }
    if r.hdr_edit {
        a.push_str(&format!(
            "   crs:HDREditMode=\"1\" crs:HDRMaxValue=\"{:.0}\" crs:SDRBlend=\"{:.0}\" crs:SDRBrightness=\"{:.0}\" crs:SDRContrast=\"{:.0}\" crs:SDRHighlights=\"{:.0}\" crs:SDRShadows=\"{:.0}\" crs:SDRWhites=\"{:.0}\" crs:SDRClarity=\"{:.0}\"\n",
            r.hdr_max,
            r.sdr[0] * 100.0,
            r.sdr[1] * 100.0,
            r.sdr[2] * 100.0,
            r.sdr[3] * 100.0,
            r.sdr[4] * 100.0,
            r.sdr[5] * 100.0,
            r.sdr[6] * 100.0
        ));
    }
    if sc.rating != 0 {
        a.push_str(&format!("   xmp:Rating=\"{}\"\n", sc.rating.clamp(-1, 5)));
    }
    if !sc.label.is_empty() {
        a.push_str(&format!("   xmp:Label=\"{}\"\n", esc(&sc.label)));
    }
    // passthrough: foreign attrs harvested on import (Upright, RetouchInfo,
    // RedEyeInfo, xmpMM:* …) — skip any key we already emitted above
    for (k, v) in &sc.extras {
        let needle = format!("{k}=\"");
        if !a.contains(&needle) {
            a.push_str(&format!("   {k}=\"{}\"\n", esc(v)));
        }
    }
    a.push_str("   ara:Format=\"1\">\n");
    if let Some(lb) = look_block {
        a.push_str(&lb);
    }
    // Lens Blur (LR12+): child element, not an attribute
    if r.lens_blur > 0.001 {
        let o = r.lens_blur_opts;
        let o2 = r.lens_blur_opts2;
        a.push_str(&format!(
            "   <crs:LensBlur crs:Version=\"1\" crs:Active=\"true\" crs:FocalRange=\"0 0 100 100\" crs:BlurAmount=\"{:.0}\" crs:BokehShape=\"{:.0}\" crs:BokehShapeDetail=\"{:.0}\" crs:HighlightsThreshold=\"{:.0}\" crs:HighlightsBoost=\"{:.0}\" crs:CatEyeAmount=\"{:.0}\" crs:CatEyeScale=\"{:.0}\" crs:BokehAspect=\"{:.0}\" crs:BokehRotation=\"{:.0}\" crs:SphericalAberration=\"{:.0}\"/>\n",
            r.lens_blur * 100.0,
            o[3],
            o2[3] * 100.0,
            o[1] * 100.0,
            o[0] * 100.0,
            o2[0] * 100.0,
            o2[1] * 100.0,
            o[2] * 100.0,
            o2[2] * 100.0,
            o[4] * 100.0
        ));
    }
    // LR-readable local corrections: gradient/circle windows -> the legacy
    // correction lists LR still reads on import.
    let local_attrs = |w: &crate::recipe::PowerWindow| {
        let mut s = format!(
            "crs:LocalExposure2012=\"{:+.2}\" crs:LocalSaturation=\"{:.2}\" crs:LocalTemperature=\"{:.0}\"",
            w.ev,
            w.sat * 100.0,
            w.temp * 100.0
        );
        if w.contrast != 0.0 {
            s += &format!(" crs:LocalContrast2012=\"{:.0}\"", w.contrast * 100.0);
        }
        if w.clarity != 0.0 {
            s += &format!(" crs:LocalClarity2012=\"{:.0}\"", w.clarity * 100.0);
        }
        if w.dehaze != 0.0 {
            s += &format!(" crs:LocalDehaze=\"{:.0}\"", w.dehaze * 100.0);
        }
        if w.tint != 0.0 {
            s += &format!(" crs:LocalTint=\"{:.0}\"", w.tint * 100.0);
        }
        for (v, name) in [
            (w.hi, "crs:LocalHighlights2012"),
            (w.sh, "crs:LocalShadows2012"),
            (w.wh, "crs:LocalWhites2012"),
            (w.bl, "crs:LocalBlacks2012"),
            (w.texture, "crs:LocalTexture2012"),
        ] {
            if v != 0.0 {
                s += &format!(" {name}=\"{:.0}\"", v * 100.0);
            }
        }
        s
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
    if !r.clones.is_empty() {
        a.push_str("   <crs:RetouchAreas><rdf:Seq>\n");
        for c in r.clones.iter().take(8) {
            a.push_str(&format!(
                "    <rdf:li crs:What=\"Retouch\" crs:Method=\"clone\" crs:SpotX=\"{:.6}\" crs:SpotY=\"{:.6}\" crs:SpotDiam=\"{:.6}\" crs:SourceX=\"{:.6}\" crs:SourceY=\"{:.6}\" crs:MidValue=\"0.5\" crs:FeatherValue=\"0.5\" crs:AspectValue=\"0\" crs:OffsetX=\"0\" crs:OffsetY=\"0\"/>\n",
                c[2],
                c[3],
                c[4] * 2.0,
                c[0],
                c[1]
            ));
        }
        a.push_str("   </rdf:Seq></crs:RetouchAreas>\n");
    }
    if !r.point_colors.is_empty() {
        a.push_str("   <crs:PointColors><rdf:Seq>\n");
        for v in &r.point_colors {
            let nums: Vec<String> = v.iter().map(|f| format!("{f:.6}")).collect();
            a.push_str(&format!("    <rdf:li>{}</rdf:li>\n", nums.join(", ")));
        }
        a.push_str("   </rdf:Seq></crs:PointColors>\n");
    }
    if !r.point_color_vars.is_empty() {
        a.push_str("   <crs:ColorVariance><rdf:Seq>\n");
        for v in &r.point_color_vars {
            a.push_str(&format!("    <rdf:li>{v:.6}</rdf:li>\n"));
        }
        a.push_str("   </rdf:Seq></crs:ColorVariance>\n");
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
        let (marker, len) = (
            bytes[p + 1],
            u16::from_be_bytes([bytes[p + 2], bytes[p + 3]]) as usize,
        );
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

/// crs:* keys that crs_to_recipe models — extras_from_xmp skips these so a
/// re-edited recipe can't be overridden by a stale passthrough value.
const MODELED_CRS: &[&str] = &[
    "crs:AutoLateralCA",
    "crs:Blacks",
    "crs:Blacks2012",
    "crs:BlueHue",
    "crs:BlueSaturation",
    "crs:CameraProfile",
    "crs:Clarity",
    "crs:Clarity2012",
    "crs:ColorGradeBlending",
    "crs:ColorGradeGlobalHue",
    "crs:ColorGradeGlobalLum",
    "crs:ColorGradeGlobalSat",
    "crs:ColorGradeHighlightHue",
    "crs:ColorGradeHighlightLum",
    "crs:ColorGradeHighlightSat",
    "crs:ColorGradeMidtoneHue",
    "crs:ColorGradeMidtoneLum",
    "crs:ColorGradeMidtoneSat",
    "crs:ColorGradeShadowHue",
    "crs:ColorGradeShadowLum",
    "crs:ColorGradeShadowSat",
    "crs:ColorNoiseReduction",
    "crs:Contrast",
    "crs:Contrast2012",
    "crs:ConvertToGrayscale",
    "crs:CropAngle",
    "crs:CropBottom",
    "crs:CropLeft",
    "crs:CropRight",
    "crs:CropTop",
    "crs:DefringeGreenAmount",
    "crs:DefringeGreenHueHi",
    "crs:DefringeGreenHueLo",
    "crs:DefringePurpleAmount",
    "crs:DefringePurpleHueHi",
    "crs:DefringePurpleHueLo",
    "crs:Dehaze",
    "crs:Exposure",
    "crs:Exposure2012",
    "crs:GrainAmount",
    "crs:GrainFrequency",
    "crs:GrainRoughness",
    "crs:GrainSize",
    "crs:GreenHue",
    "crs:GreenSaturation",
    "crs:HDREditMode",
    "crs:HDRMaxValue",
    "crs:HasCrop",
    "crs:Highlight2012",
    "crs:Highlights2012",
    "crs:IncrementalTemperature",
    "crs:IncrementalTint",
    "crs:LensProfileEnable",
    "crs:LensProfileName",
    "crs:LookTable",
    "crs:LuminanceSmoothing",
    "crs:ParametricDarks",
    "crs:ParametricHighlightSplit",
    "crs:ParametricHighlights",
    "crs:ParametricLights",
    "crs:ParametricMidtoneSplit",
    "crs:ParametricShadowSplit",
    "crs:ParametricShadows",
    "crs:PerspectiveHorizontal",
    "crs:PerspectiveRotate",
    "crs:PerspectiveScale",
    "crs:PerspectiveVertical",
    "crs:PointColors",
    "crs:PostCropVignetteAmount",
    "crs:PostCropVignetteFeather",
    "crs:PostCropVignetteMidpoint",
    "crs:PostCropVignetteRoundness",
    "crs:ProcessVersion",
    "crs:RGBTable",
    "crs:RGBTableAmount",
    "crs:RedHue",
    "crs:RedSaturation",
    "crs:SDRBlend",
    "crs:SDRBrightness",
    "crs:SDRClarity",
    "crs:SDRContrast",
    "crs:SDRHighlights",
    "crs:SDRShadows",
    "crs:SDRWhites",
    "crs:Saturation",
    "crs:Saturation2012",
    "crs:Shadow2012",
    "crs:ShadowTint",
    "crs:Shadows2012",
    "crs:SharpenDetail",
    "crs:SharpenEdgeMasking",
    "crs:SharpenRadius",
    "crs:Sharpness",
    "crs:SplitToningBalance",
    "crs:SplitToningHighlightHue",
    "crs:SplitToningHighlightSaturation",
    "crs:SplitToningShadowHue",
    "crs:SplitToningShadowSaturation",
    "crs:Temperature",
    "crs:Texture",
    "crs:Tint",
    "crs:ToneCurveName",
    "crs:ToneCurveName2012",
    "crs:Vibrance",
    "crs:Vibrance2012",
    "crs:WhiteBalance",
    "crs:Whites",
    "crs:Whites2012",
];

/// modeled key families generated from band names — matched by prefix
const MODELED_CRS_PREFIX: &[&str] = &[
    "crs:HueAdjustment",
    "crs:SaturationAdjustment",
    "crs:LuminanceAdjustment",
    "crs:GrayMixer",
];

/// Harvest `ns:key="value"` attributes from the first rdf:Description open
/// tag that we don't model — returned as extras so they re-emit verbatim
/// (Upright*, RetouchInfo, RedEyeInfo, xmpMM:*, etc.).
pub fn extras_from_xmp(body: &str) -> Vec<(String, String)> {
    let Some(i) = body.find("<rdf:Description") else {
        return Vec::new();
    };
    let rest = &body[i..];
    let end = rest.find('>').unwrap_or(rest.len());
    let tag = &rest[..end];
    let mut out = Vec::new();
    let mut p = 0;
    while let Some(e) = tag[p..].find('=') {
        let eq = p + e;
        let mut ks = eq;
        let b = tag.as_bytes();
        while ks > 0
            && (b[ks - 1].is_ascii_alphanumeric()
                || b[ks - 1] == b':'
                || b[ks - 1] == b'_'
                || b[ks - 1] == b'-')
        {
            ks -= 1;
        }
        let key = &tag[ks..eq];
        let Some(&q) = b.get(eq + 1) else { break };
        if q != b'"' && q != b'\'' {
            p = eq + 1;
            continue;
        }
        let ve = tag[eq + 2..]
            .find(q as char)
            .map(|j| eq + 2 + j)
            .unwrap_or(tag.len());
        let val = &tag[eq + 2..ve];
        let modeled = MODELED_CRS.contains(&key)
            || MODELED_CRS_PREFIX.iter().any(|p| key.starts_with(p))
            || key.starts_with("rdf:")
            || key.starts_with("xmlns")
            || key.starts_with("ara:")
            || key == "xmp:Rating"
            || key == "xmp:Label"
            || !key.contains(':');
        if !modeled && !key.is_empty() {
            out.push((key.to_string(), unescape(val)));
        }
        p = ve + 1;
    }
    out
}

fn parse_xmp_body(body: &str) -> Sidecar {
    let mut sc = Sidecar::default();
    sc.extras = extras_from_xmp(body);
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
    // dc:subject <rdf:Bag><rdf:li>…</rdf:li></rdf:Bag> — LR keywords
    if let Some(i) = body.find("<dc:subject") {
        let end = body[i..]
            .find("</dc:subject>")
            .map(|j| i + j)
            .unwrap_or(body.len());
        let seg = &body[i..end];
        let mut p = 0;
        while let Some(s) = seg[p..].find("<rdf:li") {
            let s = p + s;
            let Some(gt) = seg[s..].find('>') else { break };
            let Some(e) = seg[s..].find("</rdf:li>") else {
                break;
            };
            let k = unescape(seg[s + gt + 1..s + e].trim());
            if !k.is_empty() {
                sc.keywords.push(k);
            }
            p = s + e + 9;
        }
    }
    sc
}

pub fn read_xmp(asset: &Path) -> Option<Sidecar> {
    if let Some(p) = find_xmp(asset) {
        return std::fs::read_to_string(p).ok().map(|b| parse_xmp_body(&b));
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
        if let Some(v) = attr(&body, "crs:ProcessVersion") {
            r.process_version = v;
        }
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
        // <crs:Look><rdf:Description crs:Name="Adobe Color" …/> —
        // the modern profile picker (enhanced .xmp profiles live under
        // Settings/**/Profiles). When both are present the Look name is
        // the *effective* profile: LR keeps crs:CameraProfile pointing at
        // the base DCP the look sits on. If the name fails to resolve to
        // an enhanced profile, the DCP path still finds it (or we fall
        // back to CameraProfile below).
        let look_span = body.find("<crs:Look>").map(|ls| {
            let le = body[ls..]
                .find("</crs:Look>")
                .map(|e| ls + e + "</crs:Look>".len())
                .unwrap_or(body.len());
            (ls, le)
        });
        if let Some((ls, le)) = look_span {
            if let Some(n) = attr_str(&body[ls..le], "crs:Name") {
                if r.profile.is_empty() || find_xmp_profile(&n).is_some() {
                    r.profile = n;
                }
            }
            // the Look's own Amount is LR's profile-strength slider
            if let Some(a) = attr(&body[ls..le], "crs:Amount") {
                r.profile_amount = a.clamp(0.0, 1.0);
            }
        }
        // enhanced/Look profiles embed their grade as encoded tables:
        // crs:LookTable → dng_look_table (HSV map, applied where a DCP's
        // ProfileLookTable goes — replaces it); crs:RGBTable →
        // dng_rgb_table (RGB LUT applied near the end like a .cube LUT).
        // crs:Table_<digest> carries the base85+zlib payload.
        for (key_attr, is_rgb) in [("crs:LookTable", false), ("crs:RGBTable", true)] {
            let Some(id) = attr_str(&body, key_attr) else {
                continue;
            };
            let key = format!("crs:Table_{id}");
            let Some(payload) = attr_str(&body, &key) else {
                continue;
            };
            match crate::rgbtable::decode(&payload) {
                Some(crate::rgbtable::Table::Look(map)) if !is_rgb => {
                    r.look_table = Some(map);
                }
                Some(crate::rgbtable::Table::Rgb(t)) => {
                    r.rgb_lut = Some(crate::rgbtable::to_cube(&t, t.divisions as usize));
                }
                _ => {}
            }
        }
        // presets can dial the embedded RGB table down (0..1)
        if let Some(a) = attr(&body, "crs:RGBTableAmount") {
            r.lut_amount = a.clamp(0.0, 1.0);
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
            r.texture = v / 100.0;
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
        if let Some(v) = attr(&body, "crs:SharpenEdgeMasking") {
            r.sharp_mask = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:SharpenRadius") {
            r.sharp_radius = v.clamp(1.0, 3.0);
        }
        if let Some(v) = attr(&body, "crs:SharpenDetail") {
            r.sharp_detail = (v / 100.0).clamp(0.0, 1.0);
        }
        // PV2012 color grading luminance + global wheel -> HDR zone wheels
        if let Some(v) = attr(&body, "crs:ColorGradeShadowLum") {
            r.z_dark[2] += v / 100.0 * 0.5;
        }
        if let Some(v) = attr(&body, "crs:ColorGradeMidtoneLum") {
            r.z_shadow[2] += v / 400.0;
            r.z_light[2] += v / 400.0;
        }
        if let Some(v) = attr(&body, "crs:ColorGradeHighlightLum") {
            r.z_light[2] += v / 100.0 * 0.5;
        }
        if let Some(v) = attr(&body, "crs:ColorGradeGlobalLum") {
            r.z_global[2] += v / 100.0 * 0.5;
        }
        if let Some(v) = attr(&body, "crs:ColorGradeGlobalHue") {
            r.z_global[0] = (v / 360.0).rem_euclid(1.0);
        }
        if let Some(v) = attr(&body, "crs:ColorGradeGlobalSat") {
            r.z_global[1] += (v / 100.0 * 0.5).clamp(0.0, 1.0);
        }
        // transform panel: keystone sliders
        if let Some(v) = attr(&body, "crs:PerspectiveVertical") {
            r.key_v = (v / 100.0 * 0.4).clamp(-0.4, 0.4);
        }
        if let Some(v) = attr(&body, "crs:PerspectiveHorizontal") {
            r.key_h = (v / 100.0 * 0.4).clamp(-0.4, 0.4);
        }
        // transform-panel rotate adds on top of the crop-tool angle
        if let Some(v) = attr(&body, "crs:PerspectiveRotate") {
            r.rotation_deg += v.clamp(-45.0, 45.0);
        }
        // transform-panel scale (>100): post-transform zoom → symmetric crop
        if let Some(v) = attr(&body, "crs:PerspectiveScale") {
            if v > 100.5 && r.crop == [0.0; 4] {
                let m = ((1.0 - 100.0 / v) * 0.5).clamp(0.0, 0.45);
                r.crop = [m, m, m, m];
            }
        }
        // spot removal -> clone stamps
        for item in seq_items(&body, "crs:RetouchAreas") {
            let what = field_str(&item, "crs:What").unwrap_or_default();
            if !what.contains("Retouch") {
                continue;
            }
            // LR PatchMatch remove: pm_* attrs (emitted by the catalog
            // importer) carry the offset-map digest + sensor-px rects —
            // the blob bitmap supersedes the clone approximation.
            if let Some(dig) = field_str(&item, "crs:PmPatch") {
                let f4 = |name: &str| -> Option<[f32; 4]> {
                    let v: Vec<f32> = field_str(&item, name)?
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
                };
                let f2 = |name: &str| -> Option<[f32; 2]> {
                    let v: Vec<f32> = field_str(&item, name)?
                        .split(',')
                        .filter_map(|s| s.trim().parse().ok())
                        .collect();
                    (v.len() == 2).then(|| [v[0], v[1]])
                };
                r.pm_spots.push(crate::recipe::PmSpot {
                    target: f4("crs:PmTarget").unwrap_or_default(),
                    search: f4("crs:PmSearch").unwrap_or_default(),
                    frame_org: f2("crs:PmFrame").unwrap_or_default(),
                    ellipse: f4("crs:PmEllipse").unwrap_or_default(),
                    opacity: field(&item, "crs:PmOpacity").unwrap_or(1.0),
                    digest: dig,
                    ..Default::default()
                });
                continue;
            }
            let sx = field(&item, "crs:SourceX").unwrap_or(0.5);
            let sy = field(&item, "crs:SourceY").unwrap_or(0.5);
            let dx = field(&item, "crs:SpotX").unwrap_or(0.5);
            let dy = field(&item, "crs:SpotY").unwrap_or(0.5);
            let diam = field(&item, "crs:SpotDiam").unwrap_or(0.1);
            r.clones
                .push([sx, sy, dx, dy, (diam * 0.5).clamp(0.002, 0.5), 0.0]);
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
        if let Some(v) = attr(&body, "crs:PostCropVignetteMidpoint") {
            r.vig_mid = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:PostCropVignetteRoundness") {
            r.vig_round = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:PostCropVignetteFeather") {
            r.vig_feather = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:GrainAmount") {
            r.grain = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:GrainSize") {
            r.grain_size = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr_any(&body, &["crs:GrainRoughness", "crs:GrainFrequency"]) {
            r.grain_rough = (v / 100.0).clamp(0.0, 1.0);
        }
        // defringe: LR stores hue ranges on a 0..100 subscale around the
        // band centre — purple 210..330°, green 60..180°
        if let Some(v) = attr(&body, "crs:DefringePurpleAmount") {
            r.defringe_pu[0] = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:DefringePurpleHueLo") {
            r.defringe_pu[1] = 210.0 + v * 1.2;
        }
        if let Some(v) = attr(&body, "crs:DefringePurpleHueHi") {
            r.defringe_pu[2] = 210.0 + v * 1.2;
        }
        if let Some(v) = attr(&body, "crs:DefringeGreenAmount") {
            r.defringe_gr[0] = (v / 100.0).clamp(0.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:DefringeGreenHueLo") {
            r.defringe_gr[1] = 60.0 + v * 1.2;
        }
        if let Some(v) = attr(&body, "crs:DefringeGreenHueHi") {
            r.defringe_gr[2] = 60.0 + v * 1.2;
        }
        // camera calibration panel: ShadowTint + per-primary hue/sat
        // (all -100..100 → -1..1)
        let cal = |names: &[&str]| (attr_any(&body, names).unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0);
        r.calib = [
            cal(&["crs:ShadowTint"]),
            cal(&["crs:RedHue"]),
            cal(&["crs:RedSaturation"]),
            cal(&["crs:GreenHue"]),
            cal(&["crs:GreenSaturation"]),
            cal(&["crs:BlueHue"]),
            cal(&["crs:BlueSaturation"]),
        ];
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
        // a preset can pin a specific lens profile (LR writes e.g.
        // "Adobe (SIGMA 45mm F2.8 DG DN C019, Sony)") — takes priority
        // over the EXIF lens when matching the lens db.
        if let Some(n) = attr_str(&body, "crs:LensProfileName") {
            if let Some(l) = n.strip_prefix("Adobe (").and_then(|s| s.strip_suffix(')')) {
                // "lens, make" — keep just the lens model
                r.lens_name = l
                    .rsplit_once(',')
                    .map(|(l, _)| l.trim().to_string())
                    .unwrap_or_else(|| l.trim().to_string());
            } else {
                r.lens_name = n;
            }
        }
        if let Some(v) = attr(&body, "crs:AutoLateralCA") {
            r.ca_fix = if v >= 0.5 { 0.3 } else { 0.0 };
        }
        // Lens Blur (LR12+): <crs:LensBlur crs:Active=.. crs:BlurAmount=..>
        // depth-based defocus — we blend by (1 - subject matte) instead.
        if let Some(ls) = body.find("<crs:LensBlur") {
            let le = body[ls..].find('>').unwrap_or(0);
            let lb = &body[ls..ls + le];
            if attr_str(lb, "crs:Active").as_deref() != Some("false") {
                r.lens_blur = (attr(lb, "crs:BlurAmount").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
                // bokeh options: [highlight_boost, highlight_thresh, aspect,
                // shape(0=disc,1=ring), spherical_aberration]
                r.lens_blur_opts = [
                    (attr(lb, "crs:HighlightsBoost").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0),
                    (attr(lb, "crs:HighlightsThreshold").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0),
                    (attr(lb, "crs:BokehAspect").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
                    if attr(lb, "crs:BokehShape").unwrap_or(0.0) >= 1.0 {
                        1.0
                    } else {
                        0.0
                    },
                    (attr(lb, "crs:SphericalAberration").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
                ];
                r.lens_blur_opts2 = [
                    (attr(lb, "crs:CatEyeAmount").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0),
                    (attr(lb, "crs:CatEyeScale").unwrap_or(100.0) / 100.0).clamp(0.0, 1.0),
                    (attr(lb, "crs:BokehRotation").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0),
                    (attr(lb, "crs:BokehShapeDetail").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0),
                ];
            }
        }
        // HDR edit mode (LR13+): HDREditMode=1 edits were authored in HDR
        // space; SDRBlend/SDR* drive the SDR rendition. Our SDR render is
        // that rendition — preserve the fields so they round-trip.
        if attr(&body, "crs:HDREditMode").unwrap_or(0.0) >= 1.0 {
            r.hdr_edit = true;
        }
        if let Some(v) = attr(&body, "crs:HDRMaxValue") {
            r.hdr_max = v;
        }
        r.sdr = [
            (attr(&body, "crs:SDRBlend").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0),
            (attr(&body, "crs:SDRBrightness").unwrap_or(0.0) / 100.0).clamp(0.0, 1.0),
            (attr(&body, "crs:SDRContrast").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            (attr(&body, "crs:SDRHighlights").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            (attr(&body, "crs:SDRShadows").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            (attr(&body, "crs:SDRWhites").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
            (attr(&body, "crs:SDRClarity").unwrap_or(0.0) / 100.0).clamp(-1.0, 1.0),
        ];
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
        if let Some(v) = attr(&body, "crs:SplitToningBalance") {
            r.grade_balance = (v / 100.0).clamp(-1.0, 1.0);
        }
        if let Some(v) = attr(&body, "crs:ColorGradeBlending") {
            r.grade_blend = (v / 100.0).clamp(0.0, 1.0);
        }
        // PV2012 parametric tone curve -> sampled into our point curve
        if let Some(curve_pts) = parametric_curve(&body) {
            r.curve = curve_pts;
        }
        // explicit point curves (rdf:Seq of "x, y" strings)
        if let Some(pts) = curve_seq(&body, "crs:ToneCurvePV2012") {
            r.curve = pts;
        }
        // named tone curve when no custom points were given ("Linear"
        // must defeat our default medium-contrast base curve)
        if r.curve.is_empty() {
            if let Some(name) = attr_str(&body, "crs:ToneCurveName2012")
                .or_else(|| attr_str(&body, "crs:ToneCurveName"))
            {
                r.curve = match name.as_str() {
                    "Linear" => vec![[0.0, 0.0], [1.0, 1.0]],
                    "Medium Contrast" => vec![
                        [0.0, 0.0],
                        [0.125, 0.086],
                        [0.25, 0.22],
                        [0.5, 0.5],
                        [0.75, 0.78],
                        [0.878, 0.902],
                        [1.0, 1.0],
                    ],
                    "Strong Contrast" => vec![
                        [0.0, 0.0],
                        [0.125, 0.05],
                        [0.25, 0.15],
                        [0.5, 0.5],
                        [0.75, 0.85],
                        [0.875, 0.945],
                        [1.0, 1.0],
                    ],
                    _ => Vec::new(),
                };
            }
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
        // Point Color swatches: <crs:PointColors><rdf:Seq><rdf:li>19 floats
        for item in seq_items(&body, "crs:PointColors") {
            // li_blocks returns the whole element — keep only its text body
            let inner = match (item.find('>'), item.rfind('<')) {
                (Some(a), Some(b)) if b > a => &item[a + 1..b],
                _ => &item[..],
            };
            let mut row = [0.0f32; 19];
            let mut n = 0usize;
            for t in inner.split([',', ' ', '\n', '\r', '\t']) {
                let t = t.trim();
                if t.is_empty() {
                    continue;
                }
                if n >= 19 {
                    break;
                }
                if let Ok(f) = t.parse::<f32>() {
                    row[n] = f;
                    n += 1;
                }
            }
            if n == 19 {
                r.point_colors.push(row);
            }
        }
        // per-swatch Color Variance (-100..100 UI → -1..1), index-aligned
        for li in seq_items(&body, "crs:ColorVariance") {
            if let Some(v) = li_floats(&li).and_then(|v| v.into_iter().next()) {
                r.point_color_vars
                    .push(if v.abs() > 1.5 { v / 100.0 } else { v });
            }
        }
        if r.point_color_vars.is_empty() {
            if let Some(v) = field(&body, "crs:ColorVariance") {
                r.point_color_vars
                    .push(if v.abs() > 1.5 { v / 100.0 } else { v });
            }
        }
        // flat-attribute form: crs:PointColors="f, f, ..." (one swatch)
        if r.point_colors.is_empty() {
            if let Some(s) = attr_str(&body, "crs:PointColors") {
                let mut row = [0.0f32; 19];
                let mut n = 0usize;
                for t in s.split(',') {
                    if n >= 19 {
                        break;
                    }
                    if let Ok(f) = t.trim().parse::<f32>() {
                        row[n] = f;
                        n += 1;
                    }
                }
                if n == 19 {
                    r.point_colors.push(row);
                }
            }
        }
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
        // preset strength (LR Amount slider): scale all adjustment deltas
        // toward their defaults; geometry/masks/WB are untouched inside.
        // The crs:Amount inside a nested <crs:Look> is the *profile's*
        // amount, not preset strength — exclude that block.
        let amt_body: std::borrow::Cow<str> = match look_span {
            Some((ls, le)) => format!("{}{}", &body[..ls], &body[le..]).into(),
            None => body.into(),
        };
        if let Some(a) = attr(&amt_body, "crs:Amount") {
            r.scale(a.clamp(0.0, 2.0));
        }
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
    let bytes = seq.as_bytes();
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"</rdf:li>") {
            depth -= 1;
            i += 9;
            if depth == 0 {
                items.push(seq[start..i].to_string());
            }
        } else if rest.starts_with(b"<rdf:li") {
            let gt = rest
                .iter()
                .position(|&b| b == b'>')
                .map(|x| x + i)
                .unwrap_or(seq.len() - 1);
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
/// depth-aware, so the whole element body can be scanned directly. LR
/// catalog-derived XMP can carry the same element twice (duplicate keys in
/// the Lua settings blob), so every occurrence is scanned.
fn seq_items(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = body[from..].find(&open) {
        let ti = from + rel;
        let Some(gt) = body[ti..].find('>') else {
            break;
        };
        let si = ti + gt + 1;
        let Some(rel_e) = body[si..].find(&close) else {
            break;
        };
        out.extend(li_blocks(&body[si..si + rel_e]));
        from = si + rel_e + close.len();
    }
    out
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

/// LR person-part mask names (MaskSubType=3) → soft-ellipse prior over a
/// typically-framed portrait subject: (cx, cy, rx, ry) in normalized image
/// coords. We have no per-part segmentation model, so the subject matte is
/// gated by this region — approximate, but lands the adjustment on the
/// right end of the person instead of dropping it entirely.
fn part_prior(name: &str) -> (f32, f32, f32, f32) {
    match name {
        "Iris and Pupil" | "Eye Sclera" | "Eyebrows" => (0.5, 0.28, 0.10, 0.07),
        "Lips" | "Teeth" | "Facial Hair" => (0.5, 0.36, 0.09, 0.07),
        "Face Skin" | "Facial Skin" => (0.5, 0.28, 0.13, 0.15),
        "Hair" => (0.5, 0.14, 0.16, 0.12),
        "Body Skin" => (0.5, 0.55, 0.25, 0.40),
        "Clothes" => (0.5, 0.72, 0.22, 0.26),
        _ => (0.5, 0.30, 0.15, 0.30),
    }
}

/// LR landscape semantic mask names (MaskSubType=0, non-sky/non-veg) →
/// soft-ellipse region prior over a typical landscape composition.
/// Combined with (1 - sky matte) as the gate.
fn land_prior(name: &str) -> (f32, f32, f32, f32) {
    match name {
        "Natural Ground" | "Artificial Ground" => (0.5, 0.78, 0.65, 0.30),
        "Water" => (0.5, 0.72, 0.60, 0.30),
        "Mountains" => (0.5, 0.40, 0.65, 0.35),
        "Architecture" => (0.5, 0.55, 0.70, 0.45),
        _ => (0.5, 0.60, 0.70, 0.40),
    }
}

/// LR local corrections (graduated filter, radial filter, adjustment
/// brush) → our power windows and brush layers. Handles both the legacy
/// per-kind lists and the unified LR11+ MaskGroupBasedCorrections.
/// Locate an enhanced profile .xmp by display name ("Adobe Landscape",
/// "Artistic 01", …). Searches the profile dirs (LR's Settings tree and
/// ~/.safelight/profiles) for a matching filename stem, preferring paths
/// under a "Profiles" folder.
pub fn find_xmp_profile(name: &str) -> Option<std::path::PathBuf> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<std::path::PathBuf>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let norm = name.trim().to_lowercase();
    if norm.is_empty() {
        return None;
    }
    if let Ok(c) = cache.lock() {
        if let Some(hit) = c.get(&norm) {
            return hit.clone();
        }
    }
    let found = find_xmp_profile_uncached(&norm);
    if let Ok(mut c) = cache.lock() {
        if c.len() > 256 {
            c.clear();
        }
        c.insert(norm, found.clone());
    }
    found
}

fn find_xmp_profile_uncached(norm: &str) -> Option<std::path::PathBuf> {
    let mut fallback = None;
    for dir in crate::dcp::profile_dirs() {
        let mut stack = vec![dir];
        let mut depth_guard = 0usize;
        while let Some(d) = stack.pop() {
            depth_guard += 1;
            if depth_guard > 4000 {
                break;
            }
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    // bound the walk: LR's Settings tree is shallow
                    if p.components().count() <= d.components().count() + 6 {
                        stack.push(p);
                    }
                    continue;
                }
                if p.extension().and_then(|s| s.to_str()) != Some("xmp") {
                    continue;
                }
                let stem = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if stem == *norm {
                    if p.to_string_lossy().to_lowercase().contains("profile") {
                        return Some(p);
                    }
                    fallback.get_or_insert(p);
                }
            }
        }
    }
    fallback
}

/// Load an enhanced profile .xmp as a Recipe + its crs:Amount strength
/// (the profile "Amount" slider, default 1).
pub fn load_xmp_profile(name: &str) -> Option<(Recipe, f32)> {
    let path = find_xmp_profile(name)?;
    let body = std::fs::read_to_string(&path).ok()?;
    let r = crs_to_recipe(&body);
    let amt = body
        .find("<crs:Look>")
        .and_then(|i| {
            let e = body[i..]
                .find("</crs:Look>")
                .map(|x| i + x)
                .unwrap_or(body.len());
            attr(&body[i..e], "crs:Amount")
        })
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    Some((r, amt))
}

/// Merge a profile .xmp's baked-in grade UNDER the user's recipe: the
/// profile's sliders/curves/tables apply first, scaled by `amt` (the LR
/// profile Amount slider). Fields the user already set are composed
/// additively — matching LR's "profile then adjustments" order.
pub fn merge_profile_under(dst: &mut Recipe, pr: &Recipe, amt: f32) {
    if amt <= 0.001 {
        return;
    }
    let s = |v: &mut f32, d: f32| *v += d * amt;
    s(&mut dst.exposure, pr.exposure);
    s(&mut dst.contrast, pr.contrast);
    s(&mut dst.highlights, pr.highlights);
    s(&mut dst.shadows, pr.shadows);
    s(&mut dst.whites, pr.whites);
    s(&mut dst.blacks, pr.blacks);
    s(&mut dst.saturation, pr.saturation);
    s(&mut dst.vibrance, pr.vibrance);
    s(&mut dst.clarity, pr.clarity);
    s(&mut dst.texture, pr.texture);
    s(&mut dst.dehaze, pr.dehaze);
    s(&mut dst.noise_luma, pr.noise_luma);
    s(&mut dst.noise_chroma, pr.noise_chroma);
    s(&mut dst.vignette, pr.vignette);
    if dst.look_table.is_none() {
        // scale each cell toward identity (hue shift 0, scales 1) by amt
        dst.look_table = pr.look_table.clone().map(|mut m| {
            for c in &mut m.data {
                c[0] *= amt;
                c[1] = 1.0 + (c[1] - 1.0) * amt;
                c[2] = 1.0 + (c[2] - 1.0) * amt;
            }
            m
        });
    }
    if dst.rgb_lut.is_none() {
        dst.rgb_lut = pr.rgb_lut.clone();
        dst.lut_amount *= amt * pr.lut_amount;
    }
    // profile tone curve applies under the user's — take it when the
    // user's curve is the default identity
    let ident = |c: &Vec<[f32; 2]>| {
        c.len() == 2 && (c[0][0] - c[0][1]).abs() < 1e-4 && (c[1][0] - c[1][1]).abs() < 1e-4
    };
    if ident(&dst.curve) && !ident(&pr.curve) {
        dst.curve = pr.curve.clone();
    }
}

/// Float list inside an `<rdf:li>` item (strips the element tags).
fn li_floats(li: &str) -> Option<Vec<f32>> {
    let inner = match (li.find('>'), li.rfind('<')) {
        (Some(a), Some(b)) if b > a => &li[a + 1..b],
        _ => li,
    };
    let v: Vec<f32> = inner
        .split([',', ' ', '\n', '\r', '\t'])
        .filter_map(|t| t.trim().parse::<f32>().ok())
        .collect();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn local_corrections(body: &str, r: &mut Recipe) {
    use crate::recipe::{BrushLayer, BrushStroke, PowerWindow};
    let mut corr_id = 1u32;
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
            let n_win0 = r.windows.len();
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
            // LR writes local sliders either 0-100 (UI units) or already
            // normalized (-1..1) in Adaptive/preset XMP — the 1.5 threshold
            // distinguishes the two, same convention as saturation above.
            let loc = |names: &[&str]| {
                let v = field_any(&item, names).unwrap_or(0.0);
                (if v.abs() > 1.5 { v / 100.0 } else { v }).clamp(-1.0, 1.0)
            };
            let temp = loc(&["crs:LocalTemperature"]);
            // extended LR local adjustments (contrast/clarity/dehaze/tint)
            let con = loc(&["crs:LocalContrast2012", "crs:LocalContrast"]);
            // sharpness/noise have no dedicated channel: fold them into the
            // midtone-contrast channel (clarity) as an approximation; texture
            // maps to its own channel
            let cla = (loc(&["crs:LocalClarity2012", "crs:LocalClarity"])
                + loc(&["crs:LocalSharpness"]) * 0.5
                - loc(&["crs:LocalLuminanceNoise"]) * 0.6)
                .clamp(-1.0, 1.0);
            let tex = loc(&["crs:LocalTexture2012", "crs:LocalTexture"]);
            let deh = loc(&["crs:LocalDehaze"]);
            let tin = loc(&["crs:LocalTint"]);
            let hi = loc(&["crs:LocalHighlights2012", "crs:LocalHighlights"]);
            let sh = loc(&["crs:LocalShadows2012", "crs:LocalShadows"]);
            let wh = loc(&["crs:LocalWhites2012", "crs:LocalWhites"]);
            let bl = loc(&["crs:LocalBlacks2012", "crs:LocalBlacks"]);
            // legacy PV2003 "Brightness" ≈ gamma-lift — approximate as exposure
            let ev = (ev + loc(&["crs:LocalBrightness"]) * 0.5).clamp(-4.0, 4.0);
            // local color toning (hue/sat wheel) → temp/tint decomposition:
            // blue-yellow axis at 60°/240°, green-magenta at 120°/300°
            let ton = loc(&["crs:LocalToningSaturation"]) * 0.5;
            let th = field(&item, "crs:LocalToningHue")
                .unwrap_or(0.0)
                .to_radians();
            let temp = (temp + ton * (th - 1.0472).cos()).clamp(-1.0, 1.0);
            let tin = (tin + ton * (th - 5.236).cos()).clamp(-1.0, 1.0);
            // local saturation refine curve → modest saturation boost
            let sat = (sat + loc(&["crs:LocalCurveRefineSaturation"]) * 0.25).clamp(-1.0, 1.0);
            let feather_of =
                |mb: &str| (field(mb, "crs:Feather").unwrap_or(50.0) / 100.0).clamp(0.0, 1.0);
            // LR midpoint (0..100): 0 = at the zero-effect boundary.
            let mid_of = |mb: &str| {
                (1.0 - field(mb, "crs:Midpoint").unwrap_or(50.0) / 100.0).clamp(0.01, 0.99)
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
                contrast: con,
                clarity: cla,
                dehaze: deh,
                tint: tin,
                hi,
                sh,
                wh,
                bl,
                texture: tex,
                refine: [0.0; 6],
                pcs: Vec::new(),
                mask_digest: String::new(),
                mask_file: String::new(),
                mask_org: [0.0; 2],
                mask_area: [0.0; 2],
                blend: 0,
                group: 0,
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
            // AI masks (MaskGroupBasedCorrections): MaskSubType 1 = AI
            // subject matte, 2 = sky (heuristic matte), 3 = person part
            // (subject matte gated by a body-region prior from MaskName)
            for mb in mask_blocks(&item, "Image") {
                let sub = field(&mb, "crs:MaskSubType").unwrap_or(0.0) as i32;
                let inv = truthy(field_str(&mb, "crs:MaskInverted"));
                let n0 = r.windows.len();
                if sub == 1 {
                    r.windows.push(win("subject", [0.0; 6], 0.0, inv));
                } else if sub == 2 {
                    r.windows.push(win("sky", [0.0; 6], 0.0, inv));
                } else if sub == 3 {
                    let name = field_str(&mb, "crs:MaskName").unwrap_or_default();
                    let (cx, cy, rx, ry) = part_prior(&name);
                    r.windows
                        .push(win("part", [cx, cy, rx, ry, 0.0, 0.4], 0.0, inv));
                } else if sub == 0 {
                    // LR11+ landscape semantic masks: Sky reuses the sky
                    // matte, Vegetation the veg matte, the rest are
                    // (1 - sky) gated by a class region prior.
                    let name = field_str(&mb, "crs:MaskName").unwrap_or_default();
                    match name.as_str() {
                        "Sky" => r.windows.push(win("sky", [0.0; 6], 0.0, inv)),
                        "Vegetation" => {
                            r.windows
                                .push(win("veg", [0.5, 0.5, 1.5, 1.5, 0.0, 0.4], 0.0, inv))
                        }
                        _ => {
                            let (cx, cy, rx, ry) = land_prior(&name);
                            r.windows
                                .push(win("nsky", [cx, cy, rx, ry, 0.0, 0.4], 0.0, inv));
                        }
                    }
                }
                // AI mask bitmap binding: MaskDigest names the computed
                // matte in the catalog .lrcat-data blob store; Origin and
                // WholeImageArea place its bounding box in mask space.
                // MaskBlendMode says how this member combines inside its
                // correction: 0 add, 1 subtract, 2 intersect.
                if let Some(w) = r.windows.get_mut(n0) {
                    w.blend = field(&mb, "crs:MaskBlendMode").unwrap_or(0.0) as i32;
                    w.group = corr_id;
                    if let Some(d) = field_str(&mb, "crs:MaskDigest") {
                        w.mask_digest = d.to_string();
                    }
                    if let Some(o) = field_str(&mb, "crs:Origin") {
                        let v: Vec<f32> = o
                            .split(|c: char| c == ',' || c.is_whitespace())
                            .filter_map(|t| t.parse().ok())
                            .collect();
                        if v.len() >= 2 {
                            w.mask_org = [v[0], v[1]];
                        }
                    }
                    if let Some(a) = field_str(&mb, "crs:WholeImageArea") {
                        // "top,left,bottom,right" as n/d rationals
                        let v: Vec<f32> = a
                            .split(',')
                            .filter_map(|t| {
                                let mut it = t.trim().split('/');
                                let n: f32 = it.next()?.parse().ok()?;
                                let d: f32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);
                                (d != 0.0).then_some(n / d)
                            })
                            .collect();
                        if v.len() >= 4 {
                            w.mask_area = [v[3] - v[1], v[2] - v[0]];
                        }
                    }
                }
            }
            // LR11+ range-mask components (color/luminance range). Add-mode
            // components become standalone windows; subtract/intersect
            // fold into the previous window's single refine slot.
            for mb in mask_blocks(&item, "RangeMask") {
                let mode = field(&mb, "crs:MaskBlendMode").unwrap_or(0.0) as i32;
                let Some(rmi) = mb.find("<crs:CorrectionRangeMask") else {
                    continue;
                };
                let rm = &mb[rmi..];
                let ty = field(rm, "crs:Type").unwrap_or(1.0) as i32;
                if !(1..=2).contains(&ty) {
                    continue; // depth range masks — no depth map available
                }
                let inv = truthy(field_str(&mb, "crs:MaskInverted"))
                    || truthy(field_str(rm, "crs:Invert"));
                let (kind, p0, p1, p2) = if ty == 2 {
                    // luminance range
                    (
                        2,
                        field(rm, "crs:LumMin").unwrap_or(0.0).clamp(0.0, 1.0),
                        field(rm, "crs:LumMax").unwrap_or(1.0).clamp(0.0, 1.0),
                        field(rm, "crs:LumFeather").unwrap_or(0.1).clamp(0.0, 1.0),
                    )
                } else {
                    // color range: PointModels rows are [h01, s, luma, x, y, flag];
                    // take the primary sample (first)
                    let mut hh = 0.5;
                    let mut ss = 0.5;
                    for pm in seq_items(rm, "crs:PointModels") {
                        if let (Some(gt), Some(lt)) = (pm.find('>'), pm.rfind('<')) {
                            let v: Vec<f32> = pm[gt + 1..lt]
                                .split(|c: char| c == ',' || c.is_whitespace())
                                .filter_map(|t| t.parse().ok())
                                .collect();
                            if v.len() >= 2 {
                                hh = v[0].clamp(0.0, 1.0);
                                ss = v[1].clamp(0.0, 1.0);
                            }
                        }
                        break;
                    }
                    (
                        1,
                        hh,
                        ss,
                        field(rm, "crs:ColorAmount").unwrap_or(0.5).clamp(0.0, 1.0),
                    )
                };
                if mode >= 1 && r.windows.len() > n_win0 {
                    r.windows.last_mut().unwrap().refine = [
                        mode.min(2) as f32,
                        kind as f32,
                        p0,
                        p1,
                        p2,
                        if inv { 1.0 } else { 0.0 },
                    ];
                } else if mode == 0 {
                    let (kn, pp) = if kind == 2 {
                        ("lum", [p0, p1, p2, p2, 0.0, 0.0])
                    } else {
                        ("crange", [p0, p1, p2, 0.0, 0.0, 0.0])
                    };
                    r.windows.push(win(kn, pp, 0.0, inv));
                }
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
            // masked point colours: crs:LocalPointColors rows (same 19-float
            // serialisation as crs:PointColors) scoped to this correction's
            // mask, each with a LocalColorVariance strength. Attach to every
            // window this correction produced.
            let vars: Vec<f32> = seq_items(&item, "crs:LocalColorVariance")
                .iter()
                .filter_map(|li| {
                    let v = li_floats(li)?.into_iter().next()?;
                    // UI -100..100 → -1..1
                    Some(if v.abs() > 1.5 { v / 100.0 } else { v })
                })
                .collect();
            let mut pcs: Vec<([f32; 19], f32)> = Vec::new();
            for (si, li) in seq_items(&item, "crs:LocalPointColors").iter().enumerate() {
                let Some(row) = li_floats(li) else { continue };
                if row.len() != 19 {
                    continue;
                }
                let mut v = [0.0f32; 19];
                v.copy_from_slice(&row);
                pcs.push((v, vars.get(si).copied().unwrap_or(1.0)));
            }
            if !pcs.is_empty() {
                let from = n_win0.min(r.windows.len());
                for w in r.windows[from..].iter_mut() {
                    w.pcs.extend(pcs.iter().cloned());
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
                    contrast: con,
                    clarity: cla,
                    dehaze: deh,
                    tint: tin,
                    hi,
                    sh,
                    wh,
                    bl,
                    texture: tex,
                    strokes,
                });
            }
            // every mask member this correction produced belongs to the
            // same blend group — the renderer folds their mattes with
            // each member's MaskBlendMode before applying the correction
            let from = n_win0.min(r.windows.len());
            for w in r.windows[from..].iter_mut() {
                if w.group == 0 {
                    w.group = corr_id;
                }
            }
            corr_id += 1;
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
    let s2 =
        (attr(body, "crs:ParametricMidtoneSplit").unwrap_or(128.0) / 255.0).clamp(s1 + 0.02, 0.98);
    let s3 = (attr(body, "crs:ParametricHighlightSplit").unwrap_or(191.0) / 255.0)
        .clamp(s2 + 0.01, 0.99);
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
                for (x, dy) in [
                    (center - 0.07, 0.0),
                    (center, v / 100.0 * 0.15),
                    (center + 0.07, 0.0),
                ] {
                    r.hue_hue.push([x - x.floor(), x - x.floor() + dy]);
                }
            }
        }
        if let Some(v) = attr(body, &sat_tag) {
            if v != 0.0 {
                for (x, g) in [
                    (center - 0.07, 1.0),
                    (center, 1.0 + v / 100.0),
                    (center + 0.07, 1.0),
                ] {
                    r.hue_sat.push([x - x.floor(), g.max(0.0)]);
                }
            }
        }
        if let Some(v) = attr(body, &lum_tag) {
            if v != 0.0 {
                for (x, g) in [
                    (center - 0.07, 1.0),
                    (center, 1.0 + v / 150.0),
                    (center + 0.07, 1.0),
                ] {
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
    fn amount_scales_adjustments() {
        let xmp = r##"<x:xmpmeta xmlns:x="x" xmlns:crs="y">
<rdf:RDF xmlns:rdf="r"><rdf:Description rdf:about=""
 crs:Exposure2012="1.0" crs:Contrast2012="50"
 crs:Saturation="-20" crs:Amount="0.5"/></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(xmp);
        assert!((r.exposure - 0.5).abs() < 1e-4, "exposure {}", r.exposure);
        assert!((r.saturation - (-0.1)).abs() < 1e-4, "sat {}", r.saturation);
        // contrast is parsed in LR units — half of whatever it was
        assert!(r.contrast.abs() < 0.51);
    }

    #[test]
    fn amount_default_one_is_noop() {
        let xmp = r##"<rdf:Description rdf:about="" crs:Exposure2012="1.0" crs:Amount="1.0"/>"##;
        let r = crs_to_recipe(xmp);
        assert!((r.exposure - 1.0).abs() < 1e-4);
    }

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
            mask_digest: String::new(),
            mask_file: String::new(),
            mask_org: [0.0; 2],
            mask_area: [0.0; 2],
            blend: 0,
            group: 0,
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
    fn process_version_roundtrips() {
        let dir = std::env::temp_dir().join(format!("xmp_test_pv_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // parse: the catalog's stored version string lands on the recipe
        let r = crs_to_recipe(
            r#"<rdf:Description rdf:about="" crs:ProcessVersion="15.4" crs:Exposure2012="+0.5"/>"#,
        );
        assert!((r.process_version - 15.4).abs() < 1e-6);
        assert!((r.exposure - 0.5).abs() < 1e-6);
        // write: a stored PV is emitted verbatim; unset stays "11.0"
        let asset = dir.join("pv.arw");
        std::fs::write(&asset, b"fake").unwrap();
        let mut sc = Sidecar::default();
        sc.recipe.process_version = 15.4;
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:ProcessVersion=\"15.4\""));
        sc.recipe.process_version = 0.0;
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:ProcessVersion=\"11.0\""));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keywords_roundtrip_dc_subject() {
        let dir = std::env::temp_dir().join(format!("xmp_test_kw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("kw.arw");
        std::fs::write(&asset, b"fake").unwrap();
        let mut sc = Sidecar::default();
        sc.keywords = vec!["ポートレート".into(), "raw & co".into()];
        write_xmp(&asset, &sc).unwrap();
        let back = read_xmp(&asset).unwrap();
        assert_eq!(back.keywords, sc.keywords);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hdr_fields_roundtrip() {
        let dir = std::env::temp_dir().join(format!("xmp_test_hdr_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // parse: LR13+ HDR edit metadata lands on the recipe
        let r = crs_to_recipe(
            r#"<rdf:Description rdf:about="" crs:HDREditMode="1" crs:HDRMaxValue="4" crs:SDRBlend="0" crs:SDRBrightness="5" crs:SDRContrast="25" crs:SDRHighlights="0" crs:SDRShadows="0" crs:SDRWhites="0" crs:SDRClarity="0"/>"#,
        );
        assert!(r.hdr_edit);
        assert!((r.hdr_max - 4.0).abs() < 1e-6);
        assert!((r.sdr[1] - 0.05).abs() < 1e-6);
        assert!((r.sdr[2] - 0.25).abs() < 1e-6);
        // write: emitted only when hdr_edit is set, and parses back
        let asset = dir.join("hdr.arw");
        std::fs::write(&asset, b"fake").unwrap();
        let mut sc = Sidecar::default();
        sc.recipe.hdr_edit = true;
        sc.recipe.hdr_max = 4.0;
        sc.recipe.sdr = [0.0, 0.05, 0.25, 0.0, 0.0, 0.0, 0.0];
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:HDREditMode=\"1\""));
        assert!(xml.contains("crs:HDRMaxValue=\"4\""));
        assert!(xml.contains("crs:SDRContrast=\"25\""));
        let back = read_xmp(&asset).unwrap();
        assert!(back.recipe.hdr_edit);
        assert!((back.recipe.hdr_max - 4.0).abs() < 1e-6);
        assert!((back.recipe.sdr[1] - 0.05).abs() < 1e-6);
        assert!((back.recipe.sdr[2] - 0.25).abs() < 1e-6);
        // off by default — plain recipes don't gain HDR attrs
        write_xmp(&asset, &Sidecar::default()).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(!xml.contains("HDREditMode"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unmodeled_attrs_passthrough() {
        let dir = std::env::temp_dir().join(format!("xmp_test_ext_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("ext.arw");
        std::fs::write(&asset, b"fake").unwrap();
        std::fs::write(
            xmp_path_for(&asset),
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" crs:Exposure2012="+1.0" crs:UprightVersion="151388160" crs:UprightCenterMode="0" crs:RetouchInfo="source = 'lomo', spotType = 'heal'" crs:Version="15.4" xmp:Rating="4" xmpMM:DocumentID="abc-123"/></rdf:Description></rdf:RDF></x:xmpmeta>"#,
        )
        .unwrap();
        let sc = read_xmp(&asset).unwrap();
        assert!(sc.extras.iter().any(|(k, _)| k == "crs:UprightVersion"));
        assert!(sc.extras.iter().any(|(k, _)| k == "crs:RetouchInfo"));
        assert!(sc.extras.iter().any(|(k, _)| k == "xmpMM:DocumentID"));
        // modeled + sidecar-level keys don't enter extras
        assert!(!sc.extras.iter().any(|(k, _)| k == "crs:Exposure2012"));
        assert!(!sc.extras.iter().any(|(k, _)| k == "xmp:Rating"));
        // re-emit: extras land verbatim, no duplicates of modeled keys
        write_xmp(&asset, &sc).unwrap();
        let xml = std::fs::read_to_string(xmp_path_for(&asset)).unwrap();
        assert!(xml.contains("crs:UprightVersion=\"151388160\""));
        assert!(xml.contains("xmpMM:DocumentID=\"abc-123\""));
        assert_eq!(xml.matches("crs:Exposure2012=").count(), 1);
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
    fn retouch_areas_map_to_clones() {
        let body = r##"<x:xmpmeta><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="">
 <crs:RetouchAreas>
  <rdf:Seq>
   <rdf:li crs:What="Retouch" crs:Method="clone" crs:SpotX="0.4" crs:SpotY="0.5" crs:SpotDiam="0.08" crs:SourceX="0.6" crs:SourceY="0.5"/>
  </rdf:Seq>
 </crs:RetouchAreas>
</rdf:Description></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(body);
        assert_eq!(r.clones.len(), 1);
        let c = r.clones[0];
        assert!((c[0] - 0.6).abs() < 1e-6 && (c[1] - 0.5).abs() < 1e-6);
        assert!((c[2] - 0.4).abs() < 1e-6 && (c[3] - 0.5).abs() < 1e-6);
        assert!((c[4] - 0.04).abs() < 1e-6);
    }

    #[test]
    fn retouch_patchmatch_attrs_bind_pm_spot() {
        let body = r##"<x:xmpmeta><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="">
 <crs:RetouchAreas>
  <rdf:Seq>
   <rdf:li crs:What="Retouch" crs:Method="poisson" crs:SpotX="0.6" crs:SpotY="0.7" crs:SpotDiam="0.05" crs:SourceX="0.4" crs:SourceY="0.7" crs:PmTarget="100,200,180,260" crs:PmSearch="300,100,500,360" crs:PmFrame="8,12" crs:PmEllipse="0.6,0.7,0.03,0.05" crs:PmPatch="A1B2C3D4" crs:PmOpacity="0.8"/>
  </rdf:Seq>
 </crs:RetouchAreas>
</rdf:Description></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(body);
        assert_eq!(r.pm_spots.len(), 1);
        let s = &r.pm_spots[0];
        assert_eq!(s.target, [100.0, 200.0, 180.0, 260.0]);
        assert_eq!(s.search, [300.0, 100.0, 500.0, 360.0]);
        assert_eq!(s.frame_org, [8.0, 12.0]);
        assert_eq!(s.ellipse, [0.6, 0.7, 0.03, 0.05]);
        assert_eq!(s.digest, "A1B2C3D4");
        assert!((s.opacity - 0.8).abs() < 1e-6);
        // a pm item never falls through to the clone approximation
        assert!(r.clones.is_empty());
    }

    #[test]
    fn ai_mask_subtypes_and_fractional_locals() {
        // Adobe Adaptive presets write fractional (-1..1) local sliders and
        // AI mask subtypes: 1 = subject, 2 = sky, 3 = person part.
        let body = r##"<x:xmpmeta><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="">
 <crs:MaskGroupBasedCorrections>
  <rdf:Seq>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalDehaze="0.247045" crs:LocalLuminanceNoise="0.798285" crs:LocalSaturation="-0.350261">
    <crs:CorrectionMasks>
     <rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskName="Sky" crs:MaskSubType="2"/>
     </rdf:Seq>
    </crs:CorrectionMasks>
   </rdf:li>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalSaturation="0.07325" crs:LocalSharpness="0.144355">
    <crs:CorrectionMasks>
     <rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskName="Iris and Pupil" crs:MaskSubType="3"/>
     </rdf:Seq>
    </crs:CorrectionMasks>
   </rdf:li>
  </rdf:Seq>
 </crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(body);
        assert_eq!(r.windows.len(), 2);
        let sky = &r.windows[0];
        assert_eq!(sky.kind, "sky");
        // fractional values pass through unscaled (not divided by 100)
        assert!((sky.dehaze - 0.247045).abs() < 1e-5);
        assert!((sky.sat + 0.350261).abs() < 1e-5);
        // luminance noise folds into negative clarity
        assert!(sky.clarity < -0.4);
        let part = &r.windows[1];
        assert_eq!(part.kind, "part");
        // Iris prior: centered near the upper-middle of the frame
        assert!((part.p[0] - 0.5).abs() < 1e-6 && (part.p[1] - 0.28).abs() < 1e-6);
        assert!((part.sat - 0.07325).abs() < 1e-5);
        // sharpness folds into positive clarity
        assert!(part.clarity > 0.05);
    }

    #[test]
    fn landscape_semantic_masks_subtype0() {
        let body = r##"<x:xmpmeta><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="">
 <crs:MaskGroupBasedCorrections>
  <rdf:Seq>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:LocalSaturation="0.3">
    <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskName="Vegetation" crs:MaskSubType="0"/>
    </rdf:Seq></crs:CorrectionMasks>
   </rdf:li>
   <rdf:li crs:What="Correction" crs:CorrectionAmount="1" crs:LocalExposure="-0.5">
    <crs:CorrectionMasks><rdf:Seq>
      <rdf:li crs:What="Mask/Image" crs:MaskName="Natural Ground" crs:MaskSubType="0"/>
    </rdf:Seq></crs:CorrectionMasks>
   </rdf:li>
  </rdf:Seq>
 </crs:MaskGroupBasedCorrections>
</rdf:Description></rdf:RDF></x:xmpmeta>"##;
        let r = crs_to_recipe(body);
        assert_eq!(r.windows.len(), 2);
        assert_eq!(r.windows[0].kind, "veg");
        assert_eq!(r.windows[1].kind, "nsky");
        // ground prior: lower half of frame
        assert!(r.windows[1].p[1] > 0.6);
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

    #[test]
    fn point_colors_seq_parse() {
        // real LR serialization: rdf:Seq of <rdf:li>float rows (element text)
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4"><crs:PointColors><rdf:Seq><rdf:li>0.333333, 0.000000, 0.666667, 1.000000, 0.000000, 0.745889, 0.195889, 1.000000, 1.000000, 0.000000, 0.500000, 0.740754, 0.190754, 1.000000, 1.000000, 0.000000, 4.127050, 0.839618, 0.920754</rdf:li><rdf:li>-1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000, -1.000000</rdf:li></rdf:Seq></crs:PointColors></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let r = crs_to_recipe(xmp);
        assert_eq!(r.point_colors.len(), 2);
        assert!((r.point_colors[0][16] - 4.12705).abs() < 1e-5);
        assert!(r.point_colors[1].iter().all(|x| *x < 0.0));
        // write_xmp round-trips the rows
        let dir = std::env::temp_dir().join(format!("xmp_test_pc_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let asset = dir.join("a.nef");
        std::fs::write(&asset, b"fake").unwrap();
        let sc = Sidecar {
            recipe: r,
            ..Default::default()
        };
        write_xmp(&asset, &sc).unwrap();
        let r2 = crs_to_recipe(&std::fs::read_to_string(xmp_path_for(&asset)).unwrap());
        assert_eq!(r2.point_colors.len(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn range_mask_components_parse() {
        // real LR11+ structure: AI sky mask + color-range refinement
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:LocalExposure2012="-0.4395"><crs:CorrectionMasks><rdf:Seq>
        <rdf:li crs:What="Mask/Image" crs:MaskName="Sky 1" crs:MaskBlendMode="0" crs:MaskSubType="2" crs:MaskValue="1"/>
        <rdf:li><rdf:Description crs:What="Mask/RangeMask" crs:MaskName="Color Range 1" crs:MaskBlendMode="2" crs:MaskInverted="false" crs:MaskValue="1"><crs:CorrectionRangeMask><rdf:Description crs:Version="3" crs:Type="1" crs:ColorAmount="0.494949" crs:Invert="false" crs:SampleType="0"><crs:PointModels><rdf:Seq><rdf:li>0.562975 0.388881 0.064111 0.663744 0.266272 0</rdf:li></rdf:Seq></crs:PointModels></rdf:Description></crs:CorrectionRangeMask></rdf:Description></rdf:li>
        </rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let r = crs_to_recipe(xmp);
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].kind, "sky");
        let rf = r.windows[0].refine;
        assert_eq!(rf[0] as i32, 2, "intersect mode");
        assert_eq!(rf[1] as i32, 1, "color kind");
        assert!((rf[2] - 0.562975).abs() < 1e-5);
        assert!((rf[3] - 0.388881).abs() < 1e-5);
        assert!((rf[4] - 0.494949).abs() < 1e-5);
        // standalone color-range component (add mode)
        let xmp2 = xmp.replace("MaskBlendMode=\"2\"", "MaskBlendMode=\"0\"");
        let r2 = crs_to_recipe(&xmp2);
        assert_eq!(r2.windows.len(), 2);
        assert_eq!(r2.windows[1].kind, "crange");
        assert!((r2.windows[1].p[0] - 0.562975).abs() < 1e-5);
    }

    #[test]
    fn lens_blur_full_attrs_parse() {
        // real "Adaptive - Blur Background/Bubble Swirl" attribute set
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:LensBlur crs:Version="1" crs:Active="true" crs:BlurAmount="70" crs:BokehShape="0" crs:BokehShapeDetail="0" crs:HighlightsThreshold="50" crs:HighlightsBoost="50" crs:CatEyeAmount="80" crs:CatEyeScale="100" crs:BokehAspect="0" crs:BokehRotation="50" crs:SphericalAberration="0"/></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let r = crs_to_recipe(xmp);
        assert!((r.lens_blur - 0.7).abs() < 1e-5);
        assert!((r.lens_blur_opts2[0] - 0.8).abs() < 1e-5, "cat eye");
        assert!((r.lens_blur_opts2[1] - 1.0).abs() < 1e-5, "cat eye scale");
        assert!((r.lens_blur_opts2[2] - 0.5).abs() < 1e-5, "rotation");
    }

    #[test]
    fn local_point_colors_parse() {
        // real Adobe "Adaptive - Landscape/Autumn01" structure: a Vegetation
        // mask correction carrying two LocalPointColors swatches with
        // per-swatch LocalColorVariance strengths.
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"><crs:MaskGroupBasedCorrections><rdf:Seq><rdf:li><rdf:Description crs:What="Correction" crs:CorrectionAmount="1" crs:CorrectionActive="true" crs:CorrectionName="Vegetation"><crs:LocalPointColors><rdf:Seq>
        <rdf:li>0.648015, 0.915783, 0.651444, 0.000000, 0.050000, 0.110000, 0.500000, 0.000000, 0.170000, 0.670000, 1.000000, 0.185782, 0.735783, 1.000000, 1.000000, 0.097473, 0.647473, 1.000000, 1.000000</rdf:li>
        <rdf:li>0.650076, 0.919954, 0.062072, 0.000000, 0.080000, 0.000000, 0.500000, 0.000000, 0.333333, 0.666667, 1.000000, 0.189954, 0.739954, 1.000000, 1.000000, 0.000000, 0.096355, 0.456355, 1.000000</rdf:li>
        </rdf:Seq></crs:LocalPointColors><crs:LocalColorVariance><rdf:Seq><rdf:li>0.720000</rdf:li><rdf:li>0.460000</rdf:li></rdf:Seq></crs:LocalColorVariance>
        <crs:CorrectionMasks><rdf:Seq><rdf:li crs:What="Mask/Image" crs:MaskName="Vegetation" crs:MaskSubType="0" crs:MaskValue="1"/></rdf:Seq></crs:CorrectionMasks></rdf:Description></rdf:li></rdf:Seq></crs:MaskGroupBasedCorrections></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let r = crs_to_recipe(xmp);
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].kind, "veg");
        assert_eq!(r.windows[0].pcs.len(), 2);
        // swatch rows preserved verbatim + variance attached in order
        assert!((r.windows[0].pcs[0].0[0] - 0.648015).abs() < 1e-5);
        assert!((r.windows[0].pcs[0].1 - 0.72).abs() < 1e-5);
        assert!((r.windows[0].pcs[1].0[0] - 0.650076).abs() < 1e-5);
        assert!((r.windows[0].pcs[1].1 - 0.46).abs() < 1e-5);
    }

    /// encode bytes the way Adobe writes crs:Table_* payloads: LE u32
    /// uncompressed length, zlib stream, then their base85 alphabet
    /// (little-endian u32 per 5-char group, digits ordered LSB-first).
    #[cfg(test)]
    fn b85_encode(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 85] = b"0123456789abcdefghijklmnopqrstuvwxyz\
            ABCDEFGHIJKLMNOPQRSTUVWXYZ.-:+=^!/*?&'|()[]{}@%$#";
        let mut s = String::with_capacity(bytes.len() / 4 * 5 + 5);
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            let mut v = u32::from_le_bytes(b);
            for _ in 0..chunk.len() + 1 {
                s.push(DIGITS[(v % 85) as usize] as char);
                v /= 85;
            }
        }
        s
    }

    #[test]
    fn look_profile_xmp_parses() {
        // synthesize a tiny dng_look_table (4 hue × 2 sat × 2 val),
        // wrap it the way Adobe profile .xmp files do and check the
        // whole crs:Look / crs:LookTable / crs:Table_<digest> chain.
        let mut bin = Vec::new();
        bin.extend(0u32.to_le_bytes()); // BTT_LOOK_TABLE
        bin.extend(1u32.to_le_bytes()); // version
        for d in [4u32, 2, 2] {
            bin.extend(d.to_le_bytes());
        }
        for _ in 0..16u32 {
            bin.extend(12.5f32.to_le_bytes()); // hue shift
            bin.extend(1.0f32.to_le_bytes()); // sat scale
            bin.extend(1.0f32.to_le_bytes()); // val scale
        }
        let mut payload = (bin.len() as u32).to_le_bytes().to_vec();
        payload.extend(miniz_oxide::deflate::compress_to_vec_zlib(&bin, 6));
        let enc = b85_encode(&payload);

        let xmp = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:ProcessVersion="15.4"><crs:Look><rdf:Description crs:Name="Adobe Landscape" crs:Amount="0.5" crs:LookTable="abc123" crs:Table_abc123="{enc}"/></crs:Look></rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let r = crs_to_recipe(&xmp);
        assert_eq!(r.profile, "Adobe Landscape");
        let lt = r.look_table.expect("look table decoded");
        assert_eq!(lt.dims, [4, 2, 2]);
        assert!((lt.data[0][0] - 12.5).abs() < 1e-4);
        // the Look's own crs:Amount becomes the profile-strength slider…
        assert!((r.profile_amount - 0.5).abs() < 1e-4);
        // …and must NOT be treated as preset strength — nothing was set,
        // so nothing should be scaled.
        assert!(r.exposure.abs() < 1e-6 && r.contrast.abs() < 1e-6);
    }

    #[test]
    fn look_name_falls_back_to_camera_profile() {
        // a Look name that resolves to no enhanced .xmp profile leaves the
        // base CameraProfile (the DCP the look sits on) in place
        let xmp = r#"<rdf:Description rdf:about="" crs:CameraProfile="Adobe Standard"><crs:Look><rdf:Description crs:Name="No Such Profile Zz9"/></crs:Look></rdf:Description>"#;
        let r = crs_to_recipe(xmp);
        assert_eq!(r.profile, "Adobe Standard");
    }

    #[test]
    fn look_name_overrides_when_profile_resolves() {
        // LR semantics: crs:Look.Name is the effective profile;
        // crs:CameraProfile is just the base DCP. When the name resolves to
        // an enhanced .xmp profile on disk it wins. Adobe Color ships with
        // Lightroom; if absent this test degrades to the fallback case.
        if find_xmp_profile("Adobe Color").is_none() {
            return;
        }
        let xmp = r#"<rdf:Description rdf:about="" crs:CameraProfile="Adobe Standard"><crs:Look><rdf:Description crs:Name="Adobe Color"/></crs:Look></rdf:Description>"#;
        let r = crs_to_recipe(xmp);
        assert_eq!(r.profile, "Adobe Color");
    }
}
