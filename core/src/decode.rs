//! RAW/raster decode: LibRaw CFA extraction + fallback raster loading.
use std::ffi::CString;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::ffi;

pub const RAW_EXTS: &[&str] = &[
    "arw", "cr2", "cr3", "crw", "nef", "nrw", "raf", "orf", "ori", "rw2", "dng", "pef", "srw",
    "x3f", "mrw", "erf", "raw", "rwl", "dcr", "kdc", "mos", "3fr", "fff", "iiq", "r3d", "gpr",
    "ari", "srf", "sr2",
];
// .insp = Insta360 still: a plain JPEG holding both lenses in one 2:1
// frame — readable as a normal raster for browsing/equirect export.
pub const RASTER_EXTS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "insp"];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RAW_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub fn is_raster(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| RASTER_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default)]
pub struct CameraInfo {
    pub make: String,
    pub model: String,
    pub lens: String,
    pub iso: f32,
    pub shutter: f32,
    pub aperture: f32,
    pub focal: f32,
    pub timestamp: i64,
    /// dcraw flip code: 0 none, 3 = 180deg, 5 = 90ccw, 6 = 90cw
    pub flip: i32,
}

#[derive(Debug, Clone)]
pub struct CfaPattern {
    /// 0 = bayer-like 2x2, 1 = xtrans 6x6 (mapped to dims below)
    pub w: usize,
    pub h: usize,
    /// color index per CFA cell: 0=R 1=G 2=B 3=G2/other
    pub cells: Vec<u8>,
}

impl CfaPattern {
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.cells[(y % self.h) * self.w + (x % self.w)]
    }
}

#[derive(Debug)]
pub struct Mosaic {
    /// full raw sensor area incl. margins, stride = raw_w
    pub data: Vec<u16>,
    pub raw_w: usize,
    pub raw_h: usize,
    pub left: usize,
    pub top: usize,
    /// visible frame
    pub w: usize,
    pub h: usize,
    pub black: [f32; 4],
    pub maximum: u32,
    /// libraw's per-camera scene-referred gain in EV (0 when unspecified)
    pub baseline_exposure: f32,
    /// per-channel real saturation point when libraw knows it (0 = n/a)
    pub linear_max: [u32; 4],
    /// as-shot white balance multipliers (R G1 B G2)
    pub cam_mul: [f32; 4],
    /// XYZ->camera matrix (dcraw cam_xyz semantics), 4x3
    pub cam_xyz: [[f32; 3]; 4],
    /// libraw-computed camera->sRGB matrix, 3x4 (3x3 used)
    pub rgb_cam: [[f32; 4]; 3],
    /// libraw effective channel multipliers
    pub pre_mul: [f32; 4],
    pub cfa: CfaPattern,
    pub info: CameraInfo,
    /// DNG embedded lens-correction opcodes (GainMap / WarpRectilinear)
    pub dng_ops: Option<crate::dngops::DngOps>,
    /// LR "Denoise"/"SuperRes" DNGs embed a demosaiced camera-space RGB
    /// image (LinearRaw IFD, JPEG XL tiles) — when present this is what
    /// Lightroom renders, superseding the CFA mosaic.
    pub linear: Option<crate::dngops::LinearRaw>,
}

#[derive(Debug)]
pub enum Decoded {
    Mosaic(Mosaic),
    /// already-RGB input (jpeg/png/tiff), rgba16
    Raster {
        rgba: Vec<u16>,
        w: usize,
        h: usize,
        info: CameraInfo,
        flip: i32,
    },
}

struct RawHandle {
    ptr: *mut ffi::SlRaw,
}
impl Drop for RawHandle {
    fn drop(&mut self) {
        unsafe { ffi::sl_raw_close(self.ptr) }
    }
}

fn open_raw(path: &Path) -> Result<(RawHandle, ffi::SlRawInfo)> {
    let c = CString::new(path.to_string_lossy().as_bytes())?;
    let mut info = unsafe { std::mem::zeroed::<ffi::SlRawInfo>() };
    let ptr = unsafe { ffi::sl_raw_open(c.as_ptr(), &mut info) };
    if ptr.is_null() {
        bail!("libraw cannot open {}", path.display());
    }
    Ok((RawHandle { ptr }, info))
}

