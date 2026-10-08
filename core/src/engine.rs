//! High-level engine API shared by the C FFI surface and the CLI.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use serde_json::{json, Value};

use crate::auto;
use crate::catalog::{AssetEntry, Catalog};
use crate::decode::{self, Decoded};
use crate::develop::{self, RgbaImage};
use crate::gpu::Gpu;
use crate::recipe::{sidecar_path_for, sidecar_path_for_v, Recipe, Sidecar};

struct State {
    path: Option<PathBuf>,
    /// (mtime_ns, byte_len) of the file when it was decoded — a file that
    /// changed on disk invalidates the cached decode.
    stamp: Option<(u128, u64)>,
    decoded: Option<Decoded>,
}

fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let m = std::fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((t, m.len()))
}

pub struct Engine {
    state: Mutex<State>,
    catalog: Catalog,
    /// dropped permanently after the first GPU failure (device may be poisoned)
    gpu: Mutex<Option<Gpu>>,
}

impl Engine {
    pub fn new() -> Result<Engine> {
        // Resolve the ORT dylib into ORT_DYLIB_PATH up front: `ort`'s
        // load-dynamic fallback reads that env var, so every ort code path
        // (including any that might run before ai::ort_ready) can dlopen it.
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            let lib = crate::ai::ai_dir().join("libonnxruntime.dylib");
            if lib.exists() {
                unsafe { std::env::set_var("ORT_DYLIB_PATH", &lib) };
            }
        }
        Ok(Engine {
            state: Mutex::new(State {
                path: None,
                stamp: None,
                decoded: None,
            }),
            catalog: Catalog::open()?,
            gpu: Mutex::new(Gpu::try_new()),
        })
    }

    #[cfg(test)]
    pub fn new_mem() -> Result<Engine> {
        Ok(Engine {
            state: Mutex::new(State {
                path: None,
                stamp: None,
                decoded: None,
            }),
            catalog: Catalog::open_mem()?,
            gpu: Mutex::new(Gpu::try_new()),
        })
    }

    fn with_decoded<R>(&self, path: &Path, f: impl FnOnce(&Decoded) -> Result<R>) -> Result<R> {
        let mut st = self.state.lock().unwrap();
        let stamp = file_stamp(path);
        if st.path.as_deref() != Some(path) || st.decoded.is_none() || st.stamp != stamp {
            st.decoded = Some(decode::decode(path)?);
            st.path = Some(path.to_path_buf());
            st.stamp = stamp;
        }
        f(st.decoded.as_ref().unwrap())
    }

    /// render with a recipe, fit inside max_px (0 = full res)
    pub fn render(&self, path: &Path, recipe: &Recipe, max_px: u32) -> Result<RgbaImage> {
        // AI-denoised base (SCUNet): only loaded when the recipe asks for it.
        let den = if recipe.ai_denoise > 0.001 {
            load_denoise_cache(path)
        } else {
            None
        };
        // AI subject matte (U-2-Net): only needed by 'subject' power windows.
        let subj = if recipe
            .windows
            .iter()
            .any(|w| w.kind == "subject" && w.enabled)
        {
            load_subject_cache(path)
        } else {
            None
        };
        self.with_decoded(path, |d| {
            let mut gpu = self.gpu.lock().unwrap();
            if let (Decoded::Mosaic(m), Some(g)) = (d, gpu.as_ref()) {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    g.develop(m, recipe, max_px, den.as_ref(), subj.as_ref())
                }));
                match r {
                    Ok(Ok(img)) => return Ok(img),
                    Ok(Err(e)) => log::warn!("GPU develop failed, using CPU: {e:#}"),
                    Err(_) => log::warn!("GPU develop panicked, using CPU"),
                }
                // a failed device may be poisoned: drop it for good
                *gpu = None;
            }
            drop(gpu);
            Ok(develop::develop_cpu_ctx(
                d,
                recipe,
                max_px,
                den.as_ref(),
                subj.as_ref(),
            ))
        })
    }

    /// export: full-res render (max_px = 0)
    /// run the AI denoise pass on `path` and write the cache file next to it.
    /// Returns the cache dims. Heavy: caller should run off the UI thread.
    pub fn ai_denoise_prepare(&self, path: &Path, recipe: &Recipe) -> Result<(usize, usize)> {
        // session first: CoreML graph compile must happen before the big
        // buffers exist, or it gets jetsam-killed on large files
        eprintln!("[aidn] ai prewarm");
        crate::ai::prewarm()?;
        eprintln!("[aidn] decoding");
        let d = crate::decode::decode(path)?;
        eprintln!("[aidn] lin_base");
        let (lin, w, h) =
            develop::lin_base(&d, recipe).map_err(|e| anyhow::anyhow!("lin_base: {e}"))?;
        eprintln!("[aidn] lin ok {w}x{h}");
        // lin -> sRGB f32 -> SCUNet tiles -> sRGB u8 cache JPEG
        let mut flat: Vec<f32> = lin.iter().flat_map(|c| c.iter().copied()).collect();
        drop(lin);
        crate::ai::denoise_rgb(&mut flat, w, h)?; // leaves sRGB-encoded pixels
        let pix: Vec<u8> = flat
            .iter()
            .map(|&s| (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            .collect();
        let dst = crate::ai::denoise_cache_path(path);
        eprintln!("[aidn] encode jpeg -> {dst:?}");
        let img: image::ImageBuffer<image::Rgb<u8>, Vec<u8>> =
            image::ImageBuffer::from_raw(w as u32, h as u32, pix)
                .ok_or_else(|| anyhow::anyhow!("cache image alloc"))?;
        let f = std::fs::File::create(&dst)?;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(f, 92);
        enc.encode_image(&img)
            .map_err(|e| anyhow::anyhow!("jpeg encode: {e}"))?;
        Ok((w, h))
    }

    /// whether a denoise cache exists (and is fresh) for this photo.
    pub fn ai_denoise_ready(&self, path: &Path) -> bool {
        load_denoise_cache(path).is_some()
    }

    /// run U-2-Net on a neutral preview and cache the subject matte as a
    /// u16 PNG next to the photo. Much lighter than denoise (~seconds).
    /// (u2net session is created lazily inside subject_mask — no prewarm.)
    pub fn ai_subject_prepare(&self, path: &Path) -> Result<(usize, usize)> {
        let img = self.render(path, &Recipe::default(), 1024)?;
        let (w, h) = (img.width as usize, img.height as usize);
        let rgb8: Vec<u8> = img
            .data
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let matte = crate::ai::subject_mask(&rgb8, w, h)?;
        let pix: Vec<u16> = matte
            .iter()
            .map(|&v| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16)
            .collect();
        let dst = crate::ai::subject_cache_path(path);
        let img16: image::ImageBuffer<image::Luma<u16>, Vec<u16>> =
            image::ImageBuffer::from_raw(w as u32, h as u32, pix)
                .ok_or_else(|| anyhow::anyhow!("matte image alloc"))?;
        img16
            .save(&dst)
            .map_err(|e| anyhow::anyhow!("matte save: {e}"))?;
        Ok((w, h))
    }

    /// whether a subject-matte cache exists (and is fresh) for this photo.
    pub fn ai_subject_ready(&self, path: &Path) -> bool {
        load_subject_cache(path).is_some()
    }

    pub fn export(&self, path: &Path, recipe: &Recipe) -> Result<RgbaImage> {
        self.render(path, recipe, 0)
    }

    /// Export with finishing options: `long_edge` (px, area-average
    /// downscale) and `sharpen` (output unsharp 0..1 applied AFTER resize —
    /// LR's "sharpen for screen/print" slot).
    pub fn export_opts(&self, path: &Path, recipe: &Recipe, opts: &Value) -> Result<RgbaImage> {
        let mut img = self.export(path, recipe)?;
        let le = opts.get("long_edge").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        if le > 0 && (img.width as usize > le || img.height as usize > le) {
            img = resize_area(&img, le);
        }
        let sh = opts.get("sharpen").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        if sh > 0.001 {
            unsharp_rgb(&mut img, sh);
        }
        Ok(img)
    }

    /// auto-correction analysis: neutral 512px render -> suggested recipe
    /// values (see auto.rs). CPU stats on an sRGB preview.
    pub fn auto_analyze(&self, path: &Path) -> Result<auto::AutoResult> {
        let img = self.render(path, &Recipe::default(), 512)?;
        Ok(auto::analyze(&img))
    }

    /// fast preview: embedded thumbnail if any, else small develop.
    /// Results are cached on disk under ~/.araware/thumbs keyed by
    /// path+mtime+size+max_px so library rescans are instant.
    pub fn thumbnail(&self, path: &Path, max_px: u32) -> Result<RgbaImage> {
        let key = format!(
            "{}|{:?}|{}|{}",
            path.display(),
            file_stamp(path),
            path.file_name()
                .map(|n| n.to_string_lossy().len())
                .unwrap_or(0),
            max_px
        );
        // stable 64-bit FNV-ish hash for the cache file name
        let mut h: u64 = 0xcbf29ce484222325;
        for b in key.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let cache = crate::catalog::thumbs_dir().join(format!("{h:016x}.png"));
        if cache.exists() {
            if let Ok(img) = image::open(&cache) {
                let r = img.to_rgba8();
                return Ok(RgbaImage {
                    width: r.width(),
                    height: r.height(),
                    data: r.into_raw(),
                });
            }
        }
        let img = self.thumbnail_uncached(path, max_px)?;
        let _ = image::RgbaImage::from_raw(img.width, img.height, img.data.clone())
            .map(|r| image::DynamicImage::ImageRgba8(r).save(&cache));
        Ok(img)
    }

    fn thumbnail_uncached(&self, path: &Path, max_px: u32) -> Result<RgbaImage> {
        if decode::is_raw(path) {
            if let Ok(Some(t)) = decode::embedded_thumb(path) {
                let img = image::RgbaImage::from_raw(t.w as u32, t.h as u32, t.rgba);
                if let Some(img) = img {
                    // embedded thumbs are stored unrotated — match the develop
                    // path's orientation (dcraw flip codes)
                    let img = match t.flip {
                        3 => image::imageops::rotate180(&img),
                        5 => image::imageops::rotate270(&img),
                        6 => image::imageops::rotate90(&img),
                        _ => img,
                    };
                    let out = if max_px > 0 && t.w.max(t.h) as u32 > max_px {
                        let s = max_px as f32 / t.w.max(t.h) as f32;
                        image::imageops::resize(
                            &img,
                            ((t.w as f32 * s) as u32).max(1),
                            ((t.h as f32 * s) as u32).max(1),
                            image::imageops::FilterType::Triangle,
                        )
                    } else {
                        img
                    };
                    return Ok(RgbaImage {
                        width: out.width(),
                        height: out.height(),
                        data: out.into_raw(),
                    });
                }
            }
        }
        self.render(path, &Recipe::default(), max_px.max(1))
    }

    pub fn scan(&self, folder: &Path) -> Result<Vec<AssetEntry>> {
        self.catalog.scan(folder)
    }

    pub fn metadata(&self, path: &Path) -> Result<Value> {
        let mut v = json!({
            "path": path.to_string_lossy(),
            "kind": if decode::is_raw(path) { "raw" } else { "raster" },
        });
        if decode::is_raster(path) {
            if let Ok(f) = std::fs::File::open(path) {
                let ex = exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f));
                if let Ok(ex) = ex {
                    let get = |t: exif::Tag| {
                        ex.get_field(t, exif::In::PRIMARY)
                            .map(|f| f.display_value().to_string())
                    };
                    v["make"] = json!(get(exif::Tag::Make).unwrap_or_default());
                    v["model"] = json!(get(exif::Tag::Model).unwrap_or_default());
                    v["lens"] = json!(get(exif::Tag::LensModel).unwrap_or_default());
                    v["iso"] = json!(get(exif::Tag::PhotographicSensitivity).unwrap_or_default());
                    v["shutter"] = json!(get(exif::Tag::ExposureTime).unwrap_or_default());
                    v["aperture"] = json!(get(exif::Tag::FNumber).unwrap_or_default());
                    v["focal"] = json!(get(exif::Tag::FocalLength).unwrap_or_default());
                }
            }
            return Ok(v);
        }
        // RAW: open via libraw just for info (decode fills nothing else)
        let d = decode::decode(path)?;
        match d {
            Decoded::Mosaic(m) => {
                v["make"] = json!(m.info.make);
                v["model"] = json!(m.info.model);
                v["lens"] = json!(m.info.lens);
                v["iso"] = json!(m.info.iso);
                v["shutter"] = json!(m.info.shutter);
                v["aperture"] = json!(m.info.aperture);
                v["focal"] = json!(m.info.focal);
                v["timestamp"] = json!(m.info.timestamp);
                v["width"] = json!(m.w);
                v["height"] = json!(m.h);
                v["rgb_cam"] = json!(m.rgb_cam);
                v["cam_xyz"] = json!(m.cam_xyz);
                v["pre_mul"] = json!(m.pre_mul);
                // matched lensfun profile name, when one exists
                v["lens_profile"] = match crate::lensdb::match_name(
                    &m.info.lens,
                    &m.info.make,
                    &m.info.model,
                    m.info.focal,
                    m.info.aperture,
                ) {
                    Some(n) => json!(n),
                    None => json!(null),
                };
            }
            Decoded::Raster { w, h, .. } => {
                v["width"] = json!(w);
                v["height"] = json!(h);
            }
        }
        Ok(v)
    }

    pub fn read_sidecar(&self, asset: &Path) -> Result<Sidecar> {
        let sp = sidecar_path_for(asset);
        if sp.exists() {
            crate::catalog::read_sidecar(&sp)
        } else if let Some(sc) = crate::xmp::read_xmp(asset) {
            // Lightroom/darktable XMP without our JSON: adopt its adjustments
            Ok(sc)
        } else {
            Ok(Sidecar::default())
        }
    }

    pub fn write_sidecar(&self, asset: &Path, sc: &Sidecar) -> Result<()> {
        crate::catalog::write_sidecar(&sidecar_path_for(asset), sc)?;
        // LR-readable mirror; best-effort (interoperability, not truth)
        let _ = crate::xmp::write_xmp(asset, sc);
        Ok(())
    }

    pub fn set_rating(&self, asset: &Path, rating: i32) -> Result<()> {
        self.catalog.set_rating(asset, rating)
    }

    pub fn set_label(&self, asset: &Path, label: &str) -> Result<()> {
        self.catalog.set_label(asset, label)
    }

    pub fn read_sidecar_v(&self, asset: &Path, vslot: u32) -> Result<Sidecar> {
        let sp = sidecar_path_for_v(asset, vslot);
        if sp.exists() {
            crate::catalog::read_sidecar(&sp)
        } else if vslot == 0 {
            // virtual copies are ours only; the master slot may fall back
            // to a foreign .xmp (same rule as read_sidecar)
            Ok(crate::xmp::read_xmp(asset).unwrap_or_default())
        } else {
            Ok(Sidecar::default())
        }
    }

    pub fn write_sidecar_v(&self, asset: &Path, vslot: u32, sc: &Sidecar) -> Result<()> {
        crate::catalog::write_sidecar(&sidecar_path_for_v(asset, vslot), sc)?;
        if vslot == 0 {
            let _ = crate::xmp::write_xmp(asset, sc);
        }
        Ok(())
    }

    /// JSON command surface for library organization — one FFI entry point
    /// covering flags/keywords/stacks/variants/collections/smart rules.
    pub fn library(&self, cmd: &Value) -> Result<Value> {
        let op = cmd.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let path = || {
            cmd.get("path")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("missing path"))
        };
        let paths = || -> Vec<String> {
            cmd.get("paths")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        };
        let vslot = || cmd.get("vslot").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let id = || cmd.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        match op {
            "set_flag" => {
                let f = cmd.get("flag").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                if vslot() == 0 {
                    self.catalog.set_flag(&path()?, f)?;
                } else {
                    self.catalog.set_flag_v(&path()?, vslot(), f)?;
                }
                Ok(json!({"ok": true}))
            }
            "set_keywords" => {
                let kw: Vec<String> = cmd
                    .get("keywords")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                self.catalog.set_keywords(&path()?, &kw)?;
                Ok(json!({"ok": true}))
            }
            "stack_group" => Ok(json!({"stack": self.catalog.stack_group(&paths())?})),
            "stack_ungroup" => {
                self.catalog
                    .stack_ungroup(cmd.get("stack").and_then(|v| v.as_i64()).unwrap_or(0))?;
                Ok(json!({"ok": true}))
            }
            "stack_cover" => {
                self.catalog
                    .stack_cover(cmd.get("path").and_then(|v| v.as_str()).unwrap_or(""))?;
                Ok(json!({"ok": true}))
            }
            "variant_create" => {
                Ok(json!({"vslot": self.catalog.variant_create(&path()?, vslot())?}))
            }
            "variant_delete" => {
                self.catalog.variant_delete(&path()?, vslot())?;
                Ok(json!({"ok": true}))
            }
            "variant_promote" => {
                self.catalog.variant_promote(&path()?, vslot())?;
                Ok(json!({"ok": true}))
            }
            "coll_add" => Ok(json!({
                "id": self.catalog.collection_add(
                    cmd.get("name").and_then(|v| v.as_str()).unwrap_or("Collection"),
                    cmd.get("smart").and_then(|v| v.as_bool()).unwrap_or(false),
                    cmd.get("rules").and_then(|v| v.as_str()).unwrap_or(""))?})),
            "coll_rename" => {
                self.catalog.collection_rename(
                    id(),
                    cmd.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                )?;
                Ok(json!({"ok": true}))
            }
            "coll_rules" => {
                self.catalog.collection_set_rules(
                    id(),
                    cmd.get("rules").and_then(|v| v.as_str()).unwrap_or(""),
                )?;
                Ok(json!({"ok": true}))
            }
            "coll_delete" => {
                self.catalog.collection_delete(id())?;
                Ok(json!({"ok": true}))
            }
            "coll_list" => Ok(json!(self.catalog.collections()?)),
            "coll_items" => Ok(json!(self.catalog.collection_items(id())?)),
            "coll_add_items" => {
                self.catalog.collection_add_items(id(), &paths())?;
                Ok(json!({"ok": true}))
            }
            "coll_remove_items" => {
                self.catalog.collection_remove_items(id(), &paths())?;
                Ok(json!({"ok": true}))
            }
            "coll_set_items" => {
                self.catalog.collection_set_items(id(), &paths())?;
                Ok(json!({"ok": true}))
            }
            "smart_eval" => Ok(json!(self
                .catalog
                .smart_eval(cmd.get("rules").and_then(|v| v.as_str()).unwrap_or("{}"))?)),
            "folders" => Ok(json!(self.catalog.folders()?)),
            "assets" => Ok(json!(self.catalog.assets(&path()?)?)),
            _ => anyhow::bail!("unknown library op: {op}"),
        }
    }
}

