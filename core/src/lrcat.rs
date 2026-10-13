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
                i.rating, i.pick, COALESCE(i.colorLabels,''), COALESCE(ds.text,''), \
                COALESCE(kw.kws,'') \
         FROM Adobe_images i \
         JOIN AgLibraryFile f ON i.rootFile = f.id_local \
         JOIN AgLibraryFolder fo ON f.folder = fo.id_local \
         JOIN AgLibraryRootFolder rf ON fo.rootFolder = rf.id_local \
         LEFT JOIN Adobe_imageDevelopSettings ds ON ds.image = i.id_local \
         LEFT JOIN (SELECT ki.image, GROUP_CONCAT(k.lc_name, char(1)) AS kws \
                    FROM AgLibraryKeywordImage ki \
                    JOIN AgLibraryKeyword k ON k.id_local = ki.tag \
                    GROUP BY ki.image) kw ON kw.image = i.id_local",
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
        sc.keywords = r
            .get::<_, String>(7)
            .unwrap_or_default()
            .split('\x01')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
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
// .lrcat-data AI-mask blob extraction
// ---------------------------------------------------------------------------
//
// Lightroom stores its *computed* AI-mask bitmaps (subject/sky/landscape
// semantic mattes) in the catalog's `<name>.lrcat-data` LevelDB/blob store,
// keyed by each mask's `MaskDigest`. Every `.blob` file is a ~110-byte
// header — the digest as a 32-char ASCII GUID near offset 62 — followed by
// an embedded grayscale TIFF (deflate strips or JPEG tiles) covering only
// the mask's bounding box; `Origin`/`WholeImageArea` in the develop text
// place that box in mask space. Extracting these gives us Adobe's real
// mattes instead of our heuristic approximations.

/// A resolved mask bitmap inside the `.lrcat-data` store: either a slice
/// of a `NNNNNN.blob` file or TIFF data held inline in an SST record.
#[derive(Debug, Clone)]
pub struct MaskBlob {
    pub path: PathBuf,
    /// byte offset of the embedded TIFF record
    pub off: u64,
    /// 0 = to end of file
    pub len: u64,
    /// non-empty: TIFF-bearing record stored inline in the SST
    pub inline: Vec<u8>,
}

fn rv64(b: &[u8], o: &mut usize) -> Option<u64> {
    let (mut r, mut s) = (0u64, 0u32);
    loop {
        let x = *b.get(*o)? as u64;
        *o += 1;
        r |= (x & 0x7f) << s;
        if x & 0x80 == 0 {
            return Some(r);
        }
        s += 7;
        if s > 63 {
            return None;
        }
    }
}

/// LevelDB/RocksDB table (SST) records — the `.lrcat-data` store keys each
/// mask's MaskDigest to either `[1, blob_file_no, data_off, data_len]` (the
/// TIFF then sits at `data_off + 16` inside `NNNNNN.blob`) or an inline
/// TIFF in the value itself. Records are prefix-compressed; we scan
/// linearly and resync across block boundaries/restart arrays.
fn sst_records(raw: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut o = 0usize;
    let mut prev: Vec<u8> = Vec::new();
    while o < raw.len() {
        let st = o;
        let rec = (|| {
            let sh = rv64(raw, &mut o)? as usize;
            let ns = rv64(raw, &mut o)? as usize;
            let vl = rv64(raw, &mut o)? as usize;
            if sh > prev.len() || ns > 4096 || o.checked_add(ns + vl)? > raw.len() {
                return None;
            }
            let mut k = prev[..sh].to_vec();
            k.extend_from_slice(&raw[o..o + ns]);
            o += ns;
            let v = raw[o..o + vl].to_vec();
            o += vl;
            Some((k, v))
        })();
        match rec {
            Some((k, v)) => {
                out.push((k.clone(), v));
                prev = k;
            }
            None => {
                // restart arrays, block trailers and the index/meta blocks
                // don't parse as records — resync at the next offset that
                // starts a fresh (shared=0) record
                o = st + 1;
                prev.clear();
                let mut probe = o;
                while probe < raw.len() {
                    let mut q = probe;
                    let good = (|| {
                        let sh = rv64(raw, &mut q)? as usize;
                        let ns = rv64(raw, &mut q)? as usize;
                        let vl = rv64(raw, &mut q)? as usize;
                        (sh == 0 && ns > 0 && ns < 4096 && q + ns + vl <= raw.len()).then_some(())
                    })();
                    if good.is_some() {
                        break;
                    }
                    probe += 1;
                }
                if probe >= raw.len() {
                    break;
                }
                o = probe;
            }
        }
    }
    out
}

/// Map MaskDigest (uppercase hex) → TIFF location for every AI-mask
/// bitmap in the catalog's `<catalog>.lrcat-data` store. Two keying
/// styles are handled: newer catalogs key a LevelDB SST record by
/// MaskDigest; older stores carry the digest in each `.blob` header.
pub fn blob_index(cat: &Path) -> std::collections::HashMap<String, MaskBlob> {
    let mut out = std::collections::HashMap::new();
    let own = cat.with_extension("lrcat-data");
    // Masks are content-addressed by digest and recomputed mattes land in
    // the *newest* catalog's data store — index every sibling
    // `*.lrcat-data` directory, then the requested catalog's own so its
    // records win ties.
    if let Some(parent) = cat.parent() {
        if let Ok(rd) = std::fs::read_dir(parent) {
            let mut sibs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    *p != own && p.extension().and_then(|s| s.to_str()) == Some("lrcat-data")
                })
                .collect();
            sibs.sort();
            for d in sibs {
                blob_index_dir(&d, &mut out);
            }
        }
    }
    blob_index_dir(&own, &mut out);
    out
}

