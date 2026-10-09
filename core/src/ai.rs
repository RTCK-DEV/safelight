// AI features via ONNX Runtime: real-world denoise (SCUNet, MIT) and subject
// detection (U-2-Net, Apache-2.0). The ONNX Runtime dylib and model weights are
// resolved from the app bundle / ~/.safelight/ai, downloading from public sources
// on first use. Everything degrades gracefully: no runtime or no model → error.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const ORT_RELEASE_URL: &str =
    "https://github.com/microsoft/onnxruntime/releases/download/v1.22.0/onnxruntime-osx-arm64-1.22.0.tgz";
const SCUNET_BASE: &str = "https://huggingface.co/Heliosoph/scunet-onnx/resolve/main";
const U2NET_BASE: &str = "https://huggingface.co/Heliosoph/u2net-onnx/resolve/main";

// SCUNet is a fully-convolutional U-Net with x8 downsampling — pad to /8.
const SCUNET_TILE: usize = 320;
const SCUNET_OVERLAP: usize = 48;
const U2NET_SIZE: usize = 320;

pub fn ai_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SAFELIGHT_AI_DIR") {
        return PathBuf::from(p);
    }
    crate::catalog::app_home().join("ai")
}

fn download(url: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = dest.with_extension("part");
    let mut resp = ureq::get(url)
        .call()
        .with_context(|| format!("download {url}"))?;
    let mut out = std::fs::File::create(&tmp)?;
    std::io::copy(&mut resp.body_mut().as_reader(), &mut out)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// Locate (or fetch) libonnxruntime.dylib.
fn ort_dylib() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("ORT_DYLIB_PATH") {
        if Path::new(&p).exists() {
            return Ok(PathBuf::from(p));
        }
    }
    // Bundled inside the .app next to the executable / Frameworks.
    if let Ok(exe) = std::env::current_exe() {
        for cand in [
            exe.parent()
                .map(|p| p.join("../Frameworks/libonnxruntime.dylib")),
            exe.parent().map(|p| p.join("libonnxruntime.dylib")),
        ]
        .into_iter()
        .flatten()
        {
            if cand.exists() {
                return Ok(cand);
            }
        }
    }
    let dir = ai_dir();
    let lib = dir.join("libonnxruntime.dylib");
    if lib.exists() {
        return Ok(lib);
    }
    // Fetch the release tarball and extract just the dylib.
    let tgz = dir.join("ort.tgz");
    download(ORT_RELEASE_URL, &tgz)?;
    let status = std::process::Command::new("tar")
        .args([
            "xzf",
            &tgz.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
            "--strip-components=2",
            "onnxruntime-osx-arm64-1.22.0/lib/libonnxruntime.1.22.0.dylib",
        ])
        .status()?;
    if !status.success() {
        bail!("tar extract onnxruntime failed");
    }
    let extracted = dir.join("libonnxruntime.1.22.0.dylib");
    if extracted.exists() {
        std::fs::rename(&extracted, &lib)?;
    }
    let _ = std::fs::remove_file(&tgz);
    if lib.exists() {
        Ok(lib)
    } else {
        bail!("libonnxruntime not present after download")
    }
}

fn model_file(names: &[&str], urls: &[String]) -> Result<PathBuf> {
    let dir = ai_dir();
    if let Ok(extra) = std::env::var("SAFELIGHT_MODEL_DIR") {
        let p = PathBuf::from(extra).join(names[0]);
        if p.exists() {
            return Ok(p);
        }
    }
    let main = dir.join(names[0]);
    if names.iter().all(|n| dir.join(n).exists()) {
        return Ok(main);
    }
    for (n, u) in names.iter().zip(urls.iter()) {
        let d = dir.join(n);
        if !d.exists() {
            download(u, &d)?;
        }
    }
    Ok(main)
}

fn scunet_path() -> Result<PathBuf> {
    let p = model_file(
        &[
            "scunet_color_real_psnr.onnx",
            "scunet_color_real_psnr.onnx.data",
        ],
        &[
            format!("{SCUNET_BASE}/scunet_color_real_psnr.onnx"),
            format!("{SCUNET_BASE}/scunet_color_real_psnr.onnx.data"),
        ],
    )?;
    static_version(&p)
}

