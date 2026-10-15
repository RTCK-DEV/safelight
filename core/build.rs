use std::path::Path;

fn main() {
    // LibRaw source-tree build (Windows/CI): LIBRAW_SRC points at an
    // unpacked LibRaw checkout (contains libraw/libraw.h + src/**.cpp) and
    // the whole library is compiled statically into safelight-core.
    // Precedence: LIBRAW_SRC > LIBRAW_PREFIX-that-is-a-source-tree >
    // vendored ../third_party/libraw > installed prefix search below.
    let mut libraw_src: Option<String> = std::env::var("LIBRAW_SRC").ok();
    if libraw_src.is_none() {
        if let Ok(p) = std::env::var("LIBRAW_PREFIX") {
            if Path::new(&p).join("libraw/libraw.h").exists() {
                libraw_src = Some(p);
            }
        }
    }
    if libraw_src.is_none() {
        let v = "../third_party/libraw";
        if Path::new(v).join("libraw/libraw.h").exists() {
            libraw_src = Some(v.to_string());
        }
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Ok(p) = std::env::var("LIBRAW_PREFIX") {
        candidates.push(p);
    }
    for p in [
        "/opt/homebrew/opt/libraw",
        "/opt/homebrew",
        "/usr/local/opt/libraw",
        "/usr/local",
        "/usr",
    ] {
        candidates.push(p.to_string());
    }
    let prefix = libraw_src.clone().unwrap_or_else(|| {
        candidates
            .iter()
            .find(|p| Path::new(p).join("include/libraw/libraw.h").exists())
            .cloned()
            .unwrap_or_else(|| "/opt/homebrew".into())
    });
    let include_root = if libraw_src.is_some() {
        // source tree: public headers live at <root>/libraw/*.h
        prefix.clone()
    } else {
        format!("{prefix}/include")
    };

    if let Some(src) = &libraw_src {
        compile_libraw_from_source(src);
    }

    let mut shim = cc::Build::new();
    shim.cpp(true)
        .file("native/sl_shim.cpp")
        .include("native")
        .include(&include_root)
        .flag_if_supported("-std=c++17")
        .flag_if_supported("/std:c++17")
        .warnings(false);
    if libraw_src.is_some() {
        // static build: suppress dllimport declspecs in libraw.h
        shim.define("LIBRAW_NODLL", None);
    }
    if cfg!(target_os = "windows") {
        shim.define("_CRT_SECURE_NO_WARNINGS", None);
    }
    shim.compile("sl_shim");

    if cfg!(target_os = "macos") {
        cc::Build::new()
            .file("native/sl_imgio.mm")
            .flag_if_supported("-std=c++17")
            .flag_if_supported("-fobjc-arc")
            .warnings(false)
            .compile("sl_imgio");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=ImageIO");
        println!("cargo:rustc-link-lib=framework=CoreServices");
    }

    // GoPro GPR SDK (vendored under third_party/gpr, MIT/Apache-2.0):
    // decodes VC-5-compressed GPR (HERO5-12) into uncompressed DNG.
    let gpr_root = std::path::PathBuf::from("../third_party/gpr");
    if gpr_root.is_dir() {
        fn walk(dir: &Path, c: &mut Vec<String>, cpp: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, c, cpp);
                } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                    let f = p.to_string_lossy().to_string();
                    match ext {
                        "c" => c.push(f),
                        "cpp" | "cc" | "cxx" => cpp.push(f),
                        _ => {}
                    }
                }
            }
        }
        let (mut cs, mut cpps) = (Vec::new(), Vec::new());
        walk(&gpr_root, &mut cs, &mut cpps);

        let includes = [
            "../third_party/gpr/gpr_sdk/public",
            "../third_party/gpr/common/public",
            "../third_party/gpr/common/private",
            "../third_party/gpr/vc5_common",
            "../third_party/gpr/vc5_decoder",
            "../third_party/gpr/dng_sdk",
            "../third_party/gpr/md5_lib",
            "../third_party/gpr/expat_lib",
            "../third_party/gpr/xmp_core/public/include",
            "native",
        ];
        let defs: &[(&str, Option<&str>)] = &[
            ("GPR_READING", Some("1")),
            ("GPR_WRITING", Some("0")),
            ("GPR_JPEG_AVAILABLE", Some("0")),
            ("GPR_TIMING", Some("0")),
            ("XML_STATIC", Some("1")),
            ("GIT_BRANCH", Some("\"\"")),
            ("GIT_COMMIT_HASH", Some("\"vendored\"")),
            (
                if cfg!(target_os = "macos") {
                    "qMacOS"
                } else if cfg!(target_os = "windows") {
                    "qWinOS"
                } else {
                    "qLinux"
                },
                Some("1"),
            ),
        ];
        let mk = || {
            let mut b = cc::Build::new();
            for inc in includes {
                b.include(inc);
            }
            for (k, v) in defs {
                match v {
                    Some(v) => b.define(k, *v),
                    None => b.define(k, None),
                };
            }
            if cfg!(target_os = "windows") {
                // vendored expat uses __attribute((fallthrough)) etc.
                let shim = format!(
                    "{}\\native\\msvc_compat.h",
                    std::env::var("CARGO_MANIFEST_DIR").unwrap()
                );
                b.flag("/FI").flag(&shim);
            }
            b
        };
        if !cs.is_empty() {
            let mut b = mk();
            b.files(&cs)
                .flag_if_supported("-std=c99")
                .warnings(false)
                .compile("sl_gpr_c");
        }
        let mut b = mk();
        b.files(&cpps)
            .file("native/sl_gpr.cpp")
            .cpp(true)
            .flag_if_supported("-std=c++17")
            .warnings(false)
            .compile("sl_gpr_cpp");
    }

    if libraw_src.is_none() {
        println!("cargo:rustc-link-search=native={prefix}/lib");
        println!("cargo:rustc-link-lib=dylib=raw");
    }
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
        // let dependent binaries (app bundles) find homebrew dylibs
        println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib");
    } else if cfg!(target_os = "windows") {
        // MSVC links the C++ runtime automatically; nothing extra needed
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=native/sl_shim.cpp");
    println!("cargo:rerun-if-changed=native/sl_shim.h");
    println!("cargo:rerun-if-changed=native/sl_imgio.mm");
    println!("cargo:rerun-if-env-changed=LIBRAW_PREFIX");
    println!("cargo:rerun-if-env-changed=LIBRAW_SRC");
}