fn blob_index_dir(dir: &Path, out: &mut std::collections::HashMap<String, MaskBlob>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut blobs: Vec<(u64, PathBuf)> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            match p.extension().and_then(|s| s.to_str()) {
                Some("blob") => {
                    if let Some(n) = p
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        blobs.push((n, p));
                    }
                }
                Some("sst") => {
                    let Ok(raw) = std::fs::read(&p) else {
                        continue;
                    };
                    for (k, v) in sst_records(&raw) {
                        if k.len() < 32
                            || !k[..32]
                                .iter()
                                .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
                        {
                            continue;
                        }
                        let d = String::from_utf8_lossy(&k[..32]).into_owned();
                        if v.windows(4).any(|w| w == b"II*\x00") {
                            // small mattes are stored inline in the record
                            out.insert(
                                d,
                                MaskBlob {
                                    path: p.clone(),
                                    off: 0,
                                    len: 0,
                                    inline: v,
                                },
                            );
                            continue;
                        }
                        let mut o = 0;
                        let (Some(ver), Some(fno), Some(off), Some(len)) = (
                            rv64(&v, &mut o),
                            rv64(&v, &mut o),
                            rv64(&v, &mut o),
                            rv64(&v, &mut o),
                        ) else {
                            continue;
                        };
                        if ver != 1 || len == 0 {
                            continue;
                        }
                        out.insert(
                            d,
                            MaskBlob {
                                path: dir.join(format!("{fno:06}.blob")),
                                off: off + 16,
                                len,
                                inline: Vec::new(),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        // fallback: scan whole blobs for 32-char uppercase-hex digests —
        // each record carries the MaskDigest at +62 and its TIFF at +110,
        // i.e. the TIFF sits at digest_pos + 48. Blob files pack several
        // records, so only the first record shows up near the header.
        for (_, p) in blobs {
            let Ok(raw) = std::fs::read(&p) else {
                continue;
            };
            let mut i = 0;
            while i + 52 <= raw.len() {
                let w = &raw[i..i + 32];
                if w.iter()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(b))
                    && raw[i + 48..i + 52] == *b"II*\x00"
                {
                    let d = String::from_utf8_lossy(w).into_owned();
                    out.entry(d).or_insert(MaskBlob {
                        path: p.clone(),
                        off: (i + 48) as u64,
                        len: 0,
                        inline: Vec::new(),
                    });
                    i += 48;
                } else {
                    i += 1;
                }
            }
        }
    }
}

/// Decode the embedded TIFF of a mask blob → (w, h, 8-bit gray pixels).
/// extract the embedded TIFF bytes from a mask blob record
fn blob_tiff(b: &MaskBlob) -> Option<Vec<u8>> {
    let mut d = if !b.inline.is_empty() {
        b.inline.clone()
    } else {
        let mut d = std::fs::read(&b.path).ok()?;
        d.drain(..b.off.min(d.len() as u64) as usize);
        if b.len > 0 {
            d.truncate(b.len as usize);
        }
        d
    };
    let o = d.windows(4).position(|w| w == b"II*\x00")?;
    d.drain(..o);
    Some(d)
}

pub fn decode_mask_blob(b: &MaskBlob) -> Option<(u32, u32, Vec<u8>)> {
    let d = blob_tiff(b)?;
    let t = &d[..];
    // JPEG(7)/JXL(52546) tiles must use the custom decoders — the image
    // crate may misinterpret the payload and return noise instead of an err
    if let Some((_, _, comp, ..)) = tiff_layout(t) {
        if matches!(comp, 7 | 52546) {
            return decode_tiff_tiles(t);
        }
    }
    if let Ok(dec) = image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(t)) {
        if let Ok(img) = image::DynamicImage::from_decoder(dec) {
            let img = img.to_luma8();
            return Some((img.width(), img.height(), img.into_raw()));
        }
    }
    // newer catalogs store masks as TIFF tiles holding JPEG or JPEG XL
    // codestreams (compression 7 / 52546), which the image crate's TIFF
    // decoder can't handle — parse the IFD and decode each tile directly
    decode_tiff_tiles(t)
}

/// Decode a blob TIFF at full fidelity → (w, h, per-channel u16 planes).
/// Needed for PatchMatch offset maps (3×16-bit channels) which must not
/// collapse to luma. JPEG/JXL tile codestreams decode to their native
/// precision; anything the image crate handles normalizes to u16.
pub fn decode_blob_rgb16(b: &MaskBlob) -> Option<(u32, u32, Vec<Vec<u16>>)> {
    let d = blob_tiff(b)?;
    let t = &d[..];
    // compressed tiles (JPEG=7, JXL=52546) must go through the custom
    // multi-plane decoder — the image crate may "succeed" with a garbage
    // interpretation of the payload and silently return noise.
    if let Some((_, _, comp, ..)) = tiff_layout(t) {
        if matches!(comp, 7 | 52546) {
            return decode_tiff_tiles_rgb(t);
        }
    }
    if let Ok(dec) = image::codecs::tiff::TiffDecoder::new(std::io::Cursor::new(t)) {
        if let Ok(img) = image::DynamicImage::from_decoder(dec) {
            let rgb = img.to_rgb16();
            let (w, h) = (rgb.width(), rgb.height());
            let raw = rgb.into_raw();
            let mut planes = vec![Vec::with_capacity((w * h) as usize); 3];
            for px in raw.chunks_exact(3) {
                planes[0].push(px[0]);
                planes[1].push(px[1]);
                planes[2].push(px[2]);
            }
            return Some((w, h, planes));
        }
    }
    decode_tiff_tiles_rgb(t)
}

/// minimal TIFF reader for Adobe's compressed-tile mask bitmaps: pulls each
/// tile's codestream through the JPEG or JPEG XL decoder and blits the
/// tiles into one luma matte.
/// parse a TIFF IFD → (w, h, compression, tile_w, tile_h, offsets, lens)
fn tiff_layout(t: &[u8]) -> Option<(u32, u32, u32, u32, u32, Vec<u32>, Vec<u32>)> {
    fn le32(t: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_le_bytes(t.get(o..o + 4)?.try_into().ok()?))
    }
    fn le16(t: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_le_bytes(t.get(o..o + 2)?.try_into().ok()?))
    }
    if t.get(..4)? != b"II*\x00" {
        return None;
    }
    let ifd = le32(t, 4)? as usize;
    let n = le16(t, ifd)? as usize;
    let mut tags = std::collections::HashMap::new();
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        let id = le16(t, e)?;
        let typ = le16(t, e + 2)?;
        let cnt = le32(t, e + 4)?;
        let val = *t.get(e + 8..e + 12)?.first_chunk::<4>()?;
        tags.insert(id, (typ, cnt, val));
    }
    // tag values that don't fit inline are stored at a byte offset
    let vals = |id: u16| -> Option<Vec<u32>> {
        let &(typ, cnt, val) = tags.get(&id)?;
        let tsz = match typ {
            1 | 2 => 1usize,
            3 => 2,
            _ => 4,
        };
        let sz = tsz * cnt as usize;
        if sz <= 4 {
            return Some(
                (0..cnt as usize)
                    .map(|i| match tsz {
                        1 => val[i] as u32,
                        2 => u16::from_le_bytes(val[i * 2..i * 2 + 2].try_into().unwrap()) as u32,
                        _ => u32::from_le_bytes(val[i * 4..i * 4 + 4].try_into().unwrap()),
                    })
                    .collect(),
            );
        }
        let base = u32::from_le_bytes(val) as usize;
        Some(
            (0..cnt as usize)
                .filter_map(|i| match tsz {
                    1 => t.get(base + i).map(|&b| b as u8 as u32),
                    2 => le16(t, base + i * 2).map(|v| v as u32),
                    _ => le32(t, base + i * 4),
                })
                .collect(),
        )
    };
    let w = *vals(256)?.first()?;
    let h = *vals(257)?.first()?;
    let comp = *vals(259)?.first()?;
    let tw = vals(322).and_then(|v| v.first().copied()).unwrap_or(w);
    let th = vals(323)
        .and_then(|v| v.first().copied())
        .or_else(|| vals(278).and_then(|v| v.first().copied()))
        .unwrap_or(h);
    let offs = vals(324).or_else(|| vals(273))?;
    let lens = vals(325).or_else(|| vals(279))?;
    Some((w, h, comp, tw, th, offs, lens))
}

