use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use safelight_core::{Engine, Recipe};

// accepts an inline JSON string, a recipe .json file, or a Lightroom
// .xmp preset (crs:*) — a bad recipe must fail loudly rather than render
// with the wrong settings and look like success
fn parse_recipe(arg: &str) -> Result<Recipe> {
    if arg.trim_start().starts_with('{') {
        return Recipe::from_json(arg)
            .ok_or_else(|| anyhow::anyhow!("invalid recipe JSON"));
    }
    let s = std::fs::read_to_string(arg).with_context(|| format!("read recipe {arg}"))?;
    if arg.ends_with(".xmp") {
        Ok(safelight_core::xmp::crs_to_recipe(&s))
    } else {
        Recipe::from_json(&s)
            .ok_or_else(|| anyhow::anyhow!("invalid recipe JSON in {arg}"))
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage:
  safelight-cli render <raw>| aidn <raw>| aisub <raw> <out.png> [recipe.json] [max_px]
  safelight-cli thumb <raw> <out.png> [max_px]
  safelight-cli reference <raw> <out.png>
  safelight-cli scan <folder>
  safelight-cli lrcat <catalog.lrcat> [--apply]
  safelight-cli meta <file>
  safelight-cli auto <file>
  safelight-cli rate <file> <0-5>"
        );
        std::process::exit(2);
    }
    let eng = Engine::new().context("engine init")?;
    let cmd = args[1].as_str();
    match cmd {
        "render" => {
            let path = Path::new(&args[2]);
            let out = args.get(3).map(String::as_str).unwrap_or("out.png");
            // a recipe path argument must read + parse — silent fallback would
            // render with the wrong settings and look like success.
            // .xmp files are parsed as Lightroom develop presets (crs:*).
            let recipe = match args.get(4) {
                Some(r) => parse_recipe(r)?,
                // no recipe arg: adopt any saved sidecar (.safelight.json,
                // Lightroom .xmp, or embedded XMP in the file itself)
                None => eng
                    .read_sidecar(path)
                    .map(|sc| sc.recipe)
                    .unwrap_or_default(),
            };
            let max_px: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            let t = std::time::Instant::now();
            let img = eng.render(path, &recipe, max_px)?;
            eprintln!("render {}x{} in {:?}", img.width, img.height, t.elapsed());
            image::save_buffer(
                out,
                &img.data,
                img.width,
                img.height,
                image::ColorType::Rgba8,
            )?;
        }
        "thumb" => {
            let path = Path::new(&args[2]);
            let out = args.get(3).map(String::as_str).unwrap_or("thumb.png");
            let max_px: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(800);
            let img = eng.thumbnail(path, max_px)?;
            eprintln!("thumb {}x{}", img.width, img.height);
            image::save_buffer(
                out,
                &img.data,
                img.width,
                img.height,
                image::ColorType::Rgba8,
            )?;
        }
        "reference" => {
            let path = Path::new(&args[2]);
            let out = args.get(3).map(String::as_str).unwrap_or("ref.png");
            let (rgb, w, h) = safelight_core::decode::reference_render(path)?;
            let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
            for c in rgb.chunks_exact(3) {
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
            image::save_buffer(out, &rgba, w as u32, h as u32, image::ColorType::Rgba8)?;
        }
        "scan" => {
            let list = eng.scan(Path::new(&args[2]))?;
            println!("{}", serde_json::to_string_pretty(&list)?);
        }
        // merge <out.png> <hdr|focus> <src1> <src2> [src3...]
        "merge" => {
            if args.len() < 5 {
                anyhow::bail!("merge <out.png> <hdr|focus> <src1> <src2> [src3...]");
            }
            let out = &args[2];
            let mode = &args[3];
            let paths: Vec<std::path::PathBuf> =
                args[4..].iter().map(std::path::PathBuf::from).collect();
            let t = std::time::Instant::now();
            let img = eng.merge(&paths, mode)?;
            eprintln!(
                "merge {mode} {}x{} in {:?}",
                img.width,
                img.height,
                t.elapsed()
            );
            image::save_buffer(
                out,
                &img.data,
                img.width,
                img.height,
                image::ColorType::Rgba8,
            )?;
        }
        // import a Lightroom Classic catalog: Lua develop settings → crs
        // recipes, plus rating/flag/label, written as .safelight.json
        // sidecars next to each image that exists on disk.
        "lrcat" => {
            let apply = args.iter().any(|a| a == "--apply");
            let limit: Option<usize> = args
                .iter()
                .find_map(|a| a.strip_prefix("--limit="))
                .and_then(|v| v.parse().ok());
            let grep = args.iter().find_map(|a| a.strip_prefix("--grep="));
            // catalog stores the volume it was created on; remap when files
            // moved (e.g. --root=D:/カメラ/RAW/=G:/カメラ/RAW_alpha7R3/2023/)
            let remap: Vec<(&str, &str)> = args
                .iter()
                .filter_map(|a| a.strip_prefix("--root="))
                .filter_map(|s| s.split_once('='))
                .collect();
            let cat_path = Path::new(&args[2]);
            let mut list = safelight_core::lrcat::entries(cat_path)?;
            for e in &mut list {
                for (from, to) in &remap {
                    let p = e.path.to_string_lossy().replace('\\', "/");
                    if let Some(rest) = p.strip_prefix(from) {
                        e.path = PathBuf::from(format!("{to}{rest}"));
                    }
                }
            }
            if let Some(g) = grep {
                list.retain(|e| e.path.to_string_lossy().contains(g));
            }
            if let Some(n) = limit {
                list.truncate(n);
            }
            // LR's computed AI-mask bitmaps live in <catalog>.lrcat-data/*.blob
            // keyed by MaskDigest — index once, extract per image on apply.
            let blobs = if apply {
                safelight_core::lrcat::blob_index(cat_path)
            } else {
                Default::default()
            };
            let mut n_disk = 0usize;
            let mut n_edits = 0usize;
            let mut n_write = 0usize;
            let mut n_masks = 0usize;
            for e in &mut list {
                if !e.path.exists() {
                    continue;
                }
                n_disk += 1;
                let importable = e.has_edits
                    || e.sidecar.rating != 0
                    || e.sidecar.flag != 0
                    || !e.sidecar.label.is_empty()
                    || !e.sidecar.keywords.is_empty();
                if !importable {
                    continue;
                }
                n_edits += 1;
                if apply {
                    n_masks += safelight_core::lrcat::apply_blob_masks(
                        &blobs,
                        &mut e.sidecar.recipe,
                        &e.path,
                    );
                    let sp = safelight_core::recipe::sidecar_path_for(&e.path);
                    std::fs::write(&sp, serde_json::to_string_pretty(&e.sidecar)?)
                        .with_context(|| format!("write {}", sp.display()))?;
                    n_write += 1;
                }
            }
            println!(
                "{} catalog images, {} on disk, {} with edits — {}",
                list.len(),
                n_disk,
                n_edits,
                if apply {
                    format!("{n_write} sidecars written, {n_masks} mask bitmaps extracted")
                } else {
                    "dry run — pass --apply".into()
                }
            );
        }
        "meta" => {
            let v = eng.metadata(Path::new(&args[2]))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        "aidn" => {
            let path = Path::new(&args[2]);
            let recipe = match args.get(3) {
                Some(r) => parse_recipe(r)?,
                None => Recipe::default(),
            };
            let t = std::time::Instant::now();
            let (w, h) = eng.ai_denoise_prepare(path, &recipe)?;
            println!(
                "denoise cache {}x{} in {:?} -> {:?}",
                w,
                h,
                t.elapsed(),
                safelight_core::ai::denoise_cache_path(path)
            );
            // skip static teardown: onnxruntime's C++ globals crash on exit
            // ("mutex lock failed") — _exit bypasses atexit handlers entirely.
            unsafe extern "C" {
                fn _exit(code: i32) -> !;
            }
            unsafe { _exit(0) };
        }
        "aisub" => {
            let path = Path::new(&args[2]);
            let t = std::time::Instant::now();
            let (w, h) = eng.ai_subject_prepare(path)?;
            println!(
                "subject matte {}x{} in {:?} -> {:?}",
                w,
                h,
                t.elapsed(),
                safelight_core::ai::subject_cache_path(path)
            );
            unsafe extern "C" {
                fn _exit(code: i32) -> !;
            }
            unsafe { _exit(0) };
        }
        "auto" => {
            let r = eng.auto_analyze(Path::new(&args[2]))?;
            println!("{}", serde_json::to_string_pretty(&r)?);
        }
        "rate" => {
            let rating: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            eng.set_rating(Path::new(&args[2]), rating)?;
            println!("ok");
        }
        _ => {
            eprintln!("unknown cmd {cmd}");
            std::process::exit(2);
        }
    }
    Ok(())
}