/// Compile LibRaw itself from a source checkout into a static lib.
/// Mirrors Makefile.msvc: LIBRAW_BUILDLIB + LIBRAW_NODLL, no optional
/// dependencies (RawSpeed/DNG SDK/LCMS/JPEG) — DNGs decode via LibRaw's
/// own paths; deflate-compressed DNG needs USE_ZLIB + zlib on the path.
fn compile_libraw_from_source(root: &str) {
    let root_path = Path::new(root);
    let src_dir = root_path.join("src");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("cpp") {
                // *_ph.cpp are placeholder stubs for no-postprocessing
                // builds; they collide with the real implementations.
                let is_stub = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.ends_with("_ph"))
                    .unwrap_or(false);
                if !is_stub {
                    out.push(p);
                }
            }
        }
    }
    walk(&src_dir, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "LIBRAW_SRC={root} has no src/**/*.cpp files"
    );
    let mut b = cc::Build::new();
    b.cpp(true)
        .files(&files)
        .include(root) // public headers: <libraw/...>
        .define("LIBRAW_BUILDLIB", None)
        .define("LIBRAW_NODLL", None)
        .flag_if_supported("-std=c++17")
        .flag_if_supported("/std:c++17")
        .warnings(false);
    if cfg!(target_os = "windows") {
        b.define("WIN32", None)
            .define("_CRT_SECURE_NO_WARNINGS", None)
            .define("_WINSOCK_DEPRECATED_NO_WARNINGS", None);
    }
    println!(
        "cargo:warning=compiling {} LibRaw sources from {root}",
        files.len()
    );
    b.compile("raw");
}