/// load a subject-matte cache file if it exists and is at least as new as
/// the photo. Same freshness rule as the denoise cache.
fn load_subject_cache(path: &Path) -> Option<develop::Matte> {
    let cp = crate::ai::subject_cache_path(path);
    let (pm, cm) = (
        path.metadata().ok()?.modified().ok()?,
        cp.metadata().ok()?.modified().ok()?,
    );
    if cm < pm {
        return None;
    }
    let img = image::open(&cp).ok()?.to_luma16();
    let (w, h) = img.dimensions();
    Some(develop::Matte {
        data: img.into_raw(),
        w: w as usize,
        h: h as usize,
    })
}

/// load a denoise cache file if it exists and is at least as new as the photo.
fn load_denoise_cache(path: &Path) -> Option<develop::DenCache> {
    let cp = crate::ai::denoise_cache_path(path);
    let (pm, cm) = (
        path.metadata().ok()?.modified().ok()?,
        cp.metadata().ok()?.modified().ok()?,
    );
    if cm < pm {
        return None;
    }
    let img = image::open(&cp).ok()?.to_rgb16();
    let (w, h) = img.dimensions();
    Some(develop::DenCache {
        data: img.into_raw(),
        w: w as usize,
        h: h as usize,
    })
}

/// Area-average downscale so the long edge fits `long_edge` px — proper
/// box filtering, no aliasing (vs plain bilinear which skips source pixels).
fn resize_area(img: &RgbaImage, long_edge: usize) -> RgbaImage {
    let (w, h) = (img.width as usize, img.height as usize);
    let sc = long_edge as f64 / w.max(h) as f64;
    let (nw, nh) = (
        ((w as f64 * sc).round() as usize).max(1),
        ((h as f64 * sc).round() as usize).max(1),
    );
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        let sy0 = y * h / nh;
        let sy1 = ((y + 1) * h / nh).max(sy0 + 1);
        for x in 0..nw {
            let sx0 = x * w / nw;
            let sx1 = ((x + 1) * w / nw).max(sx0 + 1);
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            let n = ((sy1 - sy0) * (sx1 - sx0)) as u32;
            for sy in sy0..sy1.min(h) {
                for sx in sx0..sx1.min(w) {
                    let i = (sy * w + sx) * 4;
                    r += img.data[i] as u32;
                    g += img.data[i + 1] as u32;
                    b += img.data[i + 2] as u32;
                    a += img.data[i + 3] as u32;
                }
            }
            let o = (y * nw + x) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = (a / n) as u8;
        }
    }
    RgbaImage {
        width: nw as u32,
        height: nh as u32,
        data: out,
    }
}