fn decode_tiff_tiles(t: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h, comp, tw, th, offs, lens) = tiff_layout(t)?;
    let nx = w.div_ceil(tw) as usize;
    let mut out = vec![0u8; (w * h) as usize];
    for (ti, (&to, &tl)) in offs.iter().zip(lens.iter()).enumerate() {
        let tile = t.get(to as usize..(to + tl) as usize)?;
        let (pw, ph, px) = match comp {
            // 52546: JPEG XL codestream; 7: JPEG bitstream
            52546 => jxl_gray(tile)?,
            7 => jpeg_gray(tile)?,
            _ => return None,
        };
        let (tx, ty) = ((ti % nx) as u32 * tw, (ti / nx) as u32 * th);
        for y in 0..(ph as u32).min(h.saturating_sub(ty)) {
            for x in 0..(pw as u32).min(w.saturating_sub(tx)) {
                out[(ty + y) as usize * w as usize + (tx + x) as usize] =
                    px[y as usize * pw + x as usize];
            }
        }
    }
    Some((w, h, out))
}

/// multi-channel tile decode → per-channel u16 planes (PatchMatch maps:
/// 3×16-bit). JPEG tiles go through the lossless decoder (16-bit);
/// JXL tiles use jxl-oxide (f32 planes scaled to u16).
fn decode_tiff_tiles_rgb(t: &[u8]) -> Option<(u32, u32, Vec<Vec<u16>>)> {
    let (w, h, comp, tw, th, offs, lens) = tiff_layout(t)?;
    let nx = w.div_ceil(tw) as usize;
    let mut out: Vec<Vec<u16>> = Vec::new();
    for (ti, (&to, &tl)) in offs.iter().zip(lens.iter()).enumerate() {
        let tile = t.get(to as usize..(to + tl) as usize)?;
        let (pw, ph, planes) = match comp {
            52546 => jxl_planes_u16(tile)?,
            7 => jpeg_lossless_planes(tile)
                .map(|(w, h, prec, pl)| {
                    let mx = ((1i64 << prec.min(31)) - 1).max(1) as f32;
                    (
                        w,
                        h,
                        pl.iter()
                            .map(|p| {
                                p.iter()
                                    .map(|&v| (v as f32 * 65535.0 / mx).clamp(0.0, 65535.0) as u16)
                                    .collect::<Vec<u16>>()
                            })
                            .collect(),
                    )
                })
                .or_else(|| jpeg_planes_u16(tile))?,
            _ => return None,
        };
        if out.is_empty() {
            out = vec![vec![0u16; (w * h) as usize]; planes.len()];
        }
        let (tx, ty) = ((ti % nx) as u32 * tw, (ti / nx) as u32 * th);
        for (c, p) in planes.iter().enumerate().take(out.len()) {
            for y in 0..(ph as u32).min(h.saturating_sub(ty)) {
                for x in 0..(pw as u32).min(w.saturating_sub(tx)) {
                    out[c][(ty + y) as usize * w as usize + (tx + x) as usize] =
                        p[y as usize * pw + x as usize];
                }
            }
        }
    }
    if out.is_empty() {
        return None;
    }
    Some((w, h, out))
}

/// JXL codestream → (w, h, per-channel u16 planes)
fn jxl_planes_u16(b: &[u8]) -> Option<(usize, usize, Vec<Vec<u16>>)> {
    let mut img = jxl_oxide::JxlImage::read_with_defaults(std::io::Cursor::new(b)).ok()?;
    let render = img.render_frame(0).ok()?;
    let planes: Vec<Vec<u16>> = render
        .image_planar()
        .iter()
        .map(|fb| {
            fb.buf()
                .iter()
                .map(|&v| (v * 65535.0).clamp(0.0, 65535.0) as u16)
                .collect()
        })
        .collect();
    let binding = render.image_planar();
    let fb = binding.first()?;
    Some((fb.width(), fb.height(), planes))
}

