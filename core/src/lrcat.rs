//! Lightroom Classic catalog (.lrcat) import.
//!
//! LR stores each image's develop settings in
//! `Adobe_imageDevelopSettings.text` as a Lua table (`s = { ... }`) whose
//! keys are the same crs:* vocabulary as XMP sidecars. We parse the Lua
//! subset, re-serialise it as an XMP document, and feed it through the
//! existing `crs_to_recipe` — so masks, point colours, profiles, local
//! corrections and lens settings all ride the code path presets already
//! use. Ratings, pick flags and colour labels come from `Adobe_images`.
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::recipe::Sidecar;
use crate::xmp::crs_to_recipe;

/// One catalog image + its imported sidecar.
pub struct LrAsset {
    pub path: PathBuf,
    pub sidecar: Sidecar,
    /// the Lua settings parsed into a non-default recipe (masks, nonzero
    /// sliders, crop, profile, …)
    pub has_edits: bool,
}

/// List every image row joined with its develop settings.
pub fn entries(cat: &Path) -> rusqlite::Result<Vec<LrAsset>> {
    let db = Connection::open(cat)?;
    let mut st = db.prepare(
        "SELECT rf.absolutePath, fo.pathFromRoot, \
                COALESCE(f.idx_filename, f.baseName || '.' || f.extension), \
                i.rating, i.pick, COALESCE(i.colorLabels,''), COALESCE(ds.text,'') \
         FROM Adobe_images i \
         JOIN AgLibraryFile f ON i.rootFile = f.id_local \
         JOIN AgLibraryFolder fo ON f.folder = fo.id_local \
         JOIN AgLibraryRootFolder rf ON fo.rootFolder = rf.id_local \
         LEFT JOIN Adobe_imageDevelopSettings ds ON ds.image = i.id_local",
    )?;
    let mut out = Vec::new();
    let mut rows = st.query([])?;
    while let Some(r) = rows.next()? {
        let root: String = r.get(0)?;
        let sub: String = r.get(1)?;
        let name: String = r.get(2)?;
        let text: String = r.get(6)?;
        let mut sc = Sidecar::default();
        sc.rating = r.get::<_, i64>(3).unwrap_or(0) as i32;
        sc.flag = r.get::<_, i64>(4).unwrap_or(0) as i32;
        sc.label = r.get(5)?;
        // a blob that parses to the same recipe as an empty blob means the
        // image was imported but never edited — still carries rating etc.
        let mut has_edits = false;
        if text.contains('=') {
            let xmp = lua_to_xmp(&text);
            sc.recipe = crs_to_recipe(&xmp);
            has_edits = sc.recipe.to_json() != crs_to_recipe("s = {}").to_json();
        }
        out.push(LrAsset {
            path: Path::new(&root).join(&sub).join(&name),
            sidecar: sc,
            has_edits,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Lua settings blob -> XMP document
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Lua {
    Num(f64),
    Str(String),
    Bool(bool),
    // (key, value) pairs; key "" = positional array element
    Table(Vec<(String, Lua)>),
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}
impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                b'-' if self.s.get(self.i + 1) == Some(&b'-') => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn peek(&mut self) -> u8 {
        self.ws();
        *self.s.get(self.i).unwrap_or(&0)
    }
    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == b {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> String {
        self.ws();
        let st = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_')
        {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.i]).into_owned()
    }
    fn string(&mut self) -> Option<String> {
        if self.peek() != b'"' {
            return None;
        }
        self.i += 1;
        let mut out = String::new();
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let c = *self.s.get(self.i)?;
                    out.push(match c {
                        b'n' => '\n',
                        b't' => '\t',
                        _ => c as char,
                    });
                    self.i += 1;
                }
                _ => {
                    // keep utf8 bytes verbatim
                    let st = self.i;
                    while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\'
                    {
                        self.i += 1;
                    }
                    out.push_str(&String::from_utf8_lossy(&self.s[st..self.i]));
                }
            }
        }
        Some(out)
    }
    fn number(&mut self) -> Option<f64> {
        self.ws();
        let st = self.i;
        while self.i < self.s.len()
            && matches!(
                self.s[self.i],
                b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        if self.i == st {
            return None;
        }
        std::str::from_utf8(&self.s[st..self.i]).ok()?.parse().ok()
    }
    fn value(&mut self) -> Option<Lua> {
        match self.peek() {
            b'{' => self.table(),
            b'"' => self.string().map(Lua::Str),
            b't' | b'f' => {
                let id = self.ident();
                match id.as_str() {
                    "true" => Some(Lua::Bool(true)),
                    "false" => Some(Lua::Bool(false)),
                    _ => None,
                }
            }
            b'n' => {
                // nil
                self.ident();
                Some(Lua::Num(f64::NAN))
            }
            _ => self.number().map(Lua::Num),
        }
    }
    fn table(&mut self) -> Option<Lua> {
        if !self.eat(b'{') {
            return None;
        }
        let mut items = Vec::new();
        let mut idx = 1usize;
        loop {
            self.ws();
            if self.eat(b'}') {
                break;
            }
            if self.peek() == 0 {
                break;
            }
            // key forms: name = v | ["name"] = v | [n] = v | v
            let save = self.i;
            let key;
            if self.peek() == b'[' {
                self.i += 1;
                if self.peek() == b'"' {
                    key = self.string().unwrap_or_default();
                } else {
                    key = self.ident();
                }
                self.eat(b']');
                self.eat(b'=');
            } else {
                let id = self.ident();
                if self.eat(b'=') {
                    key = id;
                } else {
                    self.i = save;
                    key = idx.to_string();
                    idx += 1;
                }
            }
            let v = self.value()?;
            items.push((key, v));
            self.eat(b',');
        }
        Some(Lua::Table(items))
    }
}

fn parse_lua(text: &str) -> Option<Lua> {
    // find the first `= {` and parse the table value
    let i = text.find('=')? + 1;
    let mut p = P {
        s: text[i..].as_bytes(),
        i: 0,
    };
    p.table()
}

fn fmt_num(v: f64) -> String {
    if v.is_nan() {
        return "0".into();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{:.6}", v)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .into()
    }
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

/// scalar k=v entries of a table as ` crs:K="v"` attributes.
fn attrs(items: &[(String, Lua)], skip: &[&str]) -> String {
    let mut out = String::new();
    for (k, v) in items {
        if k.is_empty()
            || k.chars().next().is_some_and(|c| c.is_ascii_digit())
            || skip.contains(&k.as_str())
        {
            continue;
        }
        match v {
            Lua::Num(n) => out += &format!(" crs:{k}=\"{}\"", fmt_num(*n)),
            Lua::Str(s) => out += &format!(" crs:{k}=\"{}\"", esc(s)),
            Lua::Bool(b) => out += &format!(" crs:{k}=\"{b}\""),
            Lua::Table(_) => {}
        }
    }
    out
}

/// `{n, n, n, ...}` → comma-joined number list
fn num_row(t: &Lua) -> Option<String> {
    let Lua::Table(items) = t else { return None };
    let mut v = Vec::new();
    for (_, x) in items {
        if let Lua::Num(n) = x {
            v.push(fmt_num(*n));
        } else {
            return None;
        }
    }
    Some(v.join(" "))
}

/// `<crs:K><rdf:Seq><rdf:li>row</rdf:li>…</rdf:Seq></crs:K>` from an array of
/// number-rows (PointColors-style) or an array of scalars (variance).
fn seq_of_rows(items: &[(String, Lua)]) -> String {
    let mut s = String::from("<rdf:Seq>");
    for (_, it) in items {
        match it {
            Lua::Table(_) => {
                if let Some(r) = num_row(it) {
                    s += &format!("<rdf:li>{r}</rdf:li>");
                }
            }
            Lua::Num(n) => s += &format!("<rdf:li>{}</rdf:li>", fmt_num(*n)),
            _ => {}
        }
    }
    s + "</rdf:Seq>"
}

/// array of numbers as `<rdf:Seq><rdf:li>x, y</rdf:li>` point pairs
fn curve_seq(items: &[(String, Lua)]) -> String {
    let nums: Vec<f64> = items
        .iter()
        .filter_map(|(_, v)| match v {
            Lua::Num(n) => Some(*n),
            _ => None,
        })
        .collect();
    let mut s = String::from("<rdf:Seq>");
    for p in nums.chunks(2) {
        if p.len() == 2 {
            s += &format!("<rdf:li>{}, {}</rdf:li>", fmt_num(p[0]), fmt_num(p[1]));
        }
    }
    s + "</rdf:Seq>"
}

/// nested table that belongs inside a mask rdf:li (Gesture/Dabs/RangeMask).
fn mask_elem(k: &str, t: &[(String, Lua)]) -> Option<String> {
    match k {
        "Gesture" => {
            // Gesture = { { Dabs = {"r r","d x y",…} }, … }
            let mut dabs = Vec::new();
            let mut radii = Vec::new();
            for (_, g) in t {
                let Lua::Table(gi) = g else { continue };
                for (gk, gv) in gi {
                    if gk != "Dabs" {
                        continue;
                    }
                    if let Lua::Table(ds) = gv {
                        for (_, d) in ds {
                            let Lua::Str(d) = d else { continue };
                            let mut it = d.split_whitespace();
                            match it.next() {
                                Some("d") => {
                                    let x = it.next().unwrap_or("0");
                                    let y = it.next().unwrap_or("0");
                                    dabs.push(format!("<rdf:li>{x} {y}</rdf:li>"));
                                }
                                Some("r") => {
                                    if let Some(v) = it.next().and_then(|v| v.parse::<f64>().ok()) {
                                        radii.push(v);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            let mut s = String::new();
            if !radii.is_empty() {
                let mean = radii.iter().sum::<f64>() / radii.len() as f64;
                // parser reads the brush radius off the mask attrs — the
                // caller can't see this, so stash it as a Dabs sibling
                s += &format!(" crs:Radius=\"{:.6}\"", mean);
            }
            if !dabs.is_empty() {
                s += "<crs:Dabs><rdf:Seq>";
                s += &dabs.concat();
                s += "</rdf:Seq></crs:Dabs>";
            }
            Some(s)
        }
        "RangeMask" => {
            let mut inner = attrs(t, &["PointModels"]);
            for (k, v) in t {
                if k == "PointModels" {
                    if let Lua::Table(rows) = v {
                        inner += "<crs:PointModels>";
                        inner += &seq_of_rows(rows);
                        inner += "</crs:PointModels>";
                    }
                }
            }
            Some(format!(
                "<crs:CorrectionRangeMask><rdf:Description {inner}/></crs:CorrectionRangeMask>"
            ))
        }
        _ => None,
    }
}

/// one mask table → `<rdf:li><rdf:Description attrs> elems </rdf:Description></rdf:li>`
fn mask_li(t: &[(String, Lua)]) -> String {
    let mut a = attrs(t, &["Gesture", "RangeMask"]);
    let mut elems = String::new();
    for (k, v) in t {
        if let Lua::Table(items) = v {
            if let Some(e) = mask_elem(k, items) {
                if k == "Gesture" {
                    // Radius attr part goes on the Description, Dabs is an element
                    if let Some(p) = e.find("<crs:Dabs") {
                        a += &e[..p];
                        elems += &e[p..];
                    } else {
                        a += &e;
                    }
                } else {
                    elems += &e;
                }
            }
        }
    }
    format!("<rdf:li><rdf:Description{a}>{elems}</rdf:Description></rdf:li>")
}

/// correction item → `<rdf:li><rdf:Description attrs> elems </rdf:Description></rdf:li>`
fn corr_li(t: &[(String, Lua)]) -> String {
    let skip = [
        "CorrectionMasks",
        "Masks",
        "LocalPointColors",
        "LocalColorVariance",
    ];
    let a = attrs(t, &skip);
    let mut elems = String::new();
    for (k, v) in t {
        let Lua::Table(items) = v else { continue };
        match k.as_str() {
            "CorrectionMasks" | "Masks" => {
                elems += "<crs:CorrectionMasks><rdf:Seq>";
                for (_, m) in items {
                    if let Lua::Table(mi) = m {
                        elems += &mask_li(mi);
                    }
                }
                elems += "</rdf:Seq></crs:CorrectionMasks>";
            }
            "LocalPointColors" | "LocalColorVariance" => {
                elems += &format!("<crs:{k}>");
                elems += &seq_of_rows(items);
                elems += &format!("</crs:{k}>");
            }
            _ => {}
        }
    }
    // LR writes What="Correction" on the li — emit it for symmetry
    format!("<rdf:li><rdf:Description{a}>{elems}</rdf:Description></rdf:li>")
}

/// first matching key's numeric value
fn get_num(t: &[(String, Lua)], keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| {
        t.iter().find_map(|(ik, v)| {
            if ik == k {
                if let Lua::Num(n) = v {
                    Some(*n)
                } else {
                    None
                }
            } else {
                None
            }
        })
    })
}
fn get_str<'a>(t: &'a [(String, Lua)], keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| {
        t.iter().find_map(|(ik, v)| {
            if ik == k {
                if let Lua::Str(s) = v {
                    Some(s.as_str())
                } else {
                    None
                }
            } else {
                None
            }
        })
    })
}

/// RetouchAreas item → the flat `<rdf:li crs:What="Retouch" crs:SpotX …>`
/// form our XMP reader understands.  The catalog stores the spot inside a
/// `Masks` sub-table (ellipse or paint dabs) plus lowercase duplicates of
/// center/radius/source at the item top level.
fn retouch_li(t: &[(String, Lua)]) -> String {
    let mut sx = get_num(t, &["centerX", "SpotX"]);
    let mut sy = get_num(t, &["centerY", "SpotY"]);
    let mut rad = get_num(t, &["radius"]);
    for (k, v) in t {
        if k != "Masks" {
            continue;
        }
        let Lua::Table(ms) = v else { continue };
        for (_, m) in ms {
            let Lua::Table(mi) = m else { continue };
            match get_str(mi, &["What"]) {
                Some("Mask/Ellipse") => {
                    if sx.is_none() {
                        sx = get_num(mi, &["X"]);
                        sy = get_num(mi, &["Y"]);
                    }
                    if rad.is_none() {
                        rad = get_num(mi, &["SizeX"]);
                    }
                }
                Some("Mask/Paint") => {
                    if rad.is_none() {
                        rad = get_num(mi, &["Radius"]);
                    }
                    if sx.is_none() {
                        // centroid of the dab centers
                        let mut n = 0;
                        let (mut ax, mut ay) = (0.0, 0.0);
                        for (mk, mv) in mi {
                            if mk != "Dabs" {
                                continue;
                            }
                            let Lua::Table(ds) = mv else { continue };
                            for (_, d) in ds {
                                let Lua::Str(d) = d else { continue };
                                let mut it = d.split_whitespace();
                                if it.next() != Some("d") {
                                    continue;
                                }
                                let x: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                                let y: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
                                ax += x;
                                ay += y;
                                n += 1;
                            }
                        }
                        if n > 0 {
                            sx = Some(ax / n as f64);
                            sy = Some(ay / n as f64);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let (dx, dy) = (sx.unwrap_or(0.5), sy.unwrap_or(0.5));
    let diam = (rad.unwrap_or(0.02) * 2.0).clamp(0.004, 1.0);
    // source: lowercase dup fields, else SourceX + OffsetY
    let mut srcx = get_num(t, &["sourceX", "SourceX"]).unwrap_or(dx);
    let mut srcy = get_num(t, &["sourceY", "SourceY", "OffsetY"]).unwrap_or(dy);
    if (srcx == 0.0 && srcy == 0.0) || (srcx == dx && srcy == dy) {
        // patchmatch/auto-computed source — the real patch offset lives in an
        // opaque digest; approximate a heal by cloning from a few diameters
        // sideways (dust spots sit in uniform areas, so any neighbor works)
        srcx = if dx > 0.8 {
            dx - 3.0 * diam
        } else {
            dx + 3.0 * diam
        };
        srcy = dy;
    }
    let method = get_str(t, &["Method"]).unwrap_or("clone");
    format!(
        "<rdf:li crs:What=\"Retouch\" crs:Method=\"{}\" crs:SpotX=\"{:.6}\" crs:SpotY=\"{:.6}\" crs:SpotDiam=\"{:.6}\" crs:SourceX=\"{:.6}\" crs:SourceY=\"{:.6}\"/>",
        esc(method),
        dx, dy, diam, srcx, srcy
    )
}

const CORR_LISTS: &[&str] = &[
    "MaskGroupBasedCorrections",
    "GradientBasedCorrections",
    "CircularGradientBasedCorrections",
    "PaintBasedCorrections",
];
const ROW_SEQS: &[&str] = &["PointColors", "ColorVariance"];

/// `s = {...}` Lua develop-settings blob → an XMP document our
/// `crs_to_recipe` understands.
pub fn lua_to_xmp(text: &str) -> String {
    let Some(Lua::Table(items)) = parse_lua(text) else {
        return String::new();
    };
    let skip: Vec<&str> = CORR_LISTS
        .iter()
        .chain(ROW_SEQS)
        .copied()
        .chain([
            "Look",
            "LensBlur",
            "RetouchAreas",
            "ToneCurvePV2012",
            "ToneCurvePV2012Red",
            "ToneCurvePV2012Green",
            "ToneCurvePV2012Blue",
        ])
        .collect();
    let a = attrs(&items, &skip);
    let mut elems = String::new();
    for (k, v) in items {
        let Lua::Table(t) = v else { continue };
        match k.as_str() {
            "Look" => {
                let la = attrs(&t, &["Parameters", "Group", "SortName"]);
                elems += &format!("<crs:Look><rdf:Description{la}/></crs:Look>");
            }
            "LensBlur" => {
                elems += &format!("<crs:LensBlur{}/>", attrs(&t, &[]));
            }
            "RetouchAreas" => {
                elems += "<crs:RetouchAreas><rdf:Seq>";
                for (_, it) in &t {
                    if let Lua::Table(ci) = it {
                        elems += &retouch_li(ci);
                    }
                }
                elems += "</rdf:Seq></crs:RetouchAreas>";
            }
            k if CORR_LISTS.contains(&k) => {
                elems += &format!("<crs:{k}><rdf:Seq>");
                for (_, it) in &t {
                    if let Lua::Table(ci) = it {
                        elems += &corr_li(ci);
                    }
                }
                elems += &format!("</rdf:Seq></crs:{k}>");
            }
            k if ROW_SEQS.contains(&k) => {
                elems += &format!("<crs:{k}>");
                elems += &seq_of_rows(&t);
                elems += &format!("</crs:{k}>");
            }
            k if k.starts_with("ToneCurvePV2012") => {
                elems += &format!("<crs:{k}>");
                elems += &curve_seq(&t);
                elems += &format!("</crs:{k}>");
            }
            _ => {}
        }
    }
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:crs=\"http://ns.adobe.com/camera-raw-settings/1.0/\"{a}>{elems}</rdf:Description></rdf:RDF></x:xmpmeta>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lua_basic_scalars() {
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { Exposure2012 = 0.75, Contrast2012 = 30, Blacks2012 = -14, \
             CameraProfile = \"Adobe Standard\", LensProfileEnable = 1 }",
        ));
        assert!((r.exposure - 0.75).abs() < 1e-4);
        assert!((r.contrast - 0.3).abs() < 1e-4);
        assert!((r.blacks + 0.14).abs() < 1e-4);
        assert_eq!(r.profile, "Adobe Standard");
        assert!((r.lens_corr - 1.0).abs() < 1e-4);
    }

    #[test]
    fn lua_look_and_curve() {
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { Look = { Amount = 0.6, Name = \"Adobe Landscape\", \
             Parameters = { CameraProfile = \"Adobe Standard\" } }, \
             ToneCurvePV2012 = { 0, 0, 255, 255 } }",
        ));
        assert_eq!(r.profile, "Adobe Landscape");
        assert!((r.profile_amount - 0.6).abs() < 1e-4);
    }

    #[test]
    fn lua_mask_correction() {
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { MaskGroupBasedCorrections = { { CorrectionActive = true, \
             CorrectionAmount = 1, LocalExposure2012 = -0.5, What = \"Correction\", \
             CorrectionMasks = { { MaskName = \"Sky\", MaskSubType = 0, \
             MaskSubCategoryID = 2, MaskValue = 1, What = \"Mask/Image\" } } } } }",
        ));
        assert_eq!(r.windows.len(), 1);
        assert_eq!(r.windows[0].kind, "sky");
        assert!((r.windows[0].ev + 0.5).abs() < 1e-4);
    }

    #[test]
    fn lua_two_mask_corrections() {
        // two identical sky-mask corrections must produce two windows
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { MaskGroupBasedCorrections = { \
             { CorrectionActive = true, CorrectionAmount = 1, \
               LocalSaturation = 0.3, What = \"Correction\", \
               CorrectionMasks = { { MaskSubType = 2, MaskValue = 1, What = \"Mask/Image\" } } }, \
             { CorrectionActive = true, CorrectionAmount = 1, \
               LocalSaturation = 0.4, What = \"Correction\", \
               CorrectionMasks = { { MaskSubType = 2, MaskValue = 1, What = \"Mask/Image\" } } } } }",
        ));
        assert_eq!(r.windows.len(), 2);
        assert!((r.windows[0].sat - 0.3).abs() < 1e-4);
        assert!((r.windows[1].sat - 0.4).abs() < 1e-4);
    }

    #[test]
    fn lua_brush_dabs() {
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { PaintBasedCorrections = { { CorrectionAmount = 1, \
             LocalExposure2012 = 1.0, What = \"Correction\", CorrectionMasks = \
             { { What = \"Mask/Paint\", MaskValue = 1, Gesture = { { Dabs = \
             { \"r 0.02\", \"d 0.5 0.5\", \"d 0.6 0.55\" } } } } } } } }",
        ));
        assert_eq!(r.brushes.len(), 1);
        assert!((r.brushes[0].ev - 1.0).abs() < 1e-4);
        assert_eq!(r.brushes[0].strokes[0].pts.len(), 2);
    }

    #[test]
    fn lua_retouch_ellipse_and_paint() {
        // real catalog shape: spot inside Masks{}, source via lowercase dups
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { RetouchAreas = { { Method = \"gaussian\", \
             sourceX = 0.59, sourceY = 0.32, \
             Masks = { { What = \"Mask/Ellipse\", X = 0.82, Y = 0.51, \
             SizeX = 0.018, SizeY = 0.018 } } }, \
             { Method = \"poisson\", \
             Masks = { { What = \"Mask/Paint\", Radius = 0.002, \
             Dabs = { \"d 0.1 0.5\", \"d 0.3 0.5\" } } } } } }",
        ));
        assert_eq!(r.clones.len(), 2);
        let c = r.clones[0];
        assert!((c[0] - 0.59).abs() < 1e-4); // source
        assert!((c[1] - 0.32).abs() < 1e-4);
        assert!((c[2] - 0.82).abs() < 1e-4); // spot
        assert!((c[3] - 0.51).abs() < 1e-4);
        assert!((c[4] - 0.018).abs() < 1e-3); // radius
                                              // paint dab centroid 0.2,0.5 — no source → auto-heal offset
        let c = r.clones[1];
        assert!((c[2] - 0.2).abs() < 1e-4);
        assert!((c[0] - 0.212).abs() < 1e-3); // +3 diameters
        assert!((c[1] - c[3]).abs() < 1e-4);
    }
}