/// CoreML EP is useless with a model's dynamic input shape ({-1,3,-1,-1}):
/// it either rejects every node (static-shapes-only mode) or builds a
/// gigantic flexible-shape model that gets jetsam-killed on first run.
/// We always feed one fixed tensor shape, so bake the input dims into a
/// static sibling file once (same .data external file is shared).
fn static_version(p: &Path) -> Result<PathBuf> {
    let name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .replace(".onnx", ".static.onnx");
    let stat = p.with_file_name(name);
    if stat.exists() {
        return Ok(stat);
    }
    match make_static_input(&p, &stat) {
        Ok(()) => Ok(stat),
        Err(e) => {
            eprintln!("[ai] static-shape patch failed ({e}); using dynamic model");
            Ok(p.to_path_buf())
        }
    }
}

// --- minimal protobuf surgery ------------------------------------------------
// ONNX ModelProto field 7 = graph; GraphProto field 11 = input (ValueInfo);
// ValueInfo field 2 = type -> TypeProto field 1 = tensor_type -> field 2 =
// shape -> repeated field 1 = dim (field 1 = dim_value i64, field 2 = dim_param).

fn varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut s = 0;
    while *i < buf.len() && s < 70 {
        let b = buf[*i];
        *i += 1;
        v |= ((b & 0x7f) as u64) << s;
        if b & 0x80 == 0 {
            return Some(v);
        }
        s += 7;
    }
    None
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// Split a serialized proto message into (field_no, wire, payload) fields.
/// `payload` is the bare value bytes: for wire 2 it excludes the length
/// varint, for wire 0 it is the varint bytes, for 5/1 the fixed bytes.
fn fields(buf: &[u8]) -> Vec<(u32, u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let Some(tag) = varint(buf, &mut i) else {
            break;
        };
        let no = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        let start = i;
        let mut vstart = i;
        match wire {
            0 => {
                let _ = varint(buf, &mut i);
            }
            2 => {
                let Some(l) = varint(buf, &mut i) else { break };
                vstart = i;
                i = i.saturating_add(l as usize);
            }
            5 => i += 4,
            1 => i += 8,
            _ => break,
        }
        if i > buf.len() {
            break;
        }
        let lo = if wire == 2 { vstart } else { start };
        out.push((no, wire, buf[lo..i].to_vec()));
    }
    out
}

fn emit_field(out: &mut Vec<u8>, no: u32, wire: u8, val: &[u8]) {
    put_varint(out, ((no as u64) << 3) | wire as u64);
    if wire == 2 {
        put_varint(out, val.len() as u64);
    }
    out.extend_from_slice(val);
}

/// Build a TypeProto for a fixed NCHW tensor (field 1 = tensor_type -> field 1
/// elem_type=f32, field 2 = shape).
fn static_nchw_type(n: u64, c: u64, h: u64, w: u64) -> Vec<u8> {
    let mut shape = Vec::new();
    for d in [n, c, h, w] {
        let mut dim = Vec::new();
        emit_field(&mut dim, 1, 0, &{
            let mut v = Vec::new();
            put_varint(&mut v, d);
            v
        });
        emit_field(&mut shape, 1, 2, &dim);
    }
    let mut tensor = Vec::new();
    emit_field(&mut tensor, 1, 0, &[1]); // elem_type = FLOAT
    emit_field(&mut tensor, 2, 2, &shape);
    let mut ty = Vec::new();
    emit_field(&mut ty, 1, 2, &tensor);
    ty
}

/// Does this ValueInfoProto describe a 4-dim float tensor?
fn is_4d_input(value_info: &[u8]) -> bool {
    for (no, _wire, val) in fields(value_info) {
        if no != 2 {
            continue;
        }
        // TypeProto -> tensor_type -> shape -> count dims
        for (tno, _, tval) in fields(&val) {
            if tno != 1 {
                continue;
            }
            for (sno, _, sval) in fields(&tval) {
                if sno != 2 {
                    continue;
                }
                let dims = fields(&sval)
                    .into_iter()
                    .filter(|(n, _, _)| *n == 1)
                    .count();
                return dims == 4;
            }
        }
    }
    false
}