fn info_of(i: &ffi::SlRawInfo) -> CameraInfo {
    CameraInfo {
        make: ffi::cstr_field(&i.make),
        model: ffi::cstr_field(&i.model),
        lens: ffi::cstr_field(&i.lens),
        iso: i.iso,
        shutter: i.shutter,
        aperture: i.aperture,
        focal: i.focal,
        timestamp: i.timestamp as i64,
        flip: i.flip,
    }
}

/// Decode a RAW file to its CFA mosaic, or a raster file to rgba16.
pub fn decode(path: &Path) -> Result<Decoded> {
    if is_raster(path) {
        return decode_raster(path);
    }
    // Sigma/Foveon X3F containers are parsed by our own decoder
    // (libraw X3F support needs the GPL x3f-tools; dcraw is public domain).
    if crate::x3f::is_x3f(path) {
        if let Ok(d) = crate::x3f::decode_x3f(path) {
            return Ok(d);
        }
        // unsupported foveon variant (e.g. Quattro layout): embedded JPEG
        if let Some((off, len)) = crate::x3f::x3f_embedded_jpeg(path) {
            let f = std::fs::read(path)?;
            if let Ok(img) = image::load_from_memory(&f[off..off + len]) {
                let rgba16 = img.to_rgba16();
                let (w, hh) = (rgba16.width() as usize, rgba16.height() as usize);
                let mut rgba = Vec::with_capacity(w * hh * 4);
                rgba.extend_from_slice(rgba16.as_raw());
                return Ok(Decoded::Raster {
                    rgba,
                    w,
                    h: hh,
                    info: crate::x3f::probe_x3f(path).unwrap_or_else(empty_camera_info),
                    flip: 0,
                });
            }
        }
        bail!("x3f decode failed for {}", path.display());
    }
    // GoPro GPR (VC-5-compressed DNG, HERO5-12): libraw needs the
    // proprietary GoPro SDK for these — convert to an uncompressed DNG
    // via the vendored gpr SDK and decode that instead.
    if is_gpr(path) {
        if let Ok(dng) = gpr_to_dng(path) {
            return decode(&dng);
        }
        // fall through: let libraw report whatever it can.
    }
    let (h, mut info) = open_raw(path)?;
    if std::env::var_os("SAFELIGHT_VERBOSE").is_some() {
        eprintln!(
            "[decode] cfa_kind={} cfa={}x{} {}x{} make={} model={}",
            info.cfa_kind,
            info.cfa_w,
            info.cfa_h,
            info.raw_width,
            info.raw_height,
            ffi::cstr_field(&info.make),
            ffi::cstr_field(&info.model)
        );
    }
    if info.cfa_kind == 0 {
        // LinearDNG (LR "Denoise"/"Super Resolution"): the LinearRaw IFD
        // holds demosaiced scene-linear RGB — the same data Lightroom
        // renders. Route it through the mosaic/linear pipeline instead of
        // libraw's display-sRGB process (which double-gammas the data).
        if let Some(lr) = crate::dngops::linear_raw(path) {
            return Ok(Decoded::Mosaic(Mosaic {
                data: Vec::new(),
                raw_w: 0,
                raw_h: 0,
                left: 0,
                top: 0,
                w: lr.w,
                h: lr.h,
                black: [0.0; 4],
                maximum: lr.white.max(1.0) as u32,
                baseline_exposure: if info.baseline_exposure < -100.0 {
                    0.0
                } else {
                    info.baseline_exposure
                },
                linear_max: info.linear_max,
                cam_mul: info.cam_mul,
                cam_xyz: info.cam_xyz,
                rgb_cam: info.rgb_cam,
                pre_mul: info.pre_mul,
                cfa: CfaPattern {
                    w: 2,
                    h: 2,
                    cells: vec![0, 1, 1, 2],
                },
                info: info_of(&info),
                dng_ops: crate::dngops::parse(path),
                linear: Some(lr),
            }));
        }
        // not a mosaic raw (linear dng / rgb) - use libraw's own pipeline,
        // falling back to system codecs / embedded preview on failure
        match process8(&h) {
            Ok(rgb) => return Ok(rgb),
            Err(e) => {
                if let Ok(d) = imgio_decode(path, &info) {
                    return Ok(d);
                }
                if let Ok(d) = embedded_preview(&h, &info) {
                    return Ok(d);
                }
                return Err(e);
            }
        }
    }
    let rc = unsafe { ffi::sl_raw_unpack(h.ptr) };
    if rc != 0 {
        // Nikon HE/HE* ("TicoRAW"): our own decoder extracts the actual
        // Bayer mosaic — real CFA data beats any sRGB fallback, so try it
        // before the OS codec and the embedded preview.
        if std::env::var_os("SAFELIGHT_NO_HE").is_none() {
            if let Ok((bayer, bw, bh)) = crate::nef_he::decode_nef_he(path) {
                return mosaic_decoded(&info, bayer, bw, bh, crate::dngops::parse(path));
            }
        }
        // other sensor compression libraw can't decode: try macOS ImageIO
        // (system codec) then the largest embedded preview — degrade
        // gracefully instead of refusing to open.
        if let Ok(d) = imgio_decode(path, &info) {
            return Ok(d);
        }
        if let Ok(d) = embedded_preview(&h, &info) {
            return Ok(d);
        }
        bail!("libraw_unpack failed ({rc}) for {}", path.display());
    }
    // black level / pre_mul / rgb_cam are only correct after unpack
    unsafe { ffi::sl_raw_refresh_info(h.ptr, &mut info) };
    let mut out: *mut u16 = std::ptr::null_mut();
    let mut count: i32 = 0;
    let rc = unsafe { ffi::sl_raw_cfa(h.ptr, &mut out, &mut count) };
    if rc != 0 || out.is_null() {
        bail!("sl_raw_cfa failed ({rc})");
    }
    let data = unsafe { Vec::from_raw_parts(out, count as usize, count as usize) };
    let mut dec = mosaic_decoded(
        &info,
        data,
        info.raw_width as usize,
        info.raw_height as usize,
        crate::dngops::parse(path),
    )?;
    if let Decoded::Mosaic(m) = &mut dec {
        m.linear = crate::dngops::linear_raw(path);
        if m.linear.is_some() && std::env::var_os("SAFELIGHT_VERBOSE").is_some() {
            let l = m.linear.as_ref().unwrap();
            eprintln!("[dng] LinearRaw {}x{} planes={}", l.w, l.h, l.planes.len());
        }
    }
    Ok(dec)
}

