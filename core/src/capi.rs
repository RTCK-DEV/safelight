//! C ABI surface consumed by the SwiftUI app and other frontends.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use crate::develop::RgbaImage;
use crate::engine::Engine;
use crate::recipe::{Recipe, Sidecar};

#[repr(C)]
pub struct AraImage {
    pub data: *mut u8,
    pub len: usize,
    pub width: u32,
    pub height: u32,
}

/// 256-bin x 4-channel (R,G,B,luma) histogram of the rendered output.
#[repr(C)]
pub struct AraHistogram {
    pub bins: [u32; 1024],
}

// process-global so errors raised on the engine worker thread are
// readable by whichever thread calls `araware_last_error` (thread_local
// made every UI error path read an empty string).
static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

fn set_err(e: &anyhow::Error) {
    *LAST_ERROR.lock().unwrap() =
        Some(CString::new(e.to_string()).unwrap_or_default());
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn into_raw_string(s: String) -> *mut c_char {
    CString::new(s)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

fn into_raw_image(img: RgbaImage) -> AraImage {
    let mut v = img.data.into_boxed_slice();
    let out = AraImage {
        data: v.as_mut_ptr(),
        len: v.len(),
        width: img.width,
        height: img.height,
    };
    std::mem::forget(v);
    out
}

fn null_image() -> AraImage {
    AraImage {
        data: std::ptr::null_mut(),
        len: 0,
        width: 0,
        height: 0,
    }
}

fn engine<'a>(e: *mut c_void) -> Option<&'a Engine> {
    if e.is_null() {
        None
    } else {
        Some(unsafe { &*(e as *const Engine) })
    }
}

#[no_mangle]
pub extern "C" fn araware_init() -> *mut c_void {
    match Engine::new() {
        Ok(e) => Box::into_raw(Box::new(e)) as *mut c_void,
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_free_engine(e: *mut c_void) {
    if !e.is_null() {
        drop(Box::from_raw(e as *mut Engine));
    }
}

#[no_mangle]
pub extern "C" fn araware_last_error() -> *mut c_char {
    let guard = LAST_ERROR.lock().unwrap();
    let bytes = guard.as_ref().map(|s| s.as_bytes()).unwrap_or(b"");
    CString::new(bytes)
        .map(CString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn araware_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_free_image(img: AraImage) {
    if !img.data.is_null() && img.len > 0 {
        drop(Box::from_raw(
            std::slice::from_raw_parts_mut(img.data, img.len) as *mut [u8],
        ));
    }
}

/// returns JSON array of assets
#[no_mangle]
pub unsafe extern "C" fn araware_scan_folder(e: *mut c_void, folder: *const c_char) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return std::ptr::null_mut();
    };
    match eng.scan(Path::new(&cstr(folder))) {
        Ok(list) => into_raw_string(serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())),
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_thumbnail(
    e: *mut c_void,
    path: *const c_char,
    max_px: u32,
) -> AraImage {
    let Some(eng) = engine(e) else {
        return null_image();
    };
    match eng.thumbnail(Path::new(&cstr(path)), max_px) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}

/// recipe_json: serialized Recipe; empty/null = defaults
#[no_mangle]
pub unsafe extern "C" fn araware_render(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
) -> AraImage {
    let Some(eng) = engine(e) else {
        return null_image();
    };
    let recipe = cstr(recipe_json);
    let recipe = if recipe.is_empty() {
        Recipe::default()
    } else {
        Recipe::from_json(&recipe).unwrap_or_default()
    };
    match eng.render(Path::new(&cstr(path)), &recipe, max_px) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}

/// like araware_render, and additionally fills `hist` (nullable) with the
/// output-image histogram.
#[no_mangle]
pub unsafe extern "C" fn araware_render_h(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
    hist: *mut AraHistogram,
) -> AraImage {
    let img = araware_render(e, path, recipe_json, max_px);
    if !hist.is_null() {
        let bins = if img.data.is_null() {
            [0u32; 1024]
        } else {
            let data = std::slice::from_raw_parts(img.data as *const u8, img.len);
            crate::develop::histogram(data)
        };
        (*hist).bins = bins;
    }
    img
}

/// render preview + fill caller buffers: wave 3*256*256, vec 256*256,
/// cie 256*256, hist 1024 (each nullable, skipped when null)
#[no_mangle]
pub unsafe extern "C" fn araware_scopes(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    max_px: u32,
    wave: *mut u32,
    vec: *mut u32,
    cie: *mut u32,
    hist: *mut u32,
) -> AraImage {
    let img = araware_render(e, path, recipe_json, max_px);
    if !img.data.is_null() {
        let data = std::slice::from_raw_parts(img.data as *const u8, img.len);
        // heap: these exceed a dispatch-queue worker's ~512KB stack
        let mut wv = vec![0u32; 196608];
        let mut vc = vec![0u32; 65536];
        let mut ce = vec![0u32; 65536];
        crate::develop::scopes(data, img.width, img.height, &mut wv, &mut vc, &mut ce);
        if !wave.is_null() {
            std::ptr::copy_nonoverlapping(wv.as_ptr(), wave, wv.len());
        }
        if !vec.is_null() {
            std::ptr::copy_nonoverlapping(vc.as_ptr(), vec, vc.len());
        }
        if !cie.is_null() {
            std::ptr::copy_nonoverlapping(ce.as_ptr(), cie, ce.len());
        }
        if !hist.is_null() {
            std::ptr::copy_nonoverlapping(crate::develop::histogram(data).as_ptr(), hist, 1024);
        }
    }
    img
}

/// full-res render for export; RGBA8 out
#[no_mangle]
pub unsafe extern "C" fn araware_export(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
) -> AraImage {
    araware_render(e, path, recipe_json, 0)
}

/// export with finishing options `opts_json`: {"long_edge": px, "sharpen": 0..1}
#[no_mangle]
pub unsafe extern "C" fn araware_export_opts(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
    opts_json: *const c_char,
) -> AraImage {
    let Some(eng) = engine(e) else {
        return AraImage {
            data: std::ptr::null_mut(),
            len: 0,
            width: 0,
            height: 0,
        };
    };
    let recipe: crate::recipe::Recipe =
        serde_json::from_str(&cstr(recipe_json)).unwrap_or_default();
    let opts: serde_json::Value =
        serde_json::from_str(&cstr(opts_json)).unwrap_or(serde_json::json!({}));
    match eng.export_opts(std::path::Path::new(&cstr(path)), &recipe, &opts) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            AraImage {
                data: std::ptr::null_mut(),
                len: 0,
                width: 0,
                height: 0,
            }
        }
    }
}

/// merge N source photos: `paths_json` = JSON array of paths,
/// `mode` = "hdr" | "focus". Renders each at full res with its own
/// sidecar recipe, aligns, merges → RGBA8.
#[no_mangle]
pub unsafe extern "C" fn araware_merge(
    e: *mut c_void,
    paths_json: *const c_char,
    mode: *const c_char,
) -> AraImage {
    let Some(eng) = engine(e) else {
        return AraImage {
            data: std::ptr::null_mut(),
            len: 0,
            width: 0,
            height: 0,
        };
    };
    let paths: Vec<String> =
        serde_json::from_str(&cstr(paths_json)).unwrap_or_default();
    let pvec: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
    match eng.merge(&pvec, &cstr(mode)) {
        Ok(img) => into_raw_image(img),
        Err(err) => {
            set_err(&err);
            AraImage {
                data: std::ptr::null_mut(),
                len: 0,
                width: 0,
                height: 0,
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_metadata(e: *mut c_void, path: *const c_char) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return std::ptr::null_mut();
    };
    match eng.metadata(Path::new(&cstr(path))) {
        Ok(v) => into_raw_string(v.to_string()),
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_sidecar_read(path: *const c_char) -> *mut c_char {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sp = crate::recipe::sidecar_path_for(p);
    let sc = if sp.exists() {
        crate::catalog::read_sidecar(&sp).unwrap_or_default()
    } else {
        crate::xmp::read_xmp(p).unwrap_or_default()
    };
    into_raw_string(serde_json::to_string(&sc).unwrap_or_else(|_| "{}".into()))
}

/// json: serialized Sidecar
#[no_mangle]
pub unsafe extern "C" fn araware_sidecar_write(path: *const c_char, json: *const c_char) -> i32 {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sc: Sidecar = match serde_json::from_str(&cstr(json)) {
        Ok(s) => s,
        Err(err) => {
            set_err(&err.into());
            return -1;
        }
    };
    match crate::catalog::write_sidecar(&crate::recipe::sidecar_path_for(p), &sc) {
        Ok(()) => {
            let _ = crate::xmp::write_xmp(p, &sc);
            0
        }
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_set_rating(
    e: *mut c_void,
    path: *const c_char,
    rating: i32,
) -> i32 {
    let Some(eng) = engine(e) else { return -1 };
    match eng.set_rating(Path::new(&cstr(path)), rating) {
        Ok(()) => 0,
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn araware_set_label(
    e: *mut c_void,
    path: *const c_char,
    label: *const c_char,
) -> i32 {
    let Some(eng) = engine(e) else { return -1 };
    match eng.set_label(Path::new(&cstr(path)), &cstr(label)) {
        Ok(()) => 0,
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

/// sidecar read/write for a virtual copy slot (0 = master)
#[no_mangle]
pub unsafe extern "C" fn araware_sidecar_read_v(path: *const c_char, vslot: i32) -> *mut c_char {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sp = crate::recipe::sidecar_path_for_v(p, vslot.max(0) as u32);
    let sc = if sp.exists() {
        crate::catalog::read_sidecar(&sp).unwrap_or_default()
    } else if vslot <= 0 {
        crate::xmp::read_xmp(p).unwrap_or_default()
    } else {
        Sidecar::default()
    };
    into_raw_string(serde_json::to_string(&sc).unwrap_or_else(|_| "{}".into()))
}

#[no_mangle]
pub unsafe extern "C" fn araware_sidecar_write_v(
    path: *const c_char,
    vslot: i32,
    json: *const c_char,
) -> i32 {
    let pstr = cstr(path);
    let p = Path::new(&pstr);
    let sc: Sidecar = match serde_json::from_str(&cstr(json)) {
        Ok(s) => s,
        Err(err) => {
            set_err(&err.into());
            return -1;
        }
    };
    match crate::catalog::write_sidecar(
        &crate::recipe::sidecar_path_for_v(p, vslot.max(0) as u32),
        &sc,
    ) {
        Ok(()) => {
            if vslot <= 0 {
                let _ = crate::xmp::write_xmp(p, &sc);
            }
            0
        }
        Err(err) => {
            set_err(&err);
            -2
        }
    }
}

/// Library organization command surface — one JSON dispatch covering
/// flags, keywords, stacks, virtual copies and collections.
/// `cmd_json` example: {"op":"coll_list"} — see Engine::library.
/// Returns a JSON string (or null + last_error on failure).
#[no_mangle]
pub unsafe extern "C" fn araware_library(e: *mut c_void, cmd_json: *const c_char) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return std::ptr::null_mut();
    };
    let cmd: serde_json::Value = match serde_json::from_str(&cstr(cmd_json)) {
        Ok(v) => v,
        Err(err) => {
            set_err(&err.into());
            return std::ptr::null_mut();
        }
    };
    match eng.library(&cmd) {
        Ok(v) => into_raw_string(v.to_string()),
        Err(err) => {
            set_err(&err);
            std::ptr::null_mut()
        }
    }
}

/// Auto-correction analysis -> JSON with suggested recipe values.
#[no_mangle]
pub unsafe extern "C" fn araware_auto_analyze(e: *mut c_void, path: *const c_char) -> *mut c_char {
    let eng = &*(e as *const Engine);
    match eng.auto_analyze(Path::new(&cstr(path))) {
        Ok(r) => match serde_json::to_string(&r) {
            Ok(j) => into_raw_string(j),
            Err(e) => {
                set_err(&e.into());
                std::ptr::null_mut()
            }
        },
        Err(e) => {
            set_err(&e);
            std::ptr::null_mut()
        }
    }
}

/// libraw reference render (sanity check / debugging)
#[no_mangle]
pub unsafe extern "C" fn araware_reference(path: *const c_char) -> AraImage {
    match crate::decode::reference_render(Path::new(&cstr(path))) {
        Ok((rgb, w, h)) => {
            let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
            for c in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            into_raw_image(RgbaImage {
                width: w as u32,
                height: h as u32,
                data: rgba,
            })
        }
        Err(err) => {
            set_err(&err);
            null_image()
        }
    }
}

/// run the AI denoise pre-pass for `path` using `recipe_json` settings
/// (WB/lens_corr are baked into the denoised base). Blocking; heavy.
/// Returns JSON {ok, w, h, ms} or {ok:false, error}.
#[no_mangle]
pub unsafe extern "C" fn araware_ai_denoise_prepare(
    e: *mut c_void,
    path: *const c_char,
    recipe_json: *const c_char,
) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return into_raw_string(r#"{"ok":false,"error":"no engine"}"#.to_string());
    };
    let p = cstr(path);
    let recipe = cstr(recipe_json);
    let recipe = if recipe.is_empty() {
        Recipe::default()
    } else {
        Recipe::from_json(&recipe).unwrap_or_default()
    };
    let t0 = std::time::Instant::now();
    let out = match eng.ai_denoise_prepare(std::path::Path::new(&p), &recipe) {
        Ok((w, h)) => {
            serde_json::json!({"ok": true, "w": w, "h": h, "ms": t0.elapsed().as_millis() as u64})
        }
        Err(err) => serde_json::json!({"ok": false, "error": format!("{err:#}")}),
    };
    into_raw_string(out.to_string())
}

/// JSON {ready: bool}: whether a fresh denoise cache exists for `path`.
#[no_mangle]
pub unsafe extern "C" fn araware_ai_denoise_ready(
    e: *mut c_void,
    path: *const c_char,
) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return into_raw_string(r#"{"ready":false}"#.to_string());
    };
    let ok = eng.ai_denoise_ready(std::path::Path::new(&cstr(path)));
    into_raw_string(format!("{{\"ready\":{ok}}}"))
}

/// run the U-2-Net subject-mask pre-pass for `path` (seconds, CPU or
/// CoreML). Returns JSON {ok, w, h, ms} or {ok:false, error}.
#[no_mangle]
pub unsafe extern "C" fn araware_ai_subject_prepare(
    e: *mut c_void,
    path: *const c_char,
) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return into_raw_string(r#"{"ok":false,"error":"no engine"}"#.to_string());
    };
    let t0 = std::time::Instant::now();
    let out = match eng.ai_subject_prepare(std::path::Path::new(&cstr(path))) {
        Ok((w, h)) => {
            serde_json::json!({"ok": true, "w": w, "h": h, "ms": t0.elapsed().as_millis() as u64})
        }
        Err(err) => serde_json::json!({"ok": false, "error": format!("{err:#}")}),
    };
    into_raw_string(out.to_string())
}

/// JSON {ready: bool}: whether a fresh subject-matte cache exists for `path`.
#[no_mangle]
pub unsafe extern "C" fn araware_ai_subject_ready(
    e: *mut c_void,
    path: *const c_char,
) -> *mut c_char {
    let Some(eng) = engine(e) else {
        return into_raw_string(r#"{"ready":false}"#.to_string());
    };
    let ok = eng.ai_subject_ready(std::path::Path::new(&cstr(path)));
    into_raw_string(format!("{{\"ready\":{ok}}}"))
}
