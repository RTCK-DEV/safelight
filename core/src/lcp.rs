//! Adobe Lens Correction Profile (.lcp) reader — the profiles Lightroom
//! ships/uses. An .lcp is XMP: `photoshop:CameraProfiles > rdf:Seq > rdf:li`
//! entries, each a calibration sample keyed by focal/aperture/distance,
//! containing a PerspectiveModel (radial+tangential distortion), optional
//! Chromatic{Red,Green,Blue}GreenModel warps, and a VignetteModel.
//!
//! Adobe Camera Model (per Adobe's ACM tech note / RawTherapee's impl):
//!   normalize:   xd = (x - x0) / (FocalLengthX · Dmax)   // focal units
//!   distort fwd: cf  = 1 + k1·r² + k2·r⁴ + k3·r⁶ + 2·(t2·yd + t1·xd)
//!                x_d = xd·cf + t1·r²,  y_d = yd·cf + t2·r²
//!   (t1 = RadialDistortParam4? no — param4 is x-tangential (param[4]=xfac),
//!    param[3] is y-tangential)
//!   chromatic:   warp of the DISTORTED green position per channel:
//!                x_c = sf·(xg·cf_c + t1c·r²)·f + x0
//!   vignette:    residual illumination L(r) = 1 + α1·r² + α2·r⁴ + α3·r⁶ on
//!                distorted focal-unit radius; gain = 1/L (RT uses the
//!                truncated series of 1/L — exact division is equivalent).
//!
//! The warp is defined corrected→distorted, i.e. exactly our
//! output-pixel→source-pixel direction, so no inversion is needed.

use crate::lensdb::{AdobeCor, Correction};
use std::path::PathBuf;

#[derive(Clone, Copy, Default)]
struct Cal {
    focal: f32,
    aperture: f32,
    /// [k1,k2,k3, t_y(param3), t_x(param4)]
    dist: Option<[f32; 5]>,
    foc: [f32; 2],
    center: [f32; 2],
    /// [sf, k1,k2,k3, t_y, t_x]
    chr: Option<[f32; 6]>,
    chg: Option<[f32; 6]>,
    chb: Option<[f32; 6]>,
    vig_a: Option<[f32; 3]>,
    vig_foc: [f32; 2],
    vig_center: [f32; 2],
}

struct LcpLens {
    names: Vec<String>,
    sensor_factor: f32,
    cals: Vec<Cal>,
}

/// Lazy index: filename gives the lens name ("Make (Lens).lcp"), so we only
/// parse the handful of files whose name can match the query lens.
pub struct LcpDb {
    /// (path, names derived from the filename)
    files: Vec<(PathBuf, Vec<String>)>,
    parsed: std::sync::Mutex<std::collections::HashMap<PathBuf, Option<std::sync::Arc<LcpLens>>>>,
}

// ---- scoped attribute scanner ----------------------------------------------
// Fields appear as stCamera:Field="v". Scope slices isolate each nested
// model block so same-named params land in the right model.