/// Shared Mosaic construction for CFA decodes (libraw unpack or our own
/// Nikon HE decoder). `raw_w`/`raw_h` are the decoded buffer's dimensions;
/// the visible frame/margins/CFA/color data come from libraw's metadata.
fn mosaic_decoded(
    info: &ffi::SlRawInfo,
    data: Vec<u16>,
    raw_w: usize,
    raw_h: usize,
    dng_ops: Option<crate::dngops::DngOps>,
) -> Result<Decoded> {
    let (w, hgt) = if info.width > 0 && info.height > 0 {
        (info.width as usize, info.height as usize)
    } else {
        (raw_w, raw_h)
    };
    let cfa = CfaPattern {
        w: info.cfa_w as usize,
        h: info.cfa_h as usize,
        cells: info.cfa_pattern[..(info.cfa_w * info.cfa_h) as usize]
            .iter()
            .map(|&v| v as u8)
            .collect(),
    };
    let mut black = info.black;
    estimate_black_floor(
        &data,
        raw_w,
        info.left_margin as usize,
        info.top_margin as usize,
        w,
        hgt,
        &cfa,
        &mut black,
        info.maximum,
    );
    Ok(Decoded::Mosaic(Mosaic {
        data,
        raw_w,
        raw_h,
        left: info.left_margin as usize,
        top: info.top_margin as usize,
        w,
        h: hgt,
        black,
        maximum: info.maximum.max(1),
        baseline_exposure: if info.baseline_exposure < -100.0 {
            0.0
        } else {
            info.baseline_exposure
        },
        linear_max: info.linear_max,
        cam_mul: info.cam_mul,
        cam_xyz: info.cam_xyz,
        rgb_cam: info.rgb_cam,
        pre_mul: info.pre_mul,
        cfa,
        info: info_of(info),
        dng_ops,
        linear: None,
    }))
}

