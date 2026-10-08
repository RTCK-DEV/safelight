//! Lightroom-compatible XMP sidecar IO.
//!
//! `write_sidecar` also emits `<photo>.xmp` mirroring the recipe in the
//! `crs:` (Camera Raw Settings) namespace so Lightroom Classic / Camera Raw /
//! darktable can read our basic adjustments — and we read the same fields
//! back when a folder contains LR-authored XMP without an `.araware.json`.
//!
//! Round-trip fidelity: LR fields cover only a subset of the recipe and
//! rescale our [-1,1] ranges to LR's integer [-100,+100] convention, so the
//! full recipe is also embedded verbatim in `<ara:Recipe>` — araware reads
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
    a.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"araware\">\n");
    a.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
    a.push_str("  <rdf:Description rdf:about=\"\"\n");
    a.push_str("   xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"\n");
    a.push_str("   xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n");
    a.push_str("   xmlns:ara=\"https://araware.app/ns/1.0/\"\n");
    a.push_str("   crs:Version=\"15.0\" crs:ProcessVersion=\"11.0\" crs:RawFileName=\"");
    a.push_str(&esc(&asset
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()));
    a.push_str("\"\n");
    // WB: LR stores absolute Kelvin/Tint for "As Shot", incremental for custom
    a.push_str(&format!(
        "   crs:WhiteBalance=\"Custom\" crs:IncrementalTemperature=\"{}\" crs:IncrementalTint=\"{}\"\n",
        lr(r.temperature, 100.0),
        lr(r.tint, 100.0)
    ));
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
pub fn read_xmp(asset: &Path) -> Option<Sidecar> {
    let body = std::fs::read_to_string(find_xmp(asset)?).ok()?;
    let mut sc = Sidecar::default();
    // lossless path: our own embedded recipe
    if let (Some(i), Some(j)) = (body.find("<ara:Recipe>"), body.find("</ara:Recipe>")) {
        let js = unescape(body[i + 12..j].trim());
        if let Ok(r) = serde_json::from_str::<Recipe>(&js) {
            sc.recipe = r;
        }
    } else {
        let r = &mut sc.recipe;
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
    }
    if let Some(v) = attr(&body, "xmp:Rating") {
        sc.rating = v.round() as i32;
    }
    if let Some(l) = attr_str(&body, "xmp:Label") {
        sc.label = l;
    }
    Some(sc)
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
    fn roundtrip_ara_recipe_lossless() {
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
}