/// Output sharpening: separable [1,2,1]/4 gaussian + `amount` unsharp on RGB.
/// Blur is stored as u8 (one extra w*h*3 buffer — precision loss is
/// inaudible at output-sharpen amounts).
fn unsharp_rgb(img: &mut RgbaImage, amount: f32) {
    let (w, h) = (img.width as usize, img.height as usize);
    let src = &img.data;
    let mut blur = vec![0u8; w * h * 3];
    // horizontal pass
    let mut tmp = vec![0u16; w * 3];
    for y in 0..h {
        for x in 0..w {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w - 1));
            for c in 0..3 {
                let i = y * w * 4;
                tmp[x * 3 + c] = (src[i + xm * 4 + c] as u16
                    + 2 * src[i + x * 4 + c] as u16
                    + src[i + xp * 4 + c] as u16)
                    / 4;
            }
        }
        for x in 0..w {
            for c in 0..3 {
                blur[(y * w + x) * 3 + c] = tmp[x * 3 + c] as u8;
            }
        }
    }
    // vertical pass (reuse tmp as the per-column accumulator)
    let mut col = vec![0u16; h * 3];
    for x in 0..w {
        for y in 0..h {
            let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h - 1));
            for c in 0..3 {
                col[y * 3 + c] = (blur[(ym * w + x) * 3 + c] as u16
                    + 2 * blur[(y * w + x) * 3 + c] as u16
                    + blur[(yp * w + x) * 3 + c] as u16)
                    / 4;
            }
        }
        for y in 0..h {
            for c in 0..3 {
                blur[(y * w + x) * 3 + c] = col[y * 3 + c] as u8;
            }
        }
    }
    let img = &mut img.data;
    for i in 0..w * h * 3 {
        let o = (i / 3) * 4 + (i % 3);
        let v = img[o] as f32 + amount * (img[o] as f32 - blur[i] as f32);
        img[o] = v.round().clamp(0.0, 255.0) as u8;
    }
}