/// Rewrite the model's 4-D graph inputs to the fixed tile shape.
fn make_static_input(src: &Path, dst: &Path) -> Result<()> {
    let bytes = std::fs::read(src)?;
    let mut model_out = Vec::with_capacity(bytes.len());
    let mut patched = 0usize;
    for (no, wire, val) in fields(&bytes) {
        if no != 7 || wire != 2 {
            emit_field(&mut model_out, no, wire, &val);
            continue;
        }
        // GraphProto: patch each 4-dim input ValueInfo
        let mut graph_out = Vec::with_capacity(val.len());
        for (gno, gwire, gval) in fields(&val) {
            if gno == 11 && gwire == 2 && is_4d_input(&gval) {
                // ValueInfoProto: keep name (field 1), replace type (field 2)
                let mut vi_out = Vec::new();
                for (vno, vwire, vval) in fields(&gval) {
                    if vno == 2 {
                        emit_field(
                            &mut vi_out,
                            2,
                            2,
                            &static_nchw_type(1, 3, SCUNET_TILE as u64, SCUNET_TILE as u64),
                        );
                    } else {
                        emit_field(&mut vi_out, vno, vwire, &vval);
                    }
                }
                emit_field(&mut graph_out, gno, gwire, &vi_out);
                patched += 1;
            } else {
                emit_field(&mut graph_out, gno, gwire, &gval);
            }
        }
        emit_field(&mut model_out, no, wire, &graph_out);
    }
    if patched == 0 {
        bail!("no 4-dim graph input found in {src:?}");
    }
    std::fs::write(dst, &model_out)?;
    eprintln!("[ai] wrote static-shape model {dst:?} ({patched} input(s) patched)");
    Ok(())
}

fn u2net_path() -> Result<PathBuf> {
    let p = model_file(&["u2netp.onnx"], &[format!("{U2NET_BASE}/u2netp.onnx")])?;
    static_version(&p)
}

struct Sessions {
    // ort::session::Session::run takes &mut self, so denoise workers each
    // get their own session from this pool (created once, reused).
    scunet: Vec<ort::session::Session>,
    u2net: Option<ort::session::Session>,
}

static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();
static ORT_INIT: OnceLock<Result<()>> = OnceLock::new();

fn ort_ready() -> Result<()> {
    eprintln!("[ai] ort_ready");
    match ORT_INIT.get_or_init(|| {
        eprintln!("[ai] dylib resolve");
        let lib = ort_dylib()?;
        eprintln!("[ai] init_from {lib:?}");
        ort::init_from(lib)
            .map_err(|e| anyhow!("ort init: {e}"))?
            .commit();
        eprintln!("[ai] ort committed");
        Ok(())
    }) {
        Ok(()) => Ok(()),
        Err(e) => Err(anyhow!("{e}")),
    }
}

/// Load (once) the ORT runtime and compile a model session. Call this
/// BEFORE allocating big image buffers so graph compilation happens at low
/// memory pressure.
pub fn prewarm() -> Result<()> {
    with_scunet_pool(1, |_| Ok(()))
}

