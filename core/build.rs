use std::path::Path;

fn main() {
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
    let prefix = candidates
        .iter()
        .find(|p| Path::new(p).join("include/libraw/libraw.h").exists())
        .cloned()
        .unwrap_or_else(|| "/opt/homebrew".into());

    cc::Build::new()
        .cpp(true)
        .file("native/ara_shim.cpp")
        .include("native")
        .include(format!("{prefix}/include"))
        .flag_if_supported("-std=c++17")
        .warnings(false)
        .compile("ara_shim");

    if cfg!(target_os = "macos") {
        cc::Build::new()
            .file("native/ara_imgio.mm")
            .flag_if_supported("-std=c++17")
            .flag_if_supported("-fobjc-arc")
            .warnings(false)
            .compile("ara_imgio");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=ImageIO");
        println!("cargo:rustc-link-lib=framework=CoreServices");
    }

    // GoPro GPR SDK (vendored under third_party/gpr, MIT/Apache-2.0):
    // decodes VC-5-compressed GPR (HERO5-12) into uncompressed DNG.
    let gpr_root = std::path::PathBuf::from("../third_party/gpr");
    if gpr_root.is_dir() {
        fn walk(dir: &Path, c: &mut Vec<String>, cpp: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else { return };
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
            b
        };
        if !cs.is_empty() {
            let mut b = mk();
            b.files(&cs)
                .flag_if_supported("-std=c99")
                .warnings(false)
                .compile("ara_gpr_c");
        }
        let mut b = mk();
        b.files(&cpps)
            .file("native/ara_gpr.cpp")
            .cpp(true)
            .flag_if_supported("-std=c++17")
            .warnings(false)
            .compile("ara_gpr_cpp");
    }

    println!("cargo:rustc-link-search=native={prefix}/lib");
    println!("cargo:rustc-link-lib=dylib=raw");
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=c++");
        // let dependent binaries (app bundles) find homebrew dylibs
        println!("cargo:rustc-link-arg=-Wl,-rpath,{prefix}/lib");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rerun-if-changed=native/ara_shim.cpp");
    println!("cargo:rerun-if-changed=native/ara_shim.h");
    println!("cargo:rerun-if-changed=native/ara_imgio.mm");
    println!("cargo:rerun-if-env-changed=LIBRAW_PREFIX");
}