/// baseline JPEG → (w, h, per-channel u16 planes) via the image crate
fn jpeg_planes_u16(b: &[u8]) -> Option<(usize, usize, Vec<Vec<u16>>)> {
    let img = image::load_from_memory_with_format(b, image::ImageFormat::Jpeg).ok()?;
    let rgb = img.to_rgb16();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut planes = vec![Vec::with_capacity(w * h); 3];
    for px in rgb.into_raw().chunks_exact(3) {
        planes[0].push(px[0]);
        planes[1].push(px[1]);
        planes[2].push(px[2]);
    }
    Some((w, h, planes))
}

/// decode one JPEG XL codestream → (w, h, 8-bit gray)
fn jxl_gray(b: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let mut img = jxl_oxide::JxlImage::read_with_defaults(std::io::Cursor::new(b)).ok()?;
    let render = img.render_frame(0).ok()?;
    let planes = render.image_planar();
    let fb = planes.first()?;
    let (w, h) = (fb.width(), fb.height());
    let ch = planes.len().max(1) as f32;
    let px = (0..w * h)
        .map(|i| {
            let s: f32 = planes.iter().map(|p| p.buf()[i]).sum();
            (s / ch * 255.0 + 0.5).clamp(0.0, 255.0) as u8
        })
        .collect();
    Some((w, h, px))
}

/// decode one JPEG bitstream → (w, h, 8-bit gray). Baseline tiles go through
/// the image crate; lossless JPEG (SOF3, Huffman predictive) is decoded here
/// — Adobe uses it for small matte tiles.
fn jpeg_gray(b: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if let Ok(img) = image::load_from_memory_with_format(b, image::ImageFormat::Jpeg) {
        let img = img.to_luma8();
        return Some((img.width() as usize, img.height() as usize, img.into_raw()));
    }
    jpeg_lossless_gray(b)
}

/// minimal lossless-JPEG (SOF3) decoder: Huffman-coded pixel differences
/// against the SOS-selected spatial predictor. Handles the simple Adobe
/// case — 8-bit, no subsampling, optional restart intervals.
fn jpeg_lossless_gray(b: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let (w, h, prec, planes) = jpeg_lossless_planes(b)?;
    let nc = planes.len();
    let scale = 255.0 / ((1i32 << prec.min(31)) - 1).max(1) as f32;
    let px = (0..w * h)
        .map(|i| {
            let s: i32 = planes.iter().map(|p| p[i]).sum();
            (s as f32 / nc as f32 * scale).round().clamp(0.0, 255.0) as u8
        })
        .collect();
    Some((w, h, px))
}

/// raw lossless-JPEG decode → (w, h, bit precision, per-channel sample
/// planes). Handles 8/12/16-bit precision and multi-component scans
/// (e.g. PatchMatch offset maps: 3×16-bit channels).
fn jpeg_lossless_planes(b: &[u8]) -> Option<(usize, usize, usize, Vec<Vec<i32>>)> {
    let be16 = |o: usize| -> Option<usize> {
        Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize)
    };
    let (mut w, mut h, mut nc, mut prec) = (0usize, 0usize, 0usize, 8usize);
    let mut dht: [Option<([u16; 16], Vec<u8>)>; 4] = Default::default();
    let mut dtd = [0usize; 8]; // per-component DC table id
    let (mut pred, mut pt, mut ri) = (0usize, 0u32, 0usize);
    let mut scan = &[][..];
    let mut o = 2usize; // skip SOI
    if b.get(..2)? != b"\xff\xd8" {
        return None;
    }
    while o + 4 <= b.len() {
        if b[o] != 0xff {
            o += 1;
            continue;
        }
        let m = b[o + 1];
        if m == 0x00 || (0xd0..=0xd9).contains(&m) {
            o += 2;
            continue;
        }
        let ln = be16(o + 2)?;
        let seg = &b[o + 4..(o + 2 + ln).min(b.len())];
        match m {
            0xc4 => {
                // DHT: possibly several tables in one segment
                let mut s = 0usize;
                while s + 17 <= seg.len() {
                    let id = (seg[s] & 0x0f) as usize;
                    let mut cnt = [0u16; 16];
                    for (c, &x) in cnt.iter_mut().zip(&seg[s + 1..s + 17]) {
                        *c = x as u16;
                    }
                    let ns: usize = cnt.iter().map(|&c| c as usize).sum();
                    let syms = seg[s + 17..(s + 17 + ns).min(seg.len())].to_vec();
                    if id < 4 {
                        dht[id] = Some((cnt, syms));
                    }
                    s += 17 + ns;
                }
            }
            0xc3 | 0xc1 => {
                prec = seg.get(0).copied()? as usize;
                h = be16(o + 5)?;
                w = be16(o + 7)?;
                nc = seg.get(5).copied()? as usize;
            }
            0xda => {
                // SOS: comps then Ss/Se/AhAl, then entropy data
                let ns = seg.first().copied()? as usize;
                for c in 0..ns {
                    dtd[c] = (seg[2 + c * 2] >> 4) as usize;
                }
                pred = seg[1 + ns * 2] as usize;
                pt = (seg[3 + ns * 2] & 0x0f) as u32;
                scan = &b[o + 2 + ln..];
                break;
            }
            0xdd => {
                ri = be16(o + 4)?;
            }
            _ => {}
        }
        o += 2 + ln;
    }
    if w == 0 || h == 0 || nc == 0 || scan.is_empty() {
        return None;
    }
    // canonical Huffman decoders: per length l, first code + symbol base
    let mut tabs: Vec<([i32; 17], [usize; 17], Vec<u8>, [u16; 16])> = Vec::new();
    for t in dht.iter().flatten() {
        let (cnt, syms) = t;
        let mut first = [0i32; 17];
        let mut base = [0usize; 17];
        let mut code = 0i32;
        let mut idx = 0usize;
        for l in 1..=16 {
            first[l] = code;
            base[l] = idx;
            idx += cnt[l - 1] as usize;
            code = (code + cnt[l - 1] as i32) << 1;
        }
        tabs.push((first, base, syms.clone(), *cnt));
    }

    // entropy-coded bit reader: handles FF00 byte stuffing; a real marker
    // (including restart markers, consumed by the caller loop) ends data
    let mut pos = 0usize;
    let mut acc = 0u64;
    let mut nbits = 0u32;
    macro_rules! bit {
        () => {{
            while nbits == 0 {
                let by = *scan.get(pos)?;
                pos += 1;
                if by == 0xff {
                    let nx = scan.get(pos).copied()?;
                    if nx == 0x00 {
                        pos += 1;
                    } else {
                        return None; // marker — treat as end
                    }
                }
                acc = (acc << 8) | by as u64;
                nbits += 8;
            }
            nbits -= 1;
            (acc >> nbits) & 1
        }};
    }
    // entropy data is pixel-major, components interleaved; each component
    // keeps its own predictor state and DC table
    let mut planes = vec![vec![0i32; w * h]; nc];
    let init = 1i32 << (prec - pt as usize - 1); // first-sample prediction
    if ri > 0 {
        return None; // restart intervals not needed by the small Adobe tiles
    }
    for y in 0..h {
        for x in 0..w {
            for (c, plane) in planes.iter_mut().enumerate() {
                let t = dtd[c].min(tabs.len().saturating_sub(1));
                let (first, base, syms, cnt) = &tabs[t];
                let (mut code, mut l) = (0i32, 0usize);
                let ssss = loop {
                    code = (code << 1) | bit!() as i32;
                    l += 1;
                    if l > 16 {
                        return None;
                    }
                    if code >= first[l] && (code - first[l]) < cnt[l - 1] as i32 {
                        break syms[base[l] + (code - first[l]) as usize] as i32;
                    }
                };
                let mut diff = 0i32;
                for _ in 0..ssss {
                    diff = (diff << 1) | bit!() as i32;
                }
                if ssss > 0 && diff < (1 << (ssss - 1)) {
                    diff -= (1 << ssss) - 1;
                }
                let ra = if x > 0 { plane[y * w + x - 1] } else { 0 };
                let rb = if y > 0 { plane[(y - 1) * w + x] } else { init };
                let rc = if x > 0 && y > 0 {
                    plane[(y - 1) * w + x - 1]
                } else {
                    rb
                };
                let p = if x == 0 && y == 0 {
                    init
                } else if y == 0 {
                    ra
                } else if x == 0 {
                    rb
                } else {
                    match pred {
                        1 => ra,
                        2 => rb,
                        3 => rc,
                        4 => ra + rb - rc,
                        5 => ra + (rb - rc) / 2,
                        6 => rb + (ra - rc) / 2,
                        _ => (ra + rb) / 2,
                    }
                };
                plane[y * w + x] = (p + diff) << pt;
            }
        }
    }
    Some((w, h, prec, planes))
}

