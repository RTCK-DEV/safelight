# araware

RAW development + photo library application. Folder-direct-read style — point it
at a directory of RAW files, no Lightroom-style import step.

```
┌──────────────┐   C FFI   ┌──────────────────────────────┐
│ SwiftUI app  ├───────────│ araware-core (Rust engine)   │
│  (macOS)     │           │  decode → demosaic → develop │
└──────────────┘           │  catalog (SQLite) + sidecars │
                           └──────────────┬───────────────┘
                                          │ C++ shim
                                   LibRaw (dynamic, LGPL)
```

## Components

| piece | path | what it does |
|---|---|---|
| engine | `core/` | LibRaw decode (Bayer + X-Trans CFA), CPU develop pipeline (WB, cam→sRGB, tone curve, exposure, contrast, highlights/shadows, saturation/vibrance, sharpen, luma NR), embedded-thumbnail extraction, EXIF |
| catalog | `core/src/catalog.rs` | directory scan, RAW+JPEG stem pairing, SQLite db (`~/.araware/catalog.db`), `<stem>.araware.json` sidecars (rating + recipe) |
| C ABI | `core/src/capi.rs` | opaque engine handle, images as `{data,len,w,h}` RGBA8, JSON in/out |
| CLI | `cli/` | `render`, `thumb`, `reference`, `scan`, `meta`, `rate` — test harness + batch tool |
| macOS app | `mac/` | SwiftUI library grid + editor; builds a self-contained `.app` with libraw bundled |

## Build

```sh
brew install libraw            # engine dependency (LGPL, dynamically linked)
cargo build --workspace        # engine + cli
mac/build.sh                   # -> mac/build/araware.app
```

`LIBRAW_PREFIX` overrides the lib search path (default `/opt/homebrew`).

## CLI quickstart

```sh
araware-cli render IMG_1234.ARW out.png            # develop with defaults
araware-cli render IMG_1234.ARW out.png r.json 900 # recipe + max side 900px
araware-cli thumb IMG_1234.ARW t.png               # embedded JPEG thumbnail
araware-cli scan ~/Pictures/2024                   # JSON asset list
araware-cli rate IMG_1234.ARW 4                    # write sidecar
araware-cli reference IMG_1234.ARW ref.png         # libraw's own pipeline (sanity)
```

## Recipe (sidecar `recipe` object)

```json
{
  "exposure": 0.5,          // EV stops
  "contrast": 0.1,          // -1..1
  "highlights": 0.3, "shadows": 0.2, "whites": 0.0, "blacks": 0.0,
  "saturation": 0.0, "vibrance": 0.2,
  "temperature": -0.1, "tint": 0.0, "wb_mode": "as_shot",
  "curve": [],              // [[x,y]...] catmull-rom control points, 0..1
  "sharpen": 0.2, "noise_luma": 0.1
}
```

All edits are non-destructive: ratings and recipes live in
`<stem>.araware.json` next to the source file.

## Licensing notes

- Project code: MIT (see LICENSE)
- LibRaw: LGPL-2.1/CDLL — dynamically linked and bundled as a dylib inside the
  app, so the closed-source option stays open.
- GPL/AGPL codebases (darktable, Exiv2, RapidRAW) were used as references only;
  no code was copied. EXIF is read via kamadak-exif (BSD).

## Roadmap / known limits

- CPU demosaic is a generic same-colour-mean (fine for previews, soft at 100%);
  a wgpu compute pipeline is the planned fast path (wgpu is already a dep).
- X-Trans renders work; cross-checked on Fuji X-E1 RAF.
- No lens corrections yet (lensfun integration is the obvious next step).
- Windows/Linux shell not started — engine is portable by design
  (`araware-cli` runs the whole pipeline headlessly).
