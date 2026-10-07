//! Lens correction profiles parsed from a vendored lensfun database.
//!
//! The XML files live in `lensfun/db` at the repo root (CC-BY-SA data,
//! attribution in README). Parsing is read-only; we implement the three
//! calibration models ourselves: ptlens/poly3 distortion, poly3 TCA,
//! and the "pa" vignetting model — same math lensfun applies.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug)]
pub enum Distortion {
    /// ru = rd * (a rd^3 + b rd^2 + c rd + (1-a-b-c))
    Ptlens { a: f32, b: f32, c: f32 },
    /// ru = rd * (1 + k rd^2)
    Poly3 { k1: f32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Tca {
    /// red scale: ru = rd * (vr + br rd^2)
    pub vr: f32,
    pub br: f32,
    /// blue scale: ru = rd * (vb + bb rd^2)
    pub vb: f32,
    pub bb: f32,
}

#[derive(Clone, Copy, Debug)]
struct DistCal {
    focal: f32,
    kind: Distortion,
}

#[derive(Clone, Copy, Debug)]
struct TcaCal {
    focal: f32,
    t: Tca,
}

#[derive(Clone, Copy, Debug)]
struct VigCal {
    focal: f32,
    aperture: f32,
    k: [f32; 3],
}

#[derive(Debug)]
struct LensEntry {
    names: Vec<String>,
    mount: String,
    focal_min: f32,
    focal_max: f32,
    crop: f32,
    dists: Vec<DistCal>,
    tcas: Vec<TcaCal>,
    vigs: Vec<VigCal>,
}

#[derive(Debug)]
struct CamEntry {
    #[allow(dead_code)]
    mount: String,
    crop: f32,
    names: Vec<String>,
}

/// Everything a develop call needs — plain floats, shader-ready.
#[derive(Clone, Debug)]
pub struct Correction {
    /// 0 none, 1 ptlens, 2 poly3 (uniform-side model id)
    pub model: u32,
    /// ptlens (a,b,c) or poly3 (k1,0,0)
    pub abc: [f32; 3],
    /// TCA (vr,br) for red, (vb,bb) for blue; [1,0,1,0] = none
    pub tca: [f32; 4],
    /// vignetting pa coeffs k1,k2,k3; [0,0,0] = none
    pub vig: [f32; 3],
    /// radius scaling camera-crop / lens-crop
    pub scale: f32,
    /// matched lens display name ("" = no match)
    pub lens_name: String,
}

impl Correction {
    pub fn is_empty(&self) -> bool {
        self.model == 0 && self.tca == [1.0, 0.0, 1.0, 0.0] && self.vig == [0.0; 3]
    }
}

pub struct LensDb {
    lenses: Vec<LensEntry>,
    cams: Vec<CamEntry>,
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn attr<'a>(n: &'a roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    n.attribute(name)
}
fn attrf(n: &roxmltree::Node, name: &str) -> Option<f32> {
    n.attribute(name).and_then(|v| v.parse().ok())
}
fn child_text(n: &roxmltree::Node, name: &str) -> Option<String> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
        .and_then(|c| c.text())
        .map(|s| s.trim().to_string())
}

impl LensDb {
    pub fn load(dir: &Path) -> Option<LensDb> {
        let mut lenses = Vec::new();
        let mut cams = Vec::new();
        let rd = std::fs::read_dir(dir).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("xml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(doc) = roxmltree::Document::parse(&text) else {
                continue;
            };
            for el in doc.root_element().children().filter(|c| c.is_element()) {
                match el.tag_name().name() {
                    "camera" => {
                        let mount = child_text(&el, "mount").unwrap_or_default();
                        let crop = child_text(&el, "cropfactor")
                            .and_then(|s| s.parse().ok())
                            .unwrap_or(1.0);
                        let mut names = Vec::new();
                        for m in el.children().filter(|c| {
                            c.is_element() && c.tag_name().name() == "model"
                        }) {
                            if let Some(t) = m.text() {
                                names.push(t.trim().to_string());
                            }
                        }
                        if !names.is_empty() {
                            cams.push(CamEntry { mount, crop, names });
                        }
                    }
                    "lens" => {
                        let mut names = Vec::new();
                        let mut mount = String::new();
                        let mut crop = 1.0f32;
                        let (mut fmin, mut fmax) = (0.0f32, f32::MAX);
                        let mut dists = Vec::new();
                        let mut tcas = Vec::new();
                        let mut vigs = Vec::new();
                        for c in el.children().filter(|c| c.is_element()) {
                            match c.tag_name().name() {
                                "model" => {
                                    if let Some(t) = c.text() {
                                        names.push(t.trim().to_string());
                                    }
                                }
                                "mount" => {
                                    mount =
                                        c.text().unwrap_or_default().trim().to_string();
                                }
                                "cropfactor" => {
                                    if let Some(t) = c.text() {
                                        crop = t.trim().parse().unwrap_or(1.0);
                                    }
                                }
                                "focal" => {
                                    if let Some(v) = attrf(&c, "min") {
                                        fmin = v;
                                    }
                                    if let Some(v) = attrf(&c, "max") {
                                        fmax = v;
                                    }
                                }
                                "calibration" => {
                                    for k in c.children().filter(|c| c.is_element()) {
                                        let focal = attrf(&k, "focal").unwrap_or(0.0);
                                        match k.tag_name().name() {
                                            "distortion" => {
                                                let kind = match attr(&k, "model") {
                                                    Some("ptlens") => Some(
                                                        Distortion::Ptlens {
                                                            a: attrf(&k, "a")
                                                                .unwrap_or(0.0),
                                                            b: attrf(&k, "b")
                                                                .unwrap_or(0.0),
                                                            c: attrf(&k, "c")
                                                                .unwrap_or(0.0),
                                                        },
                                                    ),
                                                    Some("poly3") => Some(
                                                        Distortion::Poly3 {
                                                            k1: attrf(&k, "k1")
                                                                .unwrap_or(0.0),
                                                        },
                                                    ),
                                                    _ => None,
                                                };
                                                if let Some(kd) = kind {
                                                    dists.push(DistCal {
                                                        focal,
                                                        kind: kd,
                                                    });
                                                }
                                            }
                                            "tca" => {
                                                tcas.push(TcaCal {
                                                    focal,
                                                    t: Tca {
                                                        vr: attrf(&k, "vr")
                                                            .unwrap_or(1.0),
                                                        br: attrf(&k, "br")
                                                            .unwrap_or(0.0),
                                                        vb: attrf(&k, "vb")
                                                            .unwrap_or(1.0),
                                                        bb: attrf(&k, "bb")
                                                            .unwrap_or(0.0),
                                                    },
                                                });
                                            }
                                            "vignetting" => {
                                                if attr(&k, "model") == Some("pa")
                                                {
                                                    vigs.push(VigCal {
                                                        focal,
                                                        aperture: attrf(
                                                            &k, "aperture",
                                                        )
                                                        .unwrap_or(0.0),
                                                        k: [
                                                            attrf(&k, "k1")
                                                                .unwrap_or(0.0),
                                                            attrf(&k, "k2")
                                                                .unwrap_or(0.0),
                                                            attrf(&k, "k3")
                                                                .unwrap_or(0.0),
                                                        ],
                                                    });
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        if !names.is_empty() {
                            lenses.push(LensEntry {
                                names,
                                mount,
                                focal_min: fmin,
                                focal_max: fmax,
                                crop,
                                dists,
                                tcas,
                                vigs,
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        if lenses.is_empty() {
            None
        } else {
            Some(LensDb { lenses, cams })
        }
    }

    /// EXIF tokens: alnum splits, with f-number tokens joined
    /// ("F2.0" / "f/2.0" / "F2 0" -> "f20", "f/2.8" -> "f28").
    fn tokens(s: &str) -> Vec<String> {
        let raw: Vec<String> = s
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_lowercase())
            .collect();
        let mut out: Vec<String> = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let t = &raw[i];
            // merge "f" + digit chain and "f<d>" + digits => aperture token
            if t == "f" && i + 1 < raw.len() && raw[i + 1].chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                let mut j = i + 1;
                let mut a = String::from("f");
                while j < raw.len() && raw[j].chars().all(|c| c.is_ascii_digit()) && !raw[j].is_empty() {
                    a.push_str(&raw[j]);
                    j += 1;
                }
                out.push(a);
                i = j;
                continue;
            }
            if t.len() > 1
                && t.starts_with('f')
                && t[1..].chars().all(|c| c.is_ascii_digit())
                && i + 1 < raw.len()
                && raw[i + 1].chars().all(|c| c.is_ascii_digit())
                && !raw[i + 1].is_empty()
            {
                out.push(format!("{}{}", t, raw[i + 1]));
                i += 2;
                continue;
            }
            out.push(t.clone());
            i += 1;
        }
        out
    }

    fn find_lens(&self, exif_lens: &str) -> Option<&LensEntry> {
        let q = norm(exif_lens);
        if q.len() < 4 {
            return None;
        }
        let qt = Self::tokens(exif_lens);
        // significant query tokens: carry a digit or are reasonably long
        let qsig: Vec<&String> = qt
            .iter()
            .filter(|t| t.len() >= 3 || t.chars().any(|c| c.is_ascii_digit()))
            .collect();
        let mut best: Option<(&LensEntry, usize)> = None;
        for l in &self.lenses {
            for n in &l.names {
                let nn = norm(n);
                if nn.is_empty() {
                    continue;
                }
                let score = if nn == q {
                    usize::MAX
                } else if q.contains(&nn) || nn.contains(&q) {
                    usize::MAX - 1
                } else {
                    // token scoring: shared significant tokens; at least
                    // one must carry a digit (model number / focal) so
                    // generic pairs like "Mark III" can't false-match
                    let nt = Self::tokens(n);
                    let mut hit = 0usize;
                    let mut digit_hit = false;
                    for t in &qsig {
                        if nt.iter().any(|u| *u == **t) {
                            hit += 1;
                            digit_hit |= t.chars().any(|c| c.is_ascii_digit());
                        }
                    }
                    if hit >= 2 && digit_hit { hit } else { continue; }
                };
                if best.map(|(_, s)| score > s).unwrap_or(true) {
                    best = Some((l, score));
                }
            }
        }
        best.map(|(l, _)| l)
    }

    fn cam_entry(&self, make: &str, model: &str) -> Option<&CamEntry> {
        let q = norm(&format!("{} {}", make, model));
        let qm = norm(model);
        for c in &self.cams {
            for n in &c.names {
                let nn = norm(n);
                if !nn.is_empty() && (nn == qm || q.contains(&nn) || nn.contains(&qm)) {
                    return Some(c);
                }
            }
        }
        None
    }

    fn cam_crop(&self, make: &str, model: &str) -> Option<f32> {
        self.cam_entry(make, model).map(|c| c.crop)
    }

    /// fixed-lens compacts carry no EXIF lens name — resolve via the
    /// camera's mount to the matching lens entry. Only fires on
    /// proprietary pseudo-mounts (e.g. "panasonicFZ150"), never on real
    /// interchangeable mounts shared by hundreds of lenses.
    fn fixed_lens(&self, make: &str, model: &str) -> Option<&LensEntry> {
        let cam = self.cam_entry(make, model)?;
        const SYSTEM_MOUNTS: &[&str] = &[
            "nikonf", "canonef", "canonefs", "pentaxk", "sonya", "sonye",
            "fujifilmx", "leicam", "olympuse", "m43", "fourthirds",
            "m42", "m39", "t2", "dkl", "generic", "tamronadaptall",
            "nikonz", "canonrf", "canonfd", "minoltaa", "konicaminoltaa",
            "pentax645", "pentaxq", "samsungnx", "sigmasa", "lmount",
            "hasselbladv", "hasselbladx", "fujifilmg", "contax",
            "mamiyazd", "ricohgxr",
        ];
        let m = norm(&cam.mount);
        if SYSTEM_MOUNTS.contains(&m.as_str()) {
            return None;
        }
        // require the mount to carry a model token (digits count), e.g.
        // "fz150" inside "panasonicFZ150" vs camera "DMC-FZ150"
        let model_tokens: Vec<String> = model
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.chars().any(|c| c.is_ascii_digit()))
            .map(|t| norm(t))
            .collect();
        let tok_ok = model_tokens.iter().any(|t| t.len() >= 3 && m.contains(t))
            || m.contains(&norm(model));
        if !tok_ok {
            return None;
        }
        let _ = make;
        self.lenses.iter().find(|l| l.mount == cam.mount)
    }

    /// Resolve a full correction for an image's EXIF data.
    pub fn correction(
        &self,
        exif_lens: &str,
        make: &str,
        model: &str,
        focal: f32,
        aperture: f32,
    ) -> Correction {
        let none = Correction {
            model: 0,
            abc: [0.0; 3],
            tca: [1.0, 0.0, 1.0, 0.0],
            vig: [0.0; 3],
            scale: 1.0,
            lens_name: String::new(),
        };
        // NOTE: never free-text match the *camera model* as a lens —
        // generic tokens ("mark", "iii") false-match unrelated lenses.
        // Empty EXIF lens is handled by fixed_lens() below.
        let lens = self
            .find_lens(exif_lens)
            .or_else(|| self.fixed_lens(make, model));
        let Some(lens) = lens else {
            return none;
        };
        self.build(lens, make, model, focal, aperture)
    }

    fn build(
        &self,
        lens: &LensEntry,
        make: &str,
        model: &str,
        focal: f32,
        aperture: f32,
    ) -> Correction {
        let cc = self.cam_crop(make, model).unwrap_or(lens.crop);
        let scale = if lens.crop > 0.0 { cc / lens.crop } else { 1.0 };
        let f = focal.clamp(0.1, 500.0);
        let name = lens.names.first().cloned().unwrap_or_default();

        // distortion: lerp bracketing focals
        let mut d: Vec<DistCal> = lens
            .dists
            .iter()
            .copied()
            .filter(|d| d.focal > 0.0)
            .collect();
        d.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap());
        let dist = if d.is_empty() || f < lens.focal_min - 0.5 || f > lens.focal_max + 0.5 {
            None
        } else {
            Some(lerp_dist(&d, f))
        };

        // TCA: lerp bracketing focals
        let mut t: Vec<TcaCal> = lens
            .tcas
            .iter()
            .copied()
            .filter(|t| t.focal > 0.0)
            .collect();
        t.sort_by(|a, b| a.focal.partial_cmp(&b.focal).unwrap());
        let tca = if t.is_empty() {
            [1.0, 0.0, 1.0, 0.0]
        } else {
            lerp_tca(&t, f)
        };

        // vignetting: nearest focal, nearest aperture within it
        let vig = if lens.vigs.is_empty() {
            [0.0; 3]
        } else {
            let mut vs: Vec<VigCal> = lens.vigs.clone();
            vs.sort_by(|a, b| {
                (a.focal - f)
                    .abs()
                    .partial_cmp(&(b.focal - f).abs())
                    .unwrap()
            });
            let f0 = vs[0].focal;
            let mut near: Vec<VigCal> =
                vs.iter().copied().filter(|v| v.focal == f0).collect();
            near.sort_by(|a, b| {
                (a.aperture - aperture)
                    .abs()
                    .partial_cmp(&(b.aperture - aperture).abs())
                    .unwrap()
            });
            near[0].k
        };

        Correction {
            model: match dist {
                Some(Distortion::Ptlens { .. }) => 1,
                Some(Distortion::Poly3 { .. }) => 2,
                None => 0,
            },
            abc: match dist {
                Some(Distortion::Ptlens { a, b, c }) => [a, b, c],
                Some(Distortion::Poly3 { k1 }) => [k1, 0.0, 0.0],
                None => [0.0; 3],
            },
            tca,
            vig,
            scale,
            lens_name: name,
        }
    }
}

fn lerp_dist(d: &[DistCal], f: f32) -> Distortion {
    if d.len() == 1 || f <= d[0].focal {
        return d[0].kind;
    }
    let last = d[d.len() - 1];
    if f >= last.focal {
        return last.kind;
    }
    for w in d.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        if f >= lo.focal && f <= hi.focal {
            let t = if hi.focal > lo.focal {
                (f - lo.focal) / (hi.focal - lo.focal)
            } else {
                0.0
            };
            return match (lo.kind, hi.kind) {
                (
                    Distortion::Ptlens { a: a0, b: b0, c: c0 },
                    Distortion::Ptlens { a: a1, b: b1, c: c1 },
                ) => Distortion::Ptlens {
                    a: a0 + t * (a1 - a0),
                    b: b0 + t * (b1 - b0),
                    c: c0 + t * (c1 - c0),
                },
                (Distortion::Poly3 { k1: k0 }, Distortion::Poly3 { k1: k1_ }) => {
                    Distortion::Poly3 { k1: k0 + t * (k1_ - k0) }
                }
                // mixed models: use nearer
                (a, b) => {
                    if t < 0.5 {
                        a
                    } else {
                        b
                    }
                }
            };
        }
    }
    d[0].kind
}

fn lerp_tca(t: &[TcaCal], f: f32) -> [f32; 4] {
    let one = t[0].t;
    if t.len() == 1 || f <= t[0].focal {
        return [one.vr, one.br, one.vb, one.bb];
    }
    let last = t[t.len() - 1].t;
    if f >= t[t.len() - 1].focal {
        return [last.vr, last.br, last.vb, last.bb];
    }
    for w in t.windows(2) {
        let (lo, hi) = (w[0], w[1]);
        if f >= lo.focal && f <= hi.focal {
            let s = if hi.focal > lo.focal {
                (f - lo.focal) / (hi.focal - lo.focal)
            } else {
                0.0
            };
            return [
                lo.t.vr + s * (hi.t.vr - lo.t.vr),
                lo.t.br + s * (hi.t.br - lo.t.br),
                lo.t.vb + s * (hi.t.vb - lo.t.vb),
                lo.t.bb + s * (hi.t.bb - lo.t.bb),
            ];
        }
    }
    [one.vr, one.br, one.vb, one.bb]
}

static DB: OnceLock<Option<LensDb>> = OnceLock::new();

fn db_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ARA_LENSFUN_DB") {
        let p = PathBuf::from(d);
        if p.is_dir() {
            return Some(p);
        }
    }
    // candidates relative to the executable (app bundle) and the repo
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            for cand in [
                bin.join("../Resources/lensfun/db"),
                bin.join("lensfun/db"),
                bin.join("../../lensfun/db"),
                bin.join("../../../lensfun/db"),
            ] {
                if cand.is_dir() {
                    return Some(cand);
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let p = cwd.join("lensfun/db");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

/// Lazily-parsed global database (None when the DB isn't installed).
pub fn db() -> Option<&'static LensDb> {
    DB.get_or_init(|| db_dir().and_then(|d| LensDb::load(&d)))
        .as_ref()
}

/// Look up the lens a file would use (for UI display). Returns the
/// matched lens name or None.
pub fn match_name(
    exif_lens: &str,
    make: &str,
    model: &str,
    focal: f32,
    aperture: f32,
) -> Option<String> {
    let c = db()?.correction(exif_lens, make, model, focal, aperture);
    if c.is_empty() {
        None
    } else {
        Some(c.lens_name)
    }
}