/// Extract the blob-store mattes referenced by the recipe's windows:
/// writes `<stem>.slmask-<digest8>.png` next to `img` and sets
/// `mask_file`. Returns the number of bitmaps bound.
pub fn apply_blob_masks(
    blobs: &std::collections::HashMap<String, MaskBlob>,
    r: &mut crate::recipe::Recipe,
    img: &Path,
) -> usize {
    let mut n = 0;
    let Some(dir) = img.parent() else {
        return 0;
    };
    let stem = img
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("img")
        .to_string();
    for w in &mut r.windows {
        if w.mask_digest.is_empty() || (!w.mask_file.is_empty() && dir.join(&w.mask_file).exists())
        {
            continue;
        }
        let Some(mb) = blobs.get(&w.mask_digest.to_ascii_uppercase()) else {
            continue;
        };
        let Some((mw, mh, px)) = decode_mask_blob(mb) else {
            continue;
        };
        let name = format!(
            "{stem}.slmask-{}.png",
            &w.mask_digest[..8.min(w.mask_digest.len())]
        );
        let out = dir.join(&name);
        if image::save_buffer(&out, &px, mw, mh, image::ColorType::L8).is_ok() {
            w.mask_file = name;
            n += 1;
        }
    }
    // PatchMatch remove spots: pm_patch digest → 3×16-bit offset map,
    // written as a 16-bit RGB PNG so the u16 planes survive intact.
    for s in &mut r.pm_spots {
        if s.digest.is_empty() || (!s.file.is_empty() && dir.join(&s.file).exists()) {
            continue;
        }
        let Some(mb) = blobs.get(&s.digest.to_ascii_uppercase()) else {
            continue;
        };
        let Some((pw, ph, planes)) = decode_blob_rgb16(mb) else {
            continue;
        };
        if planes.len() < 2 {
            continue;
        }
        let mut rgb = Vec::with_capacity((pw * ph) as usize * 6);
        for i in 0..(pw * ph) as usize {
            // image's PNG encoder expects native-endian u16 samples
            rgb.extend_from_slice(&planes[0][i].to_ne_bytes());
            rgb.extend_from_slice(&planes[1][i].to_ne_bytes());
            rgb.extend_from_slice(&planes.get(2).map(|p| p[i]).unwrap_or(0).to_ne_bytes());
        }
        let name = format!("{stem}.slpm-{}.png", &s.digest[..8.min(s.digest.len())]);
        let out = dir.join(&name);
        if image::save_buffer(&out, &rgb, pw, ph, image::ColorType::Rgb16).is_ok() {
            s.file = name;
            n += 1;
        }
    }
    // serial stages can carry masked corrections too
    for s in &mut r.stages {
        n += apply_blob_masks(blobs, &mut s.params, img);
    }
    n
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
    let mut rad_y: Option<f64> = None;
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
                        rad_y = get_num(mi, &["SizeY"]);
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
    // PatchMatch remove (spotType heal_patchmatch): the catalog carries the
    // pm_* block — target/search rects in sensor px, a frame origin, and the
    // pm_patch digest keying the computed offset map in .lrcat-data. Emit it
    // as crs attrs so the recipe can bind the extracted bitmap.
    let mut pm = String::new();
    if get_str(t, &["SpotType", "spotType"]) == Some("heal_patchmatch") {
        let g = |n: &str| get_num(t, &[n]).unwrap_or(0.0);
        if let Some(dig) = get_str(t, &["pm_patch"]) {
            let ry = rad_y.unwrap_or_else(|| rad.unwrap_or(0.01));
            pm = format!(
                " crs:PmTarget=\"{},{},{},{}\" crs:PmSearch=\"{},{},{},{}\" crs:PmFrame=\"{},{}\" crs:PmEllipse=\"{:.6},{:.6},{:.6},{:.6}\" crs:PmPatch=\"{}\" crs:PmOpacity=\"{:.4}\"",
                g("pm_target_left"), g("pm_target_top"), g("pm_target_right"), g("pm_target_bottom"),
                g("pm_search_left"), g("pm_search_top"), g("pm_search_right"), g("pm_search_bottom"),
                g("pm_whole_image_left"), g("pm_whole_image_top"),
                dx, dy, rad.unwrap_or(0.01), ry,
                esc(dig),
                get_num(t, &["Opacity", "opacity"]).unwrap_or(1.0),
            );
        }
    }
    // source: lowercase dup fields, else SourceX + OffsetY
    let mut srcx = get_num(t, &["sourceX", "SourceX"]).unwrap_or(dx);
    let mut srcy = get_num(t, &["sourceY", "SourceY", "OffsetY"]).unwrap_or(dy);
    if (srcx == 0.0 && srcy == 0.0) || (srcx == dx && srcy == dy) {
        // patchmatch/auto-computed source — when the pm_patch map is present
        // the renderer uses it; otherwise approximate by cloning sideways
        srcx = if dx > 0.8 {
            dx - 3.0 * diam
        } else {
            dx + 3.0 * diam
        };
        srcy = dy;
    }
    let method = get_str(t, &["Method"]).unwrap_or("clone");
    format!(
        "<rdf:li crs:What=\"Retouch\" crs:Method=\"{}\" crs:SpotX=\"{:.6}\" crs:SpotY=\"{:.6}\" crs:SpotDiam=\"{:.6}\" crs:SourceX=\"{:.6}\" crs:SourceY=\"{:.6}\"{}/>",
        esc(method),
        dx, dy, diam, srcx, srcy,
        pm
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

    /// pm patchmaps are written as 16-bit RGB PNGs; the image crate's PNG
    /// encoder wants native-endian u16 samples, so a `to_be_bytes` write
    /// round-trips as byte-swapped garbage. Pin the convention down.
    #[test]
    fn rgb16_png_roundtrip() {
        let vals: Vec<u16> = (0..48 * 3).map(|i| (i * 1377) as u16 ^ 0x5A5A).collect();
        let mut rgb = Vec::new();
        for &v in &vals {
            rgb.extend_from_slice(&v.to_ne_bytes());
        }
        let p = std::env::temp_dir().join("sl_rgb16_rt.png");
        image::save_buffer(&p, &rgb, 16, 3, image::ColorType::Rgb16).unwrap();
        let back = image::open(&p).unwrap().to_rgb16();
        assert_eq!(&back.into_raw()[..], &vals[..]);
        std::fs::remove_file(&p).ok();
    }

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

#[cfg(test)]
mod blob_tests {
    use super::*;

    /// catalog Lua → XMP → window: MaskDigest/Origin/WholeImageArea carry
    /// the blob-store binding and the bitmap's mask-space geometry.
    #[test]
    fn lua_mask_digest_and_geometry() {
        let r = crate::xmp::crs_to_recipe(&lua_to_xmp(
            "s = { MaskGroupBasedCorrections = { { CorrectionActive = true, \
             CorrectionAmount = 1, LocalExposure2012 = -0.5, What = \"Correction\", \
             CorrectionMasks = { { MaskSubType = 1, MaskValue = 1, \
             What = \"Mask/Image\", MaskDigest = \"07AAE06235F86CE296BFE0189999E42A\", \
             Origin = \"444,20\", \
             WholeImageArea = \"0/1,0/1,1920/1,2879/1\" } } } } }",
        ));
        assert_eq!(r.windows.len(), 1);
        let w = &r.windows[0];
        assert_eq!(w.kind, "subject");
        assert_eq!(w.mask_digest, "07AAE06235F86CE296BFE0189999E42A");
        assert_eq!(w.mask_org, [444.0, 20.0]);
        assert_eq!(w.mask_area, [2879.0, 1920.0]);
    }

    /// MaskDigest → SST record [1, blob_file_no, off, len] → embedded TIFF
    /// in NNNNNN.blob decodes to the stored grayscale matte.
    #[test]
    fn sst_digest_to_blob_tiff() {
        // 2x2 L8 TIFF via the image encoder
        let mut tiff = Vec::new();
        image::DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(2, 2, vec![0, 200, 120, 255]).unwrap(),
        )
        .write_to(
            &mut std::io::Cursor::new(&mut tiff),
            image::ImageFormat::Tiff,
        )
        .unwrap();

        let tmp = std::env::temp_dir().join(format!("sl_blob_{}", std::process::id()));
        let data_dir = tmp.join("cat.lrcat-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(tmp.join("cat.lrcat"), b"").unwrap();
        // blob record: 16B header + TIFF payload (value off = 0 → +16)
        let mut blob = vec![0u8; 16];
        blob.extend_from_slice(&tiff);
        std::fs::write(data_dir.join("000042.blob"), &blob).unwrap();
        // SST record: [shared=0, key_len=40, vlen] key trailer + [1,fno,off,len]
        let mut sst = Vec::new();
        sst.push(0u8);
        sst.push(40u8);
        // value len covers the whole blob record (16B header + payload);
        // varint-encode each field properly (len may exceed 127)
        let mut val = vec![1u8, 42u8, 0u8];
        let mut l = 16 + tiff.len() as u64;
        loop {
            let mut b = (l & 0x7f) as u8;
            l >>= 7;
            if l > 0 {
                b |= 0x80;
            }
            val.push(b);
            if l == 0 {
                break;
            }
        }
        sst.push(val.len() as u8);
        sst.extend_from_slice(b"07AAE06235F86CE296BFE0189999E42A");
        sst.extend_from_slice(&[0x11, 0, 0, 0, 0, 0, 0, 0]);
        sst.extend_from_slice(&val);
        std::fs::write(data_dir.join("000001.sst"), &sst).unwrap();

        let idx = blob_index(&tmp.join("cat.lrcat"));
        let mb = idx
            .get("07AAE06235F86CE296BFE0189999E42A")
            .expect("digest resolves via SST");
        let (w, h, px) = decode_mask_blob(mb).expect("TIFF decodes");
        assert_eq!((w, h), (2, 2));
        assert_eq!(px, vec![0, 200, 120, 255]);
        std::fs::remove_dir_all(&tmp).ok();
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    /// real-catalog coverage: every MaskDigest in the DB must resolve to a
    /// decodable blob-store TIFF. Skips when the catalog isn't present.
    #[test]
    fn real_catalog_mask_index() {
        let dir = std::env::var("SL_LRCAT_DIR").unwrap_or_else(|_| {
            r"C:\Users\stu12\OneDrive - 愛知産業大学\画像\Lightroom".to_string()
        });
        for name in [
            "Lightroom Catalog-v13-2.lrcat",
            "Lightroom Catalog-v13-3.lrcat",
        ] {
            let cat = std::path::Path::new(&dir).join(name);
            if !cat.exists() {
                continue;
            }
            let idx = blob_index(&cat);
            eprintln!("{name}: {} digest→blob records", idx.len());
            let out_dir = std::env::temp_dir().join(format!(
                "sl_masks_{}",
                name.trim_end_matches(".lrcat").replace(' ', "_")
            ));
            std::fs::create_dir_all(&out_dir).ok();
            let mut ok = 0;
            let mut bad = Vec::new();
            for (d, mb) in &idx {
                match decode_mask_blob(mb) {
                    Some((w, h, px)) => {
                        if px.len() != (w * h) as usize {
                            bad.push(format!("{d}: {w}x{h} len {}", px.len()));
                        } else {
                            ok += 1;
                            if let Some(img) = image::GrayImage::from_raw(w, h, px) {
                                img.save(out_dir.join(format!("{d}.png"))).ok();
                            }
                        }
                    }
                    None => bad.push(format!("{d}: undecodable")),
                }
            }
            eprintln!("  decode ok={ok} bad={bad:?} → {}", out_dir.display());
        }
    }

    /// diagnostic: decode a PatchMatch pm_patch TIFF at full fidelity and
    /// Inspect a dumped JXL tile directly: header info + per-frame render
    /// stats (SL_JXL_FILE env, defaults to the 03B7DAA4 tile in temp).
    #[test]
    #[ignore]
    fn jxl_inspect() {
        let path = std::env::var("SL_JXL_FILE")
            .unwrap_or_else(|_| r"C:\Users\stu12\AppData\Local\Temp\03B7DAA4.jxl".into());
        let data = std::fs::read(&path).unwrap();
        let img = jxl_oxide::JxlImage::read_with_defaults(std::io::Cursor::new(&data)).unwrap();
        let h = img.image_header();
        eprintln!(
            "size {}x{} bit_depth {:?} orient {:?} xyb? color={:?} frames_loaded={}",
            h.size.width,
            h.size.height,
            h.metadata.bit_depth,
            h.metadata.orientation,
            h.metadata.colour_encoding,
            img.num_loaded_frames()
        );
        let encs: Vec<(&str, Option<jxl_oxide::EnumColourEncoding>)> = vec![
            ("default", None),
            (
                "srgb",
                Some(jxl_oxide::EnumColourEncoding::srgb(
                    jxl_oxide::RenderingIntent::Relative,
                )),
            ),
            (
                "srgb_linear",
                Some(jxl_oxide::EnumColourEncoding::srgb_linear(
                    jxl_oxide::RenderingIntent::Relative,
                )),
            ),
        ];
        for (tag, enc) in encs {
            let mut img =
                jxl_oxide::JxlImage::read_with_defaults(std::io::Cursor::new(&data)).unwrap();
            if let Some(e) = enc {
                img.request_color_encoding(e);
            }
            let r = img.render_frame(0).unwrap();
            let mut dump: Vec<u8> = Vec::new();
            for (c, fb) in r.image_planar().iter().enumerate().take(4) {
                let b = fb.buf();
                let (mn, mx) = b
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
                // spatial roughness: mean |dx|
                let w = fb.width();
                let rough: f32 = b
                    .chunks_exact(w)
                    .map(|row| row.windows(2).map(|w2| (w2[1] - w2[0]).abs()).sum::<f32>())
                    .sum::<f32>()
                    / (b.len() as f32);
                eprintln!(
                    "{tag} ch{c} {}x{} min {mn:.4} max {mx:.4} rough {rough:.4}",
                    fb.width(),
                    fb.height()
                );
                for &v in b {
                    dump.extend_from_slice(&v.to_le_bytes());
                }
            }
            std::fs::write(path.replace(".jxl", &format!("_{tag}.bin")), &dump).unwrap();
        }
    }

    /// dump per-channel stats (offset-map structure analysis).
    /// SL_PM_DIGEST = hex digest, searches every *.lrcat-data blob.
    #[test]
    #[ignore]
    fn pm_patch_dump() {
        let dig = std::env::var("SL_PM_DIGEST")
            .unwrap_or_else(|_| "183CD5D7162DE26E52CDF9BFB1EB0569".into());
        let dir = std::env::var("SL_LRCAT_DIR").unwrap_or_else(|_| {
            r"C:\Users\stu12\OneDrive - 愛知産業大学\画像\Lightroom".to_string()
        });
        let mut raw = Vec::new();
        let mut pos = 0usize;
        for dd in std::fs::read_dir(&dir).unwrap().flatten() {
            let d = dd.path();
            if d.extension().and_then(|s| s.to_str()) != Some("lrcat-data") {
                continue;
            }
            eprintln!("scan dir {d:?}");
            for f in std::fs::read_dir(&d).unwrap().flatten() {
                let p = f.path();
                if p.extension().and_then(|s| s.to_str()) != Some("blob") {
                    continue;
                }
                let Ok(r) = std::fs::read(&p) else {
                    eprintln!("  read fail {p:?}");
                    continue;
                };
                if let Some(i) = r.windows(32).position(|w| w == dig.as_bytes()) {
                    // TIFF header follows the record prefix
                    if let Some(t) = r[i..]
                        .windows(4)
                        .position(|w| w == b"II*\x00")
                        .map(|x| i + x)
                    {
                        (raw, pos) = (r, t);
                    }
                }
                if !raw.is_empty() {
                    break;
                }
            }
            if !raw.is_empty() {
                break;
            }
        }
        assert!(!raw.is_empty(), "digest {dig} not found in blob stores");
        let t = &raw[pos..];
        eprintln!("record at blob offset {pos}");
        // compare the production decode path (blob_index + decode_blob_rgb16)
        let cat = std::env::var("SL_LRCAT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::Path::new(&dir).join("Lightroom Catalog-v13-3.lrcat"));
        if cat.exists() {
            let idx = blob_index(&cat);
            eprintln!("index has {} digests", idx.len());
            if let Some(mb) = idx.get(&dig) {
                eprintln!(
                    "resolved: {:?} off={} len={} inline={}",
                    mb.path.file_name().unwrap_or_default(),
                    mb.off,
                    mb.len,
                    mb.inline.len()
                );
                if let Some(t) = blob_tiff(mb) {
                    eprintln!(
                        "blob_tiff len {} head {:02x?}",
                        t.len(),
                        &t[..8.min(t.len())]
                    );
                    if let Some(l) = tiff_layout(&t) {
                        eprintln!(
                            "layout: w={} h={} comp={} tiles={}",
                            l.0,
                            l.1,
                            l.2,
                            l.5.len()
                        );
                    } else {
                        eprintln!("tiff_layout FAILED");
                    }
                }
                if let Some((w2, h2, pl2)) = decode_blob_rgb16(mb) {
                    eprintln!("decode_blob_rgb16 {w2}x{h2} ch={}", pl2.len());
                    for (c, p) in pl2.iter().enumerate() {
                        let rough: f64 = p
                            .chunks_exact(w2 as usize)
                            .map(|row| {
                                row.windows(2)
                                    .map(|w| (w[1] as f64 - w[0] as f64).abs())
                                    .sum::<f64>()
                            })
                            .sum::<f64>()
                            / (p.len() as f64 * 65535.0);
                        eprintln!(
                            "  ch{c}: min={} max={} rough={rough:.4}",
                            p.iter().min().unwrap(),
                            p.iter().max().unwrap()
                        );
                    }
                    // dump u16 planes for offline compare
                    let out2 = std::env::temp_dir().join(format!("blob_{}.bin", &dig[..8]));
                    let mut b2 = Vec::new();
                    for p in &pl2 {
                        for &v in p {
                            b2.extend(v.to_le_bytes());
                        }
                    }
                    std::fs::write(&out2, &b2).ok();
                    eprintln!("  blob planes → {}", out2.display());
                } else {
                    eprintln!("decode_blob_rgb16 FAILED");
                }
            }
        }
        // payload: FF D8 JPEG or FF 0A JPEG XL codestream
        let js = t
            .windows(2)
            .position(|w| w == b"\xff\xd8" || w == b"\xff\x0a")
            .expect("no codec start");
        let is_jpeg = t[js] == 0xff && t[js + 1] == 0xd8;
        let (w, h, planes) = if is_jpeg {
            let (w, h, _p, pl) = jpeg_lossless_planes(&t[js..]).expect("decode pm_patch jpeg");
            (w, h, pl)
        } else {
            let mut img = jxl_oxide::JxlImage::read_with_defaults(std::io::Cursor::new(&t[js..]))
                .expect("jxl header");
            let render = img.render_frame(0).expect("jxl frame");
            let planes: Vec<Vec<i32>> = render
                .image_planar()
                .iter()
                .map(|fb| fb.buf().iter().map(|&v| (v * 65535.0) as i32).collect())
                .collect();
            let (w, h) = (
                render.image_planar()[0].width(),
                render.image_planar()[0].height(),
            );
            (w, h, planes)
        };
        eprintln!("pm_patch {w}x{h} channels={}", planes.len());
        for (c, p) in planes.iter().enumerate() {
            let (mn, mx) = (*p.iter().min().unwrap(), *p.iter().max().unwrap());
            eprintln!(
                "  ch{c}: min={mn} max={mx} center={} corner={}",
                p[(h / 2) * w + w / 2],
                p[0]
            );
        }
        // dump channel grids for structural analysis
        for (c, p) in planes.iter().enumerate() {
            let mut s = String::new();
            for y in (0..h).step_by((h / 12).max(1)) {
                for x in (0..w).step_by((w / 16).max(1)) {
                    s += &format!("{:6} ", p[y * w + x]);
                }
                s += "\n";
            }
            eprintln!("ch{c} grid:\n{s}");
        }
        // raw planes for offline analysis: [w:u32 h:u32 nc:u32] + i32 data
        let out = std::env::temp_dir().join(format!("pm_{}.bin", &dig[..8]));
        let mut b = Vec::new();
        b.extend((w as u32).to_le_bytes());
        b.extend((h as u32).to_le_bytes());
        b.extend((planes.len() as u32).to_le_bytes());
        for p in &planes {
            for &v in p {
                b.extend(v.to_le_bytes());
            }
        }
        std::fs::write(&out, &b).ok();
        eprintln!("planes → {}", out.display());
    }
}