/// When libraw reports no black level (e.g. Fuji RAF), estimate it per CFA
/// channel from a low percentile of the visible sensor data.
fn estimate_black_floor(
    data: &[u16],
    raw_w: usize,
    left: usize,
    top: usize,
    w: usize,
    h: usize,
    cfa: &CfaPattern,
    black: &mut [f32; 4],
    maximum: u32,
) {
    if black.iter().all(|&b| b > 0.0) {
        return;
    }
    let mut samples: [Vec<u16>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    // step coprime with both 2 (bayer) and 6 (x-trans) so every cell is sampled
    let step = 7usize;
    for sy in (top..top + h).step_by(step) {
        let row = sy * raw_w;
        for sx in (left..left + w).step_by(step) {
            let c = cfa.color_at(sx % cfa.w, sy % cfa.h) as usize;
            samples[c.min(3)].push(data[row + sx]);
        }
    }
    for c in 0..4 {
        if black[c] > 0.0 || samples[c].is_empty() {
            continue;
        }
        let mut v = samples[c].clone();
        v.sort_unstable();
        let p1 = v[(v.len() / 100).min(v.len() - 1)] as f32;
        let cap = maximum as f32 * 0.25;
        if p1 > 0.0 && p1 < cap {
            black[c] = p1;
        }
    }
}

fn empty_camera_info() -> CameraInfo {
    CameraInfo {
        make: String::new(),
        model: String::new(),
        lens: String::new(),
        iso: 0.0,
        shutter: 0.0,
        aperture: 0.0,
        focal: 0.0,
        timestamp: 0,
        flip: 0,
    }
}

fn is_gpr(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gpr"))
        .unwrap_or(false)
}

/// GPR → uncompressed DNG via the vendored gpr SDK. Converted files are
/// cached under the temp dir keyed by source path+size+mtime so repeated
/// renders don't re-decode the VC-5 payload.
fn gpr_to_dng(path: &Path) -> Result<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified()
        .map(|m| {
            m.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        })
        .unwrap_or(0)
        .hash(&mut hasher);
    let dir = std::env::temp_dir().join("safelight-gpr");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("g{:016x}.dng", hasher.finish()));
    if out.is_file() {
        return Ok(out);
    }
    let cin = CString::new(path.to_string_lossy().as_ref())?;
    let cout = CString::new(out.to_string_lossy().as_ref())?;
    let rc = unsafe { ffi::sl_gpr_to_dng(cin.as_ptr(), cout.as_ptr()) };
    if rc != 0 || !out.is_file() {
        bail!("gpr→dng conversion failed ({rc})");
    }
    Ok(out)
}

/// Full-res decode via macOS ImageIO system RAW codecs (real debayer for
/// formats libraw can't unpack, e.g. Nikon HE/HE*). Returns rgba16 sRGB.
#[cfg(not(target_os = "macos"))]
fn imgio_decode(_path: &Path, _info: &ffi::SlRawInfo) -> Result<Decoded> {
    bail!("imgio decode unavailable off macOS")
}

#[cfg(target_os = "macos")]
fn imgio_decode(path: &Path, info: &ffi::SlRawInfo) -> Result<Decoded> {
    let c = CString::new(path.to_string_lossy().as_bytes())?;
    let mut out: *mut u16 = std::ptr::null_mut();
    let (mut w, mut h) = (0i32, 0i32);
    let rc = unsafe { ffi::sl_imgio_decode(c.as_ptr(), &mut out, &mut w, &mut h) };
    if rc != 0 || out.is_null() || w <= 0 || h <= 0 {
        bail!("imgio decode failed ({rc})");
    }
    let n = (w * h * 4) as usize;
    let rgba = unsafe { Vec::from_raw_parts(out, n, n) };
    // ImageIO returns display-oriented output — do not re-apply dcraw flip.
    let mut ci = info_of(info);
    ci.flip = 0;
    Ok(Decoded::Raster {
        rgba,
        w: w as usize,
        h: h as usize,
        info: ci,
        flip: 0,
    })
}