fn attr_val(s: &str, field: &str) -> Option<f32> {
    for name in [format!("stCamera:{field}"), field.to_string()] {
        if let Some(i) = s.find(&format!("{name}=\"")) {
            let b = &s[i + name.len() + 2..];
            if let Some(j) = b.find('"') {
                if let Ok(v) = b[..j].trim().parse() {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn attr_txt(s: &str, field: &str) -> Option<String> {
    for name in [format!("stCamera:{field}"), field.to_string()] {
        if let Some(i) = s.find(&format!("{name}=\"")) {
            let b = &s[i + name.len() + 2..];
            if let Some(j) = b.find('"') {
                return Some(b[..j].trim().to_string());
            }
        }
    }
    None
}

/// slice from `marker` covering the opening tag's attributes and (when not
/// self-closing) the nested rdf:Description — needed for both
/// <stCamera:VignetteModel .../> and <stCamera:PerspectiveModel><rdf:Description ...>
/// variants. Stops before the next <stCamera: sibling element.
/// for self-closing model elements (<stCamera:VignetteModel ... />).
fn scope_full<'a>(s: &'a str, marker: &str) -> &'a str {
    let Some(i) = s.find(marker) else {
        return "";
    };
    let rest = &s[i + marker.len()..];
    let from = rest.find('>').map(|j| j + 1).unwrap_or(0);
    let open_attrs = &rest[..from];
    // is the opening tag self-closing?
    let body_end = if open_attrs.trim_end().ends_with('/') || open_attrs.ends_with('/') {
        0
    } else {
        let body = &rest[from..];
        body.find("<stCamera:")
            .or_else(|| body.find("</rdf:Description"))
            .or_else(|| body.find("</rdf:li"))
            .unwrap_or(body.len())
            + from
    };
    &s[i + marker.len()..i + marker.len() + body_end.max(from)]
}

fn parse_model(s: &str, marker: &str) -> Option<([f32; 5], f32, [f32; 2], [f32; 2])> {
    let sc = scope_full(s, marker);
    if sc.is_empty() {
        return None;
    }
    let k1 = attr_val(sc, "RadialDistortParam1")?;
    let k = [
        k1,
        attr_val(sc, "RadialDistortParam2").unwrap_or(0.0),
        attr_val(sc, "RadialDistortParam3").unwrap_or(0.0),
        attr_val(sc, "RadialDistortParam4").unwrap_or(0.0),
        attr_val(sc, "RadialDistortParam5").unwrap_or(0.0),
    ];
    let sf = attr_val(sc, "ScaleFactor").unwrap_or(1.0);
    let foc = [
        attr_val(sc, "FocalLengthX").unwrap_or(-1.0),
        attr_val(sc, "FocalLengthY").unwrap_or(-1.0),
    ];
    let ctr = [
        attr_val(sc, "ImageXCenter").unwrap_or(0.5),
        attr_val(sc, "ImageYCenter").unwrap_or(0.5),
    ];
    Some((k, sf, foc, ctr))
}

fn parse_lcp(text: &str) -> Option<LcpLens> {
    let mut names = Vec::new();
    let mut sensor_factor = 1.0f32;
    let mut cals = Vec::new();

    // iterate TOP-LEVEL <rdf:li> entries — nested <rdf:li> occur inside
    // AlternateLensNames lists, so track open/close depth. Only entries
    // whose first child is <rdf:Description> are calibration blocks.
    let mut pos = 0usize;
    while let Some(rel) = text[pos..].find("<rdf:li") {
        let i = pos + rel;
        // skip the opening tag
        let Some(gt) = text[i..].find('>') else { break };
        let open_end = i + gt + 1;
        // must contain an rdf:Description as first element
        let after = text[open_end..].trim_start();
        if !after.starts_with("<rdf:Description") {
            pos = open_end;
            continue;
        }
        // depth-counting scan for the matching </rdf:li>
        let mut depth = 1usize;
        let mut j = open_end;
        let mut end = text.len();
        while j < text.len() {
            let nxt_open = text[j..].find("<rdf:li").map(|k| j + k);
            let nxt_close = text[j..].find("</rdf:li>").map(|k| j + k);
            match (nxt_open, nxt_close) {
                (Some(o), Some(c)) => {
                    if o < c {
                        depth += 1;
                        j = o + 7;
                    } else {
                        depth -= 1;
                        j = c + 9;
                        if depth == 0 {
                            end = c;
                            break;
                        }
                    }
                }
                (Some(o), None) => {
                    depth += 1;
                    j = o + 7;
                }
                (None, Some(c)) => {
                    depth -= 1;
                    j = c + 9;
                    if depth == 0 {
                        end = c;
                        break;
                    }
                }
                (None, None) => break,
            }
        }
        let li = &text[i..end.min(text.len())];
        pos = (open_end.max(j)).min(text.len());
        if end >= text.len() {
            pos = text.len();
        }

        if names.is_empty() {
            for f in ["LensPrettyName", "Lens", "ProfileName"] {
                if let Some(n) = attr_txt(li, f) {
                    let n = n
                        .trim_start_matches("Adobe (")
                        .trim_end_matches(')')
                        .trim()
                        .to_string();
                    if !n.is_empty() && !names.iter().any(|x| *x == n) {
                        names.push(n);
                    }
                }
            }
            // UniqueCameraModel carries the EXIF camera-model token
            // ("DJI AC003 OsmoAction4") — fixed-lens cameras that report
            // no EXIF lens are matched on this instead
            for f in ["UniqueCameraModel"] {
                if let Some(n) = attr_txt(li, f) {
                    let n = format!("ucm:{n}");
                    if !names.iter().any(|x| *x == n) {
                        names.push(n);
                    }
                }
            }
            sensor_factor = attr_val(li, "SensorFormatFactor").unwrap_or(1.0);
        }
        // AlternateLensNames: nested <rdf:li>name</rdf:li> elements
        // (skip the block's own opening <rdf:li...> tag)
        {
            let mut r = &li[li.find('>').map(|g| g + 1).unwrap_or(0)..];
            while let Some(k) = r.find("<rdf:li>") {
                let b = &r[k + 8..];
                if let Some(e) = b.find("</rdf:li>") {
                    let n = b[..e].trim().to_string();
                    if !n.is_empty() && !names.iter().any(|x| *x == n) {
                        names.push(n);
                    }
                    r = &b[e..];
                } else {
                    break;
                }
            }
        }

        let mut cal = Cal {
            focal: attr_val(li, "FocalLength").unwrap_or(0.0),
            aperture: attr_val(li, "ApertureValue").unwrap_or(0.0),
            ..Default::default()
        };

        if let Some((k, _sf, foc, ctr)) = parse_model(li, "PerspectiveModel") {
            cal.dist = Some(k);
            cal.foc = foc;
            cal.center = ctr;
        } else {
            continue; // FisheyeModel profiles unsupported for now
        }
        if let Some((k, sf, _f, _c)) = parse_model(li, "ChromaticRedGreenModel") {
            cal.chr = Some([sf, k[0], k[1], k[2], k[3], k[4]]);
        }
        if let Some((k, sf, _f, _c)) = parse_model(li, "ChromaticBlueGreenModel") {
            cal.chb = Some([sf, k[0], k[1], k[2], k[3], k[4]]);
        }
        if let Some((k, sf, _f, _c)) = parse_model(li, "ChromaticGreenModel") {
            cal.chg = Some([sf, k[0], k[1], k[2], k[3], k[4]]);
        }
        {
            let vsc = scope_full(li, "VignetteModel");
            if !vsc.is_empty() {
                if let Some(a1) = attr_val(vsc, "VignetteModelParam1") {
                    cal.vig_a = Some([
                        a1,
                        attr_val(vsc, "VignetteModelParam2").unwrap_or(0.0),
                        attr_val(vsc, "VignetteModelParam3").unwrap_or(0.0),
                    ]);
                    cal.vig_foc = [
                        attr_val(vsc, "FocalLengthX").unwrap_or(-1.0),
                        attr_val(vsc, "FocalLengthY").unwrap_or(-1.0),
                    ];
                    cal.vig_center = [
                        attr_val(vsc, "ImageXCenter").unwrap_or(0.5),
                        attr_val(vsc, "ImageYCenter").unwrap_or(0.5),
                    ];
                }
            }
        }
        cals.push(cal);
    }
    if names.is_empty() || cals.is_empty() {
        return None;
    }
    Some(LcpLens {
        names,
        sensor_factor,
        cals,
    })
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

impl LcpDb {
    pub fn load_root(dir: &std::path::Path) -> Option<LcpDb> {
        let mut files = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("lcp") {
                    // names from "<Make> (<lens>) - RAW.lcp" / "... .lcp"
                    let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let mut names = Vec::new();
                    if let (Some(a), Some(b)) = (stem.find('('), stem.rfind(')')) {
                        names.push(stem[a + 1..b].trim().to_string());
                    }
                    // maker = text before '(' (camera brand the profile was
                    // calibrated against) — kept as a synthetic "make:" name
                    if let Some(a) = stem.find('(') {
                        names.push(format!("make:{}", stem[..a].trim()));
                    }
                    let base = stem.split('-').next().unwrap_or(stem).trim();
                    names.push(base.to_string());
                    names.retain(|n| !n.is_empty());
                    files.push((p, names));
                }
            }
        }
        if files.is_empty() {
            None
        } else {
            Some(LcpDb {
                files,
                parsed: std::sync::Mutex::new(std::collections::HashMap::new()),
            })
        }
    }

    fn parse_file(&self, p: &std::path::Path) -> Option<std::sync::Arc<LcpLens>> {
        if let Some(hit) = self.parsed.lock().unwrap().get(p).cloned() {
            return hit;
        }
        let lens = std::fs::read_to_string(p)
            .ok()
            .and_then(|t| parse_lcp(&t))
            .map(|mut l| {
                // merge the filename-derived names as aliases
                if let Some((_, names)) = self.files.iter().find(|(fp, _)| fp == p) {
                    for n in names {
                        if !l.names.iter().any(|x| x == n) {
                            l.names.push(n.clone());
                        }
                    }
                }
                std::sync::Arc::new(l)
            });
        self.parsed
            .lock()
            .unwrap()
            .insert(p.to_path_buf(), lens.clone());
        lens
    }

    fn tokens(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect()
    }

    /// score a candidate name-set against the EXIF lens string
    fn score(&self, names: &[String], q: &str, qsig: &[&String]) -> usize {
        let mut best = 0usize;
        for n in names {
            if n.starts_with("make:") || n.starts_with("ucm:") {
                continue;
            }
            let nn = norm(n);
            if nn.is_empty() {
                continue;
            }
            let score = if nn == q {
                usize::MAX
            } else if q.contains(&nn) || nn.contains(&q) {
                usize::MAX - 1
            } else {
                let nt = Self::tokens(n);
                let mut hit = 0usize;
                let mut digit_hit = false;
                for t in qsig {
                    if nt.iter().any(|u| *u == **t) {
                        hit += 1;
                        digit_hit |= t.chars().any(|c| c.is_ascii_digit());
                    }
                }
                if hit >= 2 && digit_hit {
                    hit
                } else {
                    continue;
                }
            };
            best = best.max(score);
        }
        best
    }

    /// fixed-lens camera fallback: files often carry no EXIF lens name
    /// (DJI drones/action cams, compacts). Match the profile's embedded
    /// UniqueCameraModel / pretty names against the EXIF camera model —
    /// bounded to same-make files so it can't grab an unrelated profile.
    fn find_by_camera(&self, exif_make: &str, exif_model: &str) -> Option<std::sync::Arc<LcpLens>> {
        let mq = norm(exif_model);
        if mq.len() < 3 {
            return None;
        }
        let make_q = norm(exif_make);
        let mut n = 0usize;
        for (p, _names) in &self.files {
            // same-make files only: either an explicit "make:" filename
            // alias, or the maker appearing in the filename itself —
            // without this the parse budget would burn on unrelated brands
            if !make_q.is_empty() {
                let gated = _names.iter().any(|nm| {
                    if let Some(mk) = nm.strip_prefix("make:") {
                        let fm = norm(mk);
                        !fm.is_empty()
                            && (fm == make_q || fm.contains(&make_q) || make_q.contains(&fm))
                    } else {
                        norm(nm).contains(&make_q)
                    }
                });
                if !gated {
                    continue;
                }
            }
            let Some(l) = self.parse_file(p) else {
                continue;
            };
            n += 1;
            if n > 24 {
                break;
            }
            // UniqueCameraModel / LensPrettyName contains the camera model
            // string for fixed-lens profiles ("DJI AC003 OsmoAction4")
            if l.names.iter().any(|nm| {
                let nn = norm(nm.trim_start_matches("ucm:"));
                nn.contains(&mq) || (!nn.is_empty() && mq.contains(&nn) && nn.len() >= 6)
            }) {
                return Some(l);
            }
        }
        None
    }

    /// best-matching lens: filename index narrows to a few candidates that
    /// get fully parsed on demand. `exif_make` gates the camera brand so a
    /// "45mm F2.8" Nikon-mount profile can't claim a Sony file.
    fn find(
        &self,
        exif_lens: &str,
        exif_make: &str,
        exif_model: &str,
    ) -> Option<std::sync::Arc<LcpLens>> {
        let q = norm(exif_lens);
        if q.len() < 3 {
            return self.find_by_camera(exif_make, exif_model);
        }
        let qt = Self::tokens(exif_lens);
        let qsig: Vec<&String> = qt
            .iter()
            .filter(|t| t.len() >= 3 || t.chars().any(|c| c.is_ascii_digit()))
            .collect();
        let make_q = norm(exif_make);
        // phase 1: score filename names — collect files at the top score
        let mut top = 0usize;
        let mut cand: Vec<&PathBuf> = Vec::new();
        for (p, names) in &self.files {
            // camera-make gate from the "make:" synthetic name
            if !make_q.is_empty() {
                if let Some(mk) = names.iter().find(|n| n.starts_with("make:")) {
                    let fm = norm(&mk[5..]);
                    if !fm.is_empty()
                        && fm != make_q
                        && !fm.contains(&make_q)
                        && !make_q.contains(&fm)
                    {
                        continue;
                    }
                }
            }
            let s = self.score(names, &q, &qsig);
            if s == 0 {
                continue;
            }
            if s > top {
                top = s;
                cand.clear();
            }
            if s == top && cand.len() < 8 {
                cand.push(p);
            }
        }
        // phase 2: parse candidates, rescore with embedded names
        let mut best: Option<(std::sync::Arc<LcpLens>, usize)> = None;
        for p in cand {
            if let Some(l) = self.parse_file(p) {
                let s = self.score(&l.names, &q, &qsig);
                if s > 0 && best.as_ref().map(|(_, bs)| s > *bs).unwrap_or(true) {
                    best = Some((l, s));
                }
            }
        }
        best.map(|(l, _)| l)
    }

    /// normalized focal (FocalLengthX ≈ f35mm/35); Adobe leaves it absent on
    /// many profiles — fall back to focal·SensorFormatFactor/35.
    fn foc_norm(cal: &Cal, lens: &LcpLens) -> [f32; 2] {
        if cal.foc[0] > 0.0 {
            cal.foc
        } else {
            let f = cal.focal.max(1.0) * lens.sensor_factor / 35.0;
            [f, f]
        }
    }

    pub fn correction(
        &self,
        exif_lens: &str,
        exif_make: &str,
        exif_model: &str,
        cam_crop: f32,
        focal: f32,
        aperture: f32,
    ) -> Option<Correction> {
        let lens = self.find(exif_lens, exif_make, exif_model)?;
        // nearest calibration by focal (entries of a prime share focal;
        // aperture only refines the vignette pick below)
        let mut by_focal: Vec<&Cal> = lens.cals.iter().collect();
        by_focal.sort_by(|a, b| (a.focal - focal).abs().total_cmp(&(b.focal - focal).abs()));
        let base = *by_focal.first()?;

        // vignette: nearest focal then nearest aperture
        let vig_cal = lens
            .cals
            .iter()
            .filter(|c| c.vig_a.is_some())
            .min_by(|a, b| {
                let da = (a.focal - focal).abs() + (a.aperture - aperture).abs() * 0.05;
                let db = (b.focal - focal).abs() + (b.aperture - aperture).abs() * 0.05;
                da.total_cmp(&db)
            });

        let foc = Self::foc_norm(base, &lens);
        let vig_foc = vig_cal
            .map(|c| {
                if c.vig_foc[0] > 0.0 {
                    c.vig_foc
                } else {
                    Self::foc_norm(c, &lens)
                }
            })
            .unwrap_or(foc);

        // crop-factor mismatch: the image pixels span differently vs the
        // calibration body. The Adobe normalization is pixel-based
        // (Dmax·foc), so a crop body shooting a FF-calibrated profile needs
        // its own Dmax — which happens automatically since we normalize by
        // THIS image's Dmax. Only the effective focal (FOV) shifts: the
        // calibration focal was specified on the cal body; on a crop body
        // the same physical focal gives a tighter FOV — handled by EXIF
        // focal matching, no extra scaling needed.
        let _ = cam_crop;

        let adobe = AdobeCor {
            foc,
            center: base.center,
            sf: 1.0, // RT ignores PerspectiveModel ScaleFactor (AutoScale path)
            k: base.dist.unwrap_or([0.0; 5]),
            chr: base.chr,
            chb: base.chb,
            vig_a: vig_cal.and_then(|c| c.vig_a).unwrap_or([0.0; 3]),
            vig_foc,
            vig_center: vig_cal.map(|c| c.vig_center).unwrap_or(base.center),
        };

        let mut c = Correction {
            model: if base.dist.is_some() { 3 } else { 0 },
            abc: [adobe.k[0], adobe.k[1], adobe.k[2]],
            tca: [1.0, 0.0, 1.0, 0.0],
            vig: adobe.vig_a,
            vig_adobe: true,
            scale: 1.0,
            lens_name: lens.names.first().cloned().unwrap_or_default(),
            adobe: Some(Box::new(adobe)),
            vig_foc: vig_foc[0],
            dng: None,
        };
        // fit R/B chromatic warps to v+b·r² for the GPU approx
        let fit = |m: Option<[f32; 6]>| -> (f32, f32) {
            let Some(m) = m else { return (1.0, 0.0) };
            let (sf, k1, k2, k3) = (m[0], m[1], m[2], m[3]);
            let (mut a11, mut a12, mut b1, mut b2, mut a22) =
                (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for i in 1..=5 {
                let r = i as f64 * 0.14; // focal-unit radii ≈ [0.14..0.7]
                let r2 = r * r;
                let w = sf as f64
                    * (1.0 + k1 as f64 * r2 + k2 as f64 * r2 * r2 + k3 as f64 * r2 * r2 * r2);
                a11 += 1.0;
                a12 += r2;
                a22 += r2 * r2;
                b1 += w;
                b2 += w * r2;
            }
            let det = a11 * a22 - a12 * a12;
            if det.abs() < 1e-9 {
                return (1.0, 0.0);
            }
            (
                ((b1 * a22 - a12 * b2) / det) as f32,
                ((a11 * b2 - a12 * b1) / det) as f32,
            )
        };
        let (vr, br) = fit(base.chr);
        let (vb, bb) = fit(base.chb);
        c.tca = [vr, br, vb, bb];
        eprintln!(
            "[lcp] '{}' → '{}' (f={} dist={:?} chr={} chb={} vig={:?} foc={:.3})",
            exif_lens,
            c.lens_name,
            base.focal,
            base.dist,
            base.chr.is_some(),
            base.chb.is_some(),
            vig_cal.and_then(|x| x.vig_a),
            vig_foc[0]
        );
        Some(c)
    }
}

fn lcp_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(d) = std::env::var("SAFELIGHT_LENSPROFILE_DIR") {
        out.push(PathBuf::from(d));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        out.push(PathBuf::from(home).join(".safelight/lensprofiles"));
    }
    #[cfg(target_os = "windows")]
    {
        out.push(PathBuf::from(
            r"C:\Program Files\Adobe\Adobe Lightroom Classic\Resources\LensProfiles\1.0",
        ));
        out.push(PathBuf::from(
            r"C:\ProgramData\Adobe\CameraRaw\LensProfiles\1.0",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        out.push(PathBuf::from(
            "/Applications/Adobe Lightroom Classic/Adobe Lightroom Classic.app/Contents/Resources/LensProfiles/1.0",
        ));
        out.push(PathBuf::from(
            "/Library/Application Support/Adobe/CameraRaw/LensProfiles/1.0",
        ));
    }
    out
}

/// shared lazily-loaded Adobe profile db
pub fn db() -> Option<&'static LcpDb> {
    use std::sync::OnceLock;
    static DB: OnceLock<Option<LcpDb>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut files = Vec::new();
        for d in lcp_dirs() {
            if let Some(db) = LcpDb::load_root(&d) {
                files.extend(db.files);
            }
        }
        if files.is_empty() {
            None
        } else {
            Some(LcpDb {
                files,
                parsed: std::sync::Mutex::new(std::collections::HashMap::new()),
            })
        }
    })
    .as_ref()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"
    xmlns:stCamera="http://ns.adobe.com/camera-raw-saved-lens-preset/1.0/">
   <photoshop:CameraProfiles>
    <rdf:Seq>
     <rdf:li>
      <rdf:Description
       stCamera:Author="Adobe" stCamera:Make="SONY"
       stCamera:Lens="E 20mm F2.8" stCamera:LensPrettyName="Sony E 20mm F2.8"
       stCamera:SensorFormatFactor="1.5" stCamera:FocalLength="20"
       stCamera:FocusDistance="3" stCamera:ApertureValue="4">
      <stCamera:AlternateLensNames><rdf:Seq>
        <rdf:li>Sony SEL20F28</rdf:li>
      </rdf:Seq></stCamera:AlternateLensNames>
      <stCamera:PerspectiveModel>
       <rdf:Description
        stCamera:Version="2" stCamera:ScaleFactor="1.019"
        stCamera:RadialDistortParam1="-0.143" stCamera:RadialDistortParam2="0.115"
        stCamera:RadialDistortParam3="0.02" stCamera:FocalLengthX="0.857"
        stCamera:ImageXCenter="0.5">
       <stCamera:ChromaticRedGreenModel
        stCamera:ScaleFactor="1.0002" stCamera:RadialDistortParam1="0.001"/>
       <stCamera:VignetteModel
        stCamera:VignetteModelParam1="-1.33" stCamera:VignetteModelParam2="0.97"
        stCamera:VignetteModelParam3="-0.45" stCamera:FocalLengthX="0.857"/>
       </rdf:Description>
      </stCamera:PerspectiveModel>
      </rdf:Description>
     </rdf:li>
    </rdf:Seq>
   </photoshop:CameraProfiles>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>"#;

    #[test]
    fn parses_calibration_entry() {
        let l = parse_lcp(SAMPLE).expect("sample parses");
        assert!(l.names.iter().any(|n| n.contains("20mm")));
        assert!((l.sensor_factor - 1.5).abs() < 1e-4);
        assert_eq!(l.cals.len(), 1);
        let c = &l.cals[0];
        let k = c.dist.unwrap();
        assert!((k[0] + 0.143).abs() < 1e-4);
        assert!((k[1] - 0.115).abs() < 1e-4);
        assert!(c.vig_a.is_some());
        assert!(c.chr.is_some());
    }

    #[test]
    fn nested_li_names_and_no_false_split() {
        // the alias <rdf:li> inside must not split the block in two
        let l = parse_lcp(SAMPLE).unwrap();
        assert_eq!(l.cals.len(), 1);
        assert!(l.names.iter().any(|n| n.contains("SEL20F28")));
    }
}

#[cfg(test)]
mod dji_tests {
    #[test]
    fn dji_action4_model_fallback() {
        let Some(db) = crate::lcp::db() else { return };
        // DJI DNG carries no EXIF lens — match on the camera model
        let c = db.correction("", "DJI", "AC003", 1.0, 2.34, 2.8);
        let c = c.expect("DJI AC003 should match OSMO ACTION 4 profile");
        eprintln!("matched '{}' model={} k={:?}", c.lens_name, c.model, c.abc);
        assert!(c.model == 3, "should have ACM distortion");
    }
}
