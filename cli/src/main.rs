use std::path::Path;

use anyhow::{Context, Result};
use araware_core::{Engine, Recipe};

fn main() -> Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage:
  araware-cli render <raw> <out.png> [recipe.json] [max_px]
  araware-cli thumb <raw> <out.png> [max_px]
  araware-cli reference <raw> <out.png>
  araware-cli scan <folder>
  araware-cli meta <file>
  araware-cli rate <file> <0-5>"
        );
        std::process::exit(2);
    }
    let eng = Engine::new().context("engine init")?;
    let cmd = args[1].as_str();
    match cmd {
        "render" => {
            let path = Path::new(&args[2]);
            let out = args.get(3).map(String::as_str).unwrap_or("out.png");
            let recipe = args
                .get(4)
                .and_then(|s| Recipe::from_json(s))
                .unwrap_or_default();
            let max_px: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
            let t = std::time::Instant::now();
            let img = eng.render(path, &recipe, max_px)?;
            eprintln!(
                "render {}x{} in {:?}",
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
            let (rgb, w, h) = araware_core::decode::reference_render(path)?;
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
        "meta" => {
            let v = eng.metadata(Path::new(&args[2]))?;
            println!("{}", serde_json::to_string_pretty(&v)?);
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
