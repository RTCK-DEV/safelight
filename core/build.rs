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