/// Largest embedded JPEG/bitmap preview inside the RAW container — the
/// darktable-style graceful fallback for unsupported sensor compression.
fn embedded_preview(h: &RawHandle, info: &ffi::SlRawInfo) -> Result<Decoded> {
    let mut out: *mut u8 = std::ptr::null_mut();
    let (mut len, mut w, mut hgt, mut fmt) = (0i32, 0i32, 0i32, 0i32);
    let rc = unsafe { ffi::sl_thumb_best(h.ptr, &mut out, &mut len, &mut w, &mut hgt, &mut fmt) };
    if rc != 0 || out.is_null() || len <= 0 {
        bail!("no embedded preview ({rc})");
    }
    let bytes = unsafe { Vec::from_raw_parts(out, len as usize, len as usize) };
    let (rgba, rw, rh): (Vec<u16>, usize, usize) = match fmt {
        1 => {
            let img = image::load_from_memory(&bytes).context("decode embedded jpeg")?;
            let r16 = img.to_rgba16();
            (
                r16.as_raw().to_vec(),
                r16.width() as usize,
                r16.height() as usize,
            )
        }
        2 => {
            // rgb8 bitmap
            let mut v = Vec::with_capacity(bytes.len() / 3 * 4);
            for c in bytes.chunks_exact(3) {
                v.extend_from_slice(&[
                    (c[0] as u16) << 8,
                    (c[1] as u16) << 8,
                    (c[2] as u16) << 8,
                    u16::MAX,
                ]);
            }
            (v, w as usize, hgt as usize)
        }
        3 => {
            // rgb16 bitmap, big-endian u16 per dcraw
            let mut v = Vec::with_capacity(bytes.len() / 6 * 4);
            for c in bytes.chunks_exact(6) {
                let r = u16::from_be_bytes([c[0], c[1]]);
                let g = u16::from_be_bytes([c[2], c[3]]);
                let b = u16::from_be_bytes([c[4], c[5]]);
                v.extend_from_slice(&[r, g, b, u16::MAX]);
            }
            (v, w as usize, hgt as usize)
        }
        _ => bail!("unsupported embedded preview format {fmt}"),
    };
    Ok(Decoded::Raster {
        rgba,
        w: rw,
        h: rh,
        info: info_of(info),
        flip: info.flip,
    })
}

/// Header-only camera info without unpacking pixels — fast enough for
/// catalog scans. Rasters get EXIF make/model/lens.
pub fn probe(path: &Path) -> Option<CameraInfo> {
    if crate::x3f::is_x3f(path) {
        return crate::x3f::probe_x3f(path);
    }
    if is_raw(path) {
        let (_h, info) = open_raw(path).ok()?;
        return Some(info_of(&info));
    }
    if is_raster(path) {
        let f = std::fs::File::open(path).ok()?;
        let ex = exif::Reader::new()
            .read_from_container(&mut std::io::BufReader::new(f))
            .ok()?;
        let get = |t: exif::Tag| {
            ex.get_field(t, exif::In::PRIMARY)
                .map(|f| f.display_value().to_string())
                .unwrap_or_default()
        };
        return Some(CameraInfo {
            make: get(exif::Tag::Make),
            model: get(exif::Tag::Model),
            lens: get(exif::Tag::LensModel),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        });
    }
    None
}

/// Reference render via libraw's own dcraw pipeline (rgb8).
pub fn reference_render(path: &Path) -> Result<(Vec<u8>, usize, usize)> {
    let (h, _info) = open_raw(path)?;
    process8(&h).map(|d| match d {
        Decoded::Raster { rgba, w, h, .. } => {
            let bytes: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|c| [(c[0] >> 8) as u8, (c[1] >> 8) as u8, (c[2] >> 8) as u8])
                .collect();
            (bytes, w, h)
        }
        Decoded::Mosaic(_) => unreachable!(),
    })
}

fn process8(h: &RawHandle) -> Result<Decoded> {
    let mut out: *mut u8 = std::ptr::null_mut();
    let (mut w, mut hgt) = (0i32, 0i32);
    let rc = unsafe { ffi::sl_process8(h.ptr, &mut out, &mut w, &mut hgt) };
    if rc != 0 || out.is_null() {
        bail!("libraw dcraw_process failed ({rc})");
    }
    let n = (w * hgt * 3) as usize;
    let rgb = unsafe { Vec::from_raw_parts(out, n, n) };
    let mut rgba = Vec::with_capacity(n / 3 * 4);
    for c in rgb.chunks_exact(3) {
        rgba.push((c[0] as u16) << 8);
        rgba.push((c[1] as u16) << 8);
        rgba.push((c[2] as u16) << 8);
        rgba.push(u16::MAX);
    }
    Ok(Decoded::Raster {
        rgba,
        w: w as usize,
        h: hgt as usize,
        info: CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        },
        flip: 0,
    })
}