fn make_session(which: u8) -> Result<ort::session::Session> {
    let path = if which == 0 {
        scunet_path()?
    } else {
        u2net_path()?
    };
    // SCUNet (which 0) always runs on the CPU EP: ORT-CoreML partitioning of
    // its ~3000-node graph (many rank-6 attention reshapes CoreML rejects)
    // explodes to 13+ GB and gets jetsam-killed on the first inference —
    // verified on this machine. U-2-Net (1) is a small conv-only model and
    // may use CoreML fine.
    let allow_coreml = which == 1;
    let build = |use_coreml: bool| -> Result<ort::session::Session> {
        let use_coreml = use_coreml && allow_coreml;
        let mut builder =
            ort::session::Session::builder().map_err(|e| anyhow!("ort session builder: {e}"))?;
        if std::env::var_os("SAFELIGHT_ORT_VERBOSE").is_some() {
            builder = builder
                .with_log_level(ort::logging::LogLevel::Verbose)
                .map_err(|e| anyhow!("ort log: {e}"))?;
        }
        if std::env::var_os("SAFELIGHT_NO_ARENA").is_some() {
            builder = builder
                .with_memory_pattern(false)
                .map_err(|e| anyhow!("ort mem: {e}"))?;
        }
        #[cfg(target_os = "macos")]
        if use_coreml && std::env::var_os("SAFELIGHT_NO_COREML").is_none() {
            let cache = ai_dir().join("coreml-cache");
            let _ = std::fs::create_dir_all(&cache);
            let units = match std::env::var("SAFELIGHT_COREML_UNITS")
                .unwrap_or_default()
                .as_str()
            {
                "gpu" => ort::ep::coreml::ComputeUnits::CPUAndGPU,
                "ane" => ort::ep::coreml::ComputeUnits::CPUAndNeuralEngine,
                _ => ort::ep::coreml::ComputeUnits::All,
            };
            builder = builder
                .with_execution_providers([ort::ep::CoreML::default()
                    .with_compute_units(units)
                    // newer program format: more ops + lighter runtime than
                    // the legacy NeuralNetwork format
                    .with_model_format(ort::ep::coreml::ModelFormat::MLProgram)
                    .with_low_precision_accumulation_on_gpu(true)
                    // models are patched to fixed shapes at first use — one
                    // specialized graph per session, no per-shape recompiles
                    .with_static_input_shapes(true)
                    // compiled graph cache: avoids recompiling on every run
                    .with_model_cache_dir(cache.to_string_lossy().to_string())
                    .build()])
                .map_err(|e| anyhow!("ort EP: {e}"))?;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = use_coreml;
        // a pool of N sessions runs side by side; keep each session's
        // intra-op pool modest so workers don't oversubscribe cores
        builder = builder
            .with_intra_threads(4)
            .map_err(|e| anyhow!("ort threads: {e}"))?;
        eprintln!("[ai] commit_from_file {path:?} (coreml={use_coreml})");
        builder
            .commit_from_file(&path)
            .map_err(|e| anyhow!("ort commit {path:?}: {e}"))
    };
    match build(true) {
        Ok(s) => Ok(s),
        Err(e) => {
            eprintln!("[ai] coreml session failed ({e}); retrying CPU-only");
            build(false)
        }
    }
}

/// Lock the session pool, create sessions until at least `n` scunet
/// sessions exist, then hand the pool to `f` while still locked.
fn with_scunet_pool<F, T>(n: usize, f: F) -> Result<T>
where
    F: FnOnce(&mut Vec<ort::session::Session>) -> Result<T>,
{
    ort_ready()?;
    let cell = SESSIONS.get_or_init(|| {
        Mutex::new(Sessions {
            scunet: Vec::new(),
            u2net: None,
        })
    });
    let mut g = cell.lock().map_err(|e| anyhow!("ai lock: {e}"))?;
    while g.scunet.len() < n {
        g.scunet.push(make_session(0)?);
    }
    f(&mut g.scunet)
}

fn with_session<F, T>(which: u8, f: F) -> Result<T>
where
    F: FnOnce(&mut ort::session::Session) -> Result<T>,
{
    ort_ready()?;
    let cell = SESSIONS.get_or_init(|| {
        Mutex::new(Sessions {
            scunet: Vec::new(),
            u2net: None,
        })
    });
    let mut g = cell.lock().map_err(|e| anyhow!("ai lock: {e}"))?;
    if which == 0 {
        if g.scunet.is_empty() {
            g.scunet.push(make_session(0)?);
        }
        let s = g.scunet.first_mut().ok_or_else(|| anyhow!("no session"))?;
        f(s)
    } else {
        if g.u2net.is_none() {
            g.u2net = Some(make_session(1)?);
        }
        let s = g.u2net.as_mut().ok_or_else(|| anyhow!("no session"))?;
        f(s)
    }
}

fn srgb_encode(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(dead_code)]
fn srgb_decode(s: f32) -> f32 {
    let s = s.clamp(0.0, 1.0);
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Denoise an interleaved linear-RGB f32 buffer in place. The model domain is
/// sRGB [0,1]: we encode into `img`, run tiled inference, and leave the
/// denoised image sRGB-encoded in `img` (callers quantize/decode as needed).
pub fn denoise_rgb(img: &mut Vec<f32>, w: usize, h: usize) -> Result<()> {
    // linear -> sRGB in place
    for v in img.iter_mut() {
        *v = srgb_encode(*v);
    }
    let tiles = tile_grid(w, h, SCUNET_TILE, SCUNET_OVERLAP);
    let total = tiles.len();
    // SCUNet inference uses ~2GB workspace per session — cap the pool so
    // N sessions + image buffers stay well under jetsam limits (8-core
    // machine: 2 workers x 4 intra-op threads = 8 threads busy).
    let workers = (std::thread::available_parallelism()
        .map(|n| n.get() / 4)
        .unwrap_or(1))
    .clamp(1, 3);
    // one session per worker thread; each stays locked inside the pool mutex
    with_scunet_pool(workers, |pool| {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let acc = Mutex::new((vec![0f32; w * h * 3], vec![0f32; w * h]));
        let img_ref: &Vec<f32> = img;
        let result: Result<()> = std::thread::scope(|s| {
            let mut handles = Vec::new();
            let mut it = pool.iter_mut();
            for _ in 0..workers {
                let Some(sess) = it.next() else { break };
                let next = &next;
                let acc = &acc;
                let tiles = &tiles;
                handles.push(s.spawn(move || -> Result<()> {
                    loop {
                        let ti = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if ti >= total {
                            return Ok(());
                        }
                        if ti % 20 == 0 {
                            eprintln!("[ai] tile {ti}/{total}");
                        }
                        let (x0, y0, tw, th) = tiles[ti];
                        let patch = run_scunet(sess, img_ref, w, h, x0, y0, tw, th)?;
                        // accumulate under a short lock — ~1ms vs ~1.5s infer
                        let mut g = acc.lock().map_err(|e| anyhow!("acc: {e}"))?;
                        for ty in 0..th {
                            let wy = hann(ty, th);
                            for tx in 0..tw {
                                let wx = hann(tx, tw) * wy;
                                let gx = x0 + tx;
                                let gy = y0 + ty;
                                let gi = (gy * w + gx) * 3;
                                let pi = (ty * tw + tx) * 3;
                                for c in 0..3 {
                                    g.0[gi + c] += patch[pi + c] * wx;
                                }
                                g.1[gy * w + gx] += wx;
                            }
                        }
                    }
                }));
            }
            for hd in handles {
                match hd.join() {
                    Ok(r) => r?,
                    Err(_) => bail!("denoise worker panicked"),
                }
            }
            Ok(())
        });
        result?;
        let (acc, weight) = acc.into_inner().map_err(|e| anyhow!("acc: {e}"))?;
        for i in 0..w * h {
            let ww = weight[i].max(1e-6);
            for c in 0..3 {
                let idx = i * 3 + c;
                img[idx] = acc[idx] / ww;
            }
        }
        Ok(())
    })
}

fn hann(i: usize, n: usize) -> f32 {
    if n < 2 {
        return 1.0;
    }
    let x = i as f32 / (n - 1) as f32;
    0.5 - 0.5 * (x * std::f32::consts::TAU).cos()
}

fn tile_grid(w: usize, h: usize, tile: usize, overlap: usize) -> Vec<(usize, usize, usize, usize)> {
    let step = tile - overlap;
    let mut out = Vec::new();
    let mut y = 0usize;
    while y < h {
        let th = tile.min(h - y);
        let mut x = 0usize;
        while x < w {
            let tw = tile.min(w - x);
            out.push((x, y, tw, th));
            x += step.min(w.saturating_sub(x));
            if tw == w - x && x + tw >= w {
                break;
            }
            if x + tw >= w {
                break;
            }
        }
        y += step.min(h.saturating_sub(y));
        if y + th >= h {
            break;
        }
    }
    // dedup right/bottom edges produced by step clamping
    out.sort_unstable();
    out.dedup();
    out
}

fn run_scunet(
    s: &mut ort::session::Session,
    img: &[f32],
    w: usize,
    h: usize,
    x0: usize,
    y0: usize,
    tw: usize,
    th: usize,
) -> Result<Vec<f32>> {
    // all tiles run at one fixed shape (edge-padded) so an execution provider
    // compiles a single graph instead of one per edge tile size
    let pw = SCUNET_TILE;
    let ph = SCUNET_TILE;
    let mut input = vec![0f32; 3 * ph * pw];
    for c in 0..3 {
        for y in 0..ph {
            let sy = (y0 + y.min(th - 1)).min(h - 1);
            for x in 0..pw {
                let sx = (x0 + x.min(tw - 1)).min(w - 1);
                input[c * ph * pw + y * pw + x] = img[(sy * w + sx) * 3 + c];
            }
        }
    }
    let tensor = ort::value::Tensor::from_array(([1usize, 3, ph, pw], input))
        .map_err(|e| anyhow!("tensor: {e}"))?;
    let outputs = s
        .run(ort::inputs![tensor])
        .map_err(|e| anyhow!("run: {e}"))?;
    let (_shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow!("extract: {e}"))?;
    let data = data.to_vec();
    // data: [1,3,ph,pw]
    let mut out = vec![0f32; tw * th * 3];
    for y in 0..th {
        for x in 0..tw {
            for c in 0..3 {
                out[(y * tw + x) * 3 + c] = data[c * ph * pw + y * pw + x];
            }
        }
    }
    Ok(out)
}

/// U-2-Net salient-subject mask. Input: interleaved RGB8 at any size (we
/// downscale internally). Returns a w*h f32 mask in [0,1].
pub fn subject_mask(rgb8: &[u8], w: usize, h: usize) -> Result<Vec<f32>> {
    // downsample to 320x320 bilinear
    let mut small = vec![0f32; U2NET_SIZE * U2NET_SIZE * 3];
    for y in 0..U2NET_SIZE {
        let sy = (y * h / U2NET_SIZE).min(h - 1);
        for x in 0..U2NET_SIZE {
            let sx = (x * w / U2NET_SIZE).min(w - 1);
            let si = (sy * w + sx) * 3;
            let di = (y * U2NET_SIZE + x) * 3;
            for c in 0..3 {
                small[di + c] = rgb8[si + c] as f32 / 255.0;
            }
        }
    }
    // ImageNet normalize, NCHW
    let mean = [0.485f32, 0.456, 0.406];
    let std = [0.229f32, 0.224, 0.225];
    let mut input = vec![0f32; 3 * U2NET_SIZE * U2NET_SIZE];
    for c in 0..3 {
        for i in 0..U2NET_SIZE * U2NET_SIZE {
            input[c * U2NET_SIZE * U2NET_SIZE + i] = (small[i * 3 + c] - mean[c]) / std[c];
        }
    }
    let tensor = ort::value::Tensor::from_array(([1usize, 3, U2NET_SIZE, U2NET_SIZE], input))
        .map_err(|e| anyhow!("tensor: {e}"))?;
    let mask_small = with_session(1, |s| {
        let inputs = ort::inputs![tensor];
        let outputs = s.run(inputs).map_err(|e| anyhow!("run: {e}"))?;
        let (_shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow!("extract: {e}"))?;
        Ok(data.to_vec())
    })?;
    // normalize mask 0..1
    let (mn, mx) = mask_small
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let span = (mx - mn).max(1e-6);
    // bilinear upsample to w*h
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        let fy = y as f32 * (U2NET_SIZE - 1) as f32 / (h - 1).max(1) as f32;
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(U2NET_SIZE - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            let fx = x as f32 * (U2NET_SIZE - 1) as f32 / (w - 1).max(1) as f32;
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(U2NET_SIZE - 1);
            let tx = fx - x0 as f32;
            let a = mask_small[y0 * U2NET_SIZE + x0];
            let b = mask_small[y0 * U2NET_SIZE + x1];
            let c = mask_small[y1 * U2NET_SIZE + x0];
            let d = mask_small[y1 * U2NET_SIZE + x1];
            out[y * w + x] =
                (a * (1.0 - tx) + b * tx) * (1.0 - ty) + (c * (1.0 - tx) + d * tx) * ty;
        }
    }
    for v in out.iter_mut() {
        *v = (*v - mn) / span;
    }
    Ok(out)
}

/// Files used to cache denoised bases and AI masks next to the source photo.
/// Denoised base cache: JPEG q92 sRGB8. A 45MP lossless PNG is ~180MB/photo
/// which does not scale to real libraries; the cache is display-domain data
/// anyway, and linear-space blending against the noisy original adds natural
/// dither over any JPEG quantization.
pub fn denoise_cache_path(photo: &Path) -> PathBuf {
    photo.with_extension("safelight.aidn.jpg")
}

pub fn subject_cache_path(photo: &Path) -> PathBuf {
    photo.with_extension("safelight.aimask.png")
}