fn decode_raster(path: &Path) -> Result<Decoded> {
    // sniff content, not extension: .insp stills are dual-lens JPEGs and
    // image::open's format guess rejects unknown extensions outright
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let img =
        image::load_from_memory(&bytes).with_context(|| format!("decode {}", path.display()))?;
    // keep 16-bit sources at full depth (to_rgba16 expands 8-bit inputs
    // identically to the old (v<<8)|v path)
    let rgba16 = img.to_rgba16();
    let (w, h) = (rgba16.width() as usize, rgba16.height() as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    rgba.extend_from_slice(rgba16.as_raw());
    Ok(Decoded::Raster {
        rgba,
        w,
        h,
        info: CameraInfo {
            make: String::new(),
            model: String::new(),
            lens: String::new(),
            iso: 0.0,
            shutter: 0.0,
            aperture: 0.0,
            focal: 0.0,
            timestamp: 0,
            flip: 0,
        },
        flip: 0,
    })
}

/// Embedded JPEG/bitmap thumbnail from the RAW container.
pub struct Thumb {
    pub w: usize,
    pub h: usize,
    /// rgba8 pixels
    pub rgba: Vec<u8>,
    /// dcraw flip code for the source frame — most cameras embed the
    /// thumbnail unrotated, so the caller applies it
    pub flip: i32,
}

pub fn embedded_thumb(path: &Path) -> Result<Option<Thumb>> {
    // x3f containers carry JPEG previews libraw can't reach (it can't even
    // open the file) — parse the FOVb directory ourselves.
    if crate::x3f::is_x3f(path) {
        if let Some((off, len)) = crate::x3f::x3f_embedded_jpeg(path) {
            if let Ok(f) = std::fs::read(path) {
                if let Ok(img) = image::load_from_memory(&f[off..off + len]) {
                    let flip = crate::x3f::probe_x3f(path).map(|i| i.flip).unwrap_or(0);
                    let rgba = img.to_rgba8();
                    let (tw, th) = (rgba.width() as usize, rgba.height() as usize);
                    return Ok(Some(Thumb {
                        w: tw,
                        h: th,
                        rgba: rgba.into_raw(),
                        flip,
                    }));
                }
            }
        }
        return Ok(None);
    }
    let (h, info) = open_raw(path)?;
    let mut out: *mut u8 = std::ptr::null_mut();
    let (mut len, mut w, mut hgt, mut fmt) = (0i32, 0i32, 0i32, 0i32);
    let rc = unsafe { ffi::sl_thumb(h.ptr, &mut out, &mut len, &mut w, &mut hgt, &mut fmt) };
    if rc != 0 || out.is_null() || len <= 0 {
        return Ok(None);
    }
    let bytes = unsafe { Vec::from_raw_parts(out, len as usize, len as usize) };
    let rgba = match fmt {
        1 => {
            // jpeg
            let img = image::load_from_memory(&bytes).context("decode embedded jpeg")?;
            img.to_rgba8().into_raw()
        }
        2 => {
            // rgb8 bitmap
            let mut v = Vec::with_capacity(bytes.len() / 3 * 4);
            for c in bytes.chunks_exact(3) {
                v.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            v
        }
        3 => {
            // rgb16 bitmap
            let mut v = Vec::with_capacity(bytes.len() / 6 * 4);
            for c in bytes.chunks_exact(6) {
                let r = u16::from_be_bytes([c[0], c[1]]);
                let g = u16::from_be_bytes([c[2], c[3]]);
                let b = u16::from_be_bytes([c[4], c[5]]);
                v.extend_from_slice(&[(r >> 8) as u8, (g >> 8) as u8, (b >> 8) as u8, 255]);
            }
            v
        }
        _ => return Ok(None),
    };
    Ok(Some(Thumb {
        w: w as usize,
        h: hgt as usize,
        rgba,
        flip: info.flip,
    }))
}

#[cfg(test)]
mod rawstats {
    use super::*;
    #[test]
    fn parse_dcp() {
        let p = Path::new(
            r"C:\Program Files\Adobe\Adobe Lightroom Classic\Resources\CameraProfiles\Adobe Standard\Sony ILCE-7RM3 Adobe Standard.dcp",
        );
        // diagnostic: needs a local Lightroom install; skip elsewhere
        let Ok(d) = std::fs::read(p) else { return };
        let prof = crate::dcp::DcpProfile::parse(&d).unwrap();
        eprintln!(
            "name={:?} hsm1={} hsm2={} tone={}pts look={} fwd={:?}",
            prof.name,
            prof.hsm1.is_some(),
            prof.hsm2.is_some(),
            prof.tone_curve.len(),
            prof.look_table.is_some(),
            (
                prof.forward_matrix1.is_some(),
                prof.forward_matrix2.is_some()
            )
        );
        if let Some(h) = &prof.hsm1 {
            let vm: f32 = h.data.iter().map(|c| c[2]).sum::<f32>() / h.data.len() as f32;
            eprintln!("hsm dims={:?} vscale_mean={vm:.3}", h.dims);
        }
    }
    #[test]
    #[ignore = "slow diagnostic: decodes an 86MB raw several times"]
    fn print_sony() {
        let p = Path::new(r"G:\カメラ\RAW_alpha7R3\2023\0419\DSC00151.ARW");
        // diagnostic: needs the local RAW collection; skip elsewhere
        if !p.exists() {
            return;
        }
        match decode(p) {
            Ok(Decoded::Mosaic(m)) => {
                eprintln!(
                    "black={:?} max={} cam_mul={:?} pre_mul={:?} baseline_ev={} linear_max={:?}",
                    m.black, m.maximum, m.cam_mul, m.pre_mul, m.baseline_exposure, m.linear_max
                );
                // raw DN histogram on the CFA plane (sample every 4th px)
                let mut h = [0u64; 16];
                let mut at_black = 0u64;
                let mut n = 0u64;
                let max = m.maximum.max(1) as f32;
                for i in (0..m.data.len()).step_by(4) {
                    let v = m.data[i] as f32 / max;
                    h[(v * 15.999) as usize] += 1;
                    if m.data[i] as f32 <= m.black[0].max(m.black[1]).max(m.black[2]) {
                        at_black += 1;
                    }
                    n += 1;
                }
                eprintln!("hist/16 {:?}", h);
                eprintln!("at_or_below_black {:.3}", at_black as f32 / n as f32);
                // scene-linear stats after black subtraction, per CFA channel
                let blk = m.black[0];
                let mut sums = [0f64; 4];
                let mut cnt = [0u64; 4];
                // step 1: every CFA cell must be hit (a 2-step keeps parity)
                for y in 0..m.raw_h.min(4000) {
                    for x in 0..m.raw_w.min(4000) {
                        let ch = m.cfa.color_at(x % m.cfa.w, y % m.cfa.h) as usize;
                        let v = (m.data[y * m.raw_w + x] as f32 - blk).max(0.0) / (max - blk);
                        sums[ch] += v as f64;
                        cnt[ch] += 1;
                    }
                }
                eprintln!(
                    "cfa={:?}x{:?} cnt={:?} linear/ch R={:.5} G={:.5} B={:.5} G2={:.5}",
                    m.cfa.w,
                    m.cfa.cells,
                    cnt,
                    sums[0] / cnt[0].max(1) as f64,
                    sums[1] / cnt[1].max(1) as f64,
                    sums[2] / cnt[2].max(1) as f64,
                    sums[3] / cnt[3].max(1) as f64
                );
                // masked-margin pixels = the true optical black level
                eprintln!(
                    "raw {}x{} margins l={} t={}",
                    m.raw_w, m.raw_h, m.left, m.top
                );
                for band in [
                    (0usize, m.raw_h, 0usize, m.left.min(64)),
                    (0usize, m.top.min(64), 0usize, m.raw_w),
                ] {
                    let (y0, y1, x0, x1) = band;
                    let mut v = Vec::new();
                    for y in y0..y1 {
                        for x in x0..x1 {
                            v.push(m.data[y * m.raw_w + x]);
                        }
                    }
                    if !v.is_empty() {
                        v.sort_unstable();
                        eprintln!(
                            "  band {:?}: n={} p10={} p50={} p90={}",
                            band,
                            v.len(),
                            v[v.len() / 10],
                            v[v.len() / 2],
                            v[v.len() * 9 / 10]
                        );
                    }
                }
                // stage isolation: mean of the post-colorize linear buffer
                // (WB + profile matrix, pre-adjust) vs the display output
                {
                    let r = crate::recipe::Recipe::default();
                    let dd = Decoded::Mosaic(m);
                    match crate::develop::lin_base(&dd, &r) {
                        Ok((lin, _w, _h)) => {
                            let (mut s, mut n) = (0.0f64, 0u64);
                            for px in lin.iter().step_by(97) {
                                s += (px[0] + px[1] + px[2]) as f64;
                                n += 1;
                            }
                            eprintln!("lin_base mean {:.5}", s / n.max(1) as f64 / 3.0);
                        }
                        Err(e) => eprintln!("lin_base: {e}"),
                    }
                }
                // try lower black levels: if the true black is below 512,
                // subtracting 512 crushes real shadow signal
                for blk in [512.0f32, 384.0, 256.0, 128.0, 0.0] {
                    let mut m2 = match decode(p) {
                        Ok(Decoded::Mosaic(mm)) => mm,
                        _ => continue,
                    };
                    m2.black = [blk; 4];
                    let mut r = crate::recipe::Recipe::default();
                    r.profile = "Adobe Color".to_string();
                    r.curve = vec![[0.0, 0.0], [1.0, 1.0]];
                    let img = crate::develop::develop_cpu(&Decoded::Mosaic(m2), &r, 1024);
                    let buf = &img.data;
                    let (mut n0, mut sum) = (0u64, 0u64);
                    for px in buf.chunks(4) {
                        if px[0].max(px[1]).max(px[2]) == 0 {
                            n0 += 1;
                        }
                        sum += px[0] as u64 + px[1] as u64 + px[2] as u64;
                    }
                    let np = (buf.len() / 4) as u64;
                    eprintln!(
                        "black={blk}: zero={:.3} mean={:.3}",
                        n0 as f32 / np as f32,
                        sum as f32 / (np as f32 * 3.0) / 255.0
                    );
                }
            }
            _ => eprintln!("not mosaic"),
        }
    }

    /// Scene-linear percentiles of the demosaiced G channel, for
    /// correlating fitted exposure-ramp params with raw stats.
    /// `SAFELIGHT_DIAG_FILE` picks the raw file.
    #[test]
    #[ignore = "diagnostic: decodes a local raw file"]
    fn raw_stats() {
        let path = std::env::var("SAFELIGHT_DIAG_FILE")
            .unwrap_or_else(|_| r"G:\カメラ\RAW_alpha7R3\2023\0419\DSC00151.ARW".into());
        let p = Path::new(&path);
        if !p.exists() {
            return;
        }
        let m = match decode(p) {
            Ok(Decoded::Mosaic(m)) => m,
            _ => return,
        };
        let blk = m.black[0].max(m.black[1]).max(m.black[2]);
        let white = m
            .linear_max
            .iter()
            .copied()
            .min()
            .unwrap_or(m.maximum)
            .max(blk as u32) as f32;
        // sample the green channel (dominant luma proxy) every 8th px
        let mut v = Vec::with_capacity(1 << 18);
        for i in (0..m.data.len()).step_by(8) {
            v.push((m.data[i] as f32 - blk).max(0.0) / (white - blk));
        }
        v.sort_unstable_by(|a, b| a.total_cmp(b));
        let q = |f: f64| v[((v.len() - 1) as f64 * f) as usize];
        eprintln!(
            "raw_stats {}: p001={:.5} p01={:.5} p05={:.5} p25={:.5} p50={:.5} p75={:.5} p95={:.5} p99={:.5} p999={:.5} max={:.5}",
            path, q(0.001), q(0.01), q(0.05), q(0.25), q(0.5), q(0.75), q(0.95), q(0.99), q(0.999), v[v.len()-1]
        );
    }
}

#[cfg(test)]
mod heinfo {
    use super::*;
    #[test]
    fn print_info() {
        for f in [
            "real-raws/net/nikon_z9_he_star.nef",
            "real-raws/net/nikon_z8_lossless.nef",
        ] {
            let fp = format!("/Users/devin/{f}");
            let p = Path::new(&fp);
            match decode(p) {
                Ok(Decoded::Mosaic(m)) => {
                    eprintln!(
                        "{f}: black={:?} max={} cam_mul={:?} pre_mul={:?}",
                        m.black, m.maximum, m.cam_mul, m.pre_mul
                    );
                    eprintln!("  rgb_cam={:?}", m.rgb_cam);
                    eprintln!(
                        "  cfa={:?} w={}x{} raw={}x{} margins={}/{}",
                        m.cfa.cells, m.w, m.h, m.raw_w, m.raw_h, m.left, m.top
                    );
                }
                Ok(_) => eprintln!("{f}: raster"),
                Err(e) => eprintln!("{f}: ERR {e}"),
            }
        }
    }
}
