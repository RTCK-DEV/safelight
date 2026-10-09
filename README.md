# Safelight

RAW development + photo library application. Folder-direct-read style — point it
at a directory of RAW files, no Lightroom-style import step.

```
┌──────────────┐   C FFI   ┌──────────────────────────────┐
│ SwiftUI app  ├───────────│ safelight-core (Rust engine)   │
│  (macOS)     │           │  decode → demosaic → develop │
└──────────────┘           │  catalog (SQLite) + sidecars │
                           └──────────────┬───────────────┘
                                          │ C++ shim
                                   LibRaw (dynamic, LGPL)
```

## Components

| piece | path | what it does |
|---|---|---|
| engine | `core/` | LibRaw decode (Bayer + X-Trans CFA), develop pipeline (WB as-shot/auto/pick, cam→sRGB, tone + custom/hue curves, exposure, contrast, highlights/shadows, saturation/vibrance, sharpen, luma+chroma NR, clarity, straighten, vignette, grain, DaVinci-style grading, retouch, light effects), embedded-thumbnail extraction, EXIF, scopes |
| GPU pipeline | `core/src/gpu.rs` | wgpu compute (Metal/Vulkan/DX12): stats→demosaic→heal/clone→NR→soft→glow→sharpen/clarity→finish (crop+resize+straighten+flip+CA-fix+adjust+windows+flare+dodge/burn+grain+vignette→rgba8). CPU path kept as fallback (`SAFELIGHT_DISABLE_GPU=1` or any GPU failure) |
| catalog | `core/src/catalog.rs` | directory scan, RAW+JPEG stem pairing, SQLite db (`~/.safelight/catalog.db`), `<stem>.safelight.json` sidecars (rating + label + flag + keywords + recipe + versions), stacks/collections tables, smart rules |
| C ABI | `core/src/capi.rs` | opaque engine handle, images as `{data,len,w,h}` RGBA8, JSON in/out |
| CLI | `cli/` | `render`, `thumb`, `reference`, `scan`, `meta`, `rate` — test harness + batch tool |
| macOS app | `mac/` | SwiftUI library grid + editor; builds a self-contained `.app` with libraw bundled |

## Build

```sh
brew install libraw            # engine dependency (LGPL, dynamically linked)
cargo build --workspace        # engine + cli
mac/build.sh                   # -> mac/build/Safelight.app
```

`LIBRAW_PREFIX` overrides the lib search path (default `/opt/homebrew`).

## CLI quickstart

```sh
safelight-cli render IMG_1234.ARW out.png            # develop with defaults
safelight-cli render IMG_1234.ARW out.png r.json 900 # recipe + max side 900px
safelight-cli thumb IMG_1234.ARW t.png               # embedded JPEG thumbnail
safelight-cli scan ~/Pictures/2024                   # JSON asset list
safelight-cli rate IMG_1234.ARW 4                    # write sidecar
safelight-cli reference IMG_1234.ARW ref.png         # libraw's own pipeline (sanity)
```

## Recipe (sidecar `recipe` object)

```json
{
  "exposure": 0.5,          // EV stops
  "contrast": 0.1,          // -1..1
  "highlights": 0.3, "shadows": 0.2, "whites": 0.0, "blacks": 0.0,
  "saturation": 0.0, "vibrance": 0.2,
  "temperature": -0.1, "tint": 0.0,
  "wb_mode": "pick", "wb_pick": [0.5, 0.45],  // eyedropper WB: neutralize region
  "curve": [],              // [[x,y]...] catmull-rom control points, 0..1
  "curve_r": [], "curve_g": [], "curve_b": [],   // per-channel curves
  "hue_hue": [], "hue_sat": [], "hue_lum": [],    // hue-domain curves
  "lum_sat": [], "sat_sat": [],
  "sharpen": 0.2, "noise_luma": 0.1,
  "rotation_deg": 0.0,      // straighten, -10..10
  "clarity": 0.0,           // midtone local contrast, -1..1
  "vignette": 0.0,          // -1..1 (positive = darkened corners)
  "grain": 0.0,             // film grain, 0..1

  // DaVinci-style grading
  "lift": [0,0,0], "gamma": [1,1,1], "gain": [1,1,1], "offset": [0,0,0],
  "shadow_hue": 0.55, "shadow_sat": 0.3,    // split tone, hue 0..1 (0.52≈teal)
  "midtone_hue": 0.55, "midtone_sat": 0.0,
  "highlight_hue": 0.08, "highlight_sat": 0.2,
  "look": "teal_orange",  // preset: none|teal_orange|film_fade|bleach|noir|matte
  "pivot": 0.18,          // contrast pivot
  "highlight_rolloff": 1.0, "shadow_rolloff": 1.0,
  "z_dark": [0,0,0,0], "z_shadow": [0,0,0,0],   // HDR zone wheels [hue,amt,ev,sat]
  "z_light": [0,0,0,0], "z_global": [0,0,0,0],
  "mixer": [1,0,0, 0,1,0, 0,0,1],              // 3x3 channel mixer
  "mono": [0,0,0],        // monochrome weights (all zero = off)
  "qh": [0.5,0.1,0.1], "qs": [0,1,0.1], "ql": [0,1,0.1],  // HSL qualifier
  "qadj": [0.1,0.4,0,0], "q_invert": false,     // [hue,sat,lum,temp] on masked px
  "q_clean": [0,1], "q_blur": 0.0, "q_show": false,  // matte finesse + highlight view
  "windows": [{"kind":"circle","p":[0.5,0.5,0.2,0.2,0,0.4],"ev":0.5,"sat":0.1,"temp":0.2,
              "invert":false,"enabled":true,"opacity":1.0}],

  // auto correction (sparse scene stats drive all three)
  "wb_mode": "auto", "auto_exposure": true, "auto_contrast": true,

  // retouch
  "crop": [0,0,0,0],        // [left,top,right,bottom] fractions of frame
  "spots": [[0.5,0.3,0.04,0]],   // spot heal [cx,cy,r], frame-normalized (max 8)
  "clones": [[0.2,0.2,0.7,0.7,0.05,0]], // clone stamp [sx,sy,dx,dy,r] (max 8)
  "lights": [[0.5,0.7,0.25,0.8]], // dodge/burn [cx,cy,r,ev], dst-normalized (max 8)
  "noise_chroma": 0.3, "beauty": 0.2, "deband": 0.0, "ca_fix": 0.0,
  "glow": 0.0,                    // highlight bloom
  "flare": [0.3,0.3,0.5,0.08]     // lens flare [cx,cy,strength,hue]
}
```

All edits are non-destructive: ratings, recipes and grade versions live in
`<stem>.safelight.json` next to the source file. Named snapshots:

```json
"versions": [{"name": "Teal look", "recipe": { ... }}]
```

## Library

Three-pane layout (sidebar / grid / editor). Per-file state lives in the
sidecars; the SQLite catalog indexes scans and owns stacks + collections.

- **Flags**: `P` pick / `U` unflag / `X` reject — badges on cells, rejected
  cells dimmed. Keys work on every selected photo (multi-select: `⌘`-click
  toggles, `⇧`-click ranges, arrows navigate).
- **Stacks**: `G` groups the selection into a stack; collapsed stacks show
  the cover with a depth badge. Context menu: expand/collapse, ungroup,
  set-as-cover.
- **Virtual copies**: extra sidecar `<stem>.safelight.vN.json` — no file
  duplication, inherits the master's rating/label on creation, can be
  promoted to master.
- **Collections**: manual (drag-in via context menu) and smart (rule sheet:
  rating, flag, label, camera/lens/keyword/filename contains, edited-only);
  sidebar shows live counts.
- **Survey**: 2–4 selected photos side-by-side, rendered at loupe res with
  their own recipes; click flags picked, `Esc` exits.
- **Filter strip**: stars-min, flag segment, label dots, camera/lens/keyword
  dropdowns, filename/camera/lens search, sort (name / capture time /
  modified / rating / size).
- **Info card**: camera, lens, ISO / f / shutter / focal chips, flag buttons,
  stack + variant badges, keyword editor.
- **Thumbnails**: PNG disk cache in `~/.safelight/thumbs/` keyed by
  path+mtime+size — rescans and scope switches are instant.
- **Batch export**: context menu "Export Selected…" renders every selected
  photo with its own recipe to a chosen folder.

## Editor interaction (DaVinci-derived)

- **Palettes**: 13 icon tabs (Light / Wheels / Curves / Zones / Qualifier /
  Windows / Mixer / Retouch / Detail / FX / Transform / Meters / Versions);
  an amber dot marks any palette holding non-default values; the header reset
  arrow clears only the active palette.
- **Compare**: Before / Wipe V / Wipe H / Difference / Mix — the wipe divider
  drags. `B` toggles Before, `W` cycles the wipe modes.
- **Zoom/pan**: scroll wheel or pinch zooms under the cursor zone, drag pans
  when zoomed, double-click toggles 1×/2×, overlay controls for fit/−/%/＋/1:1.
- **History**: unlimited undo/redo (Cmd+Z / Cmd+Shift+Z); slider drags
  coalesce into one step. Undo also reverts a version apply.
- **Keys**: `0`–`5` rating, `←`/`→` photo nav, `Esc` exits tool/compare,
  `Cmd+S` save, `Cmd+E` export, `Cmd+=` apply the previous photo's grade.
- **Qualifier pickers**: New / + / − eyedroppers sample a 5×5 area under the
  cursor; the View toggle highlights the matte; Hue/Sat/Lum gradient range
  bars + Matte Finesse (Clean Black/White/Blur) refine the key.
- **Power windows**: per-window eye toggle and opacity, amber selection
  outline, drag on the stage to move the selected shape, drag in Grad mode
  draws the gradient line.
- **Numeric entry**: every slider value is tappable — type a number, Enter
  commits, Esc cancels.
- **Versions**: named recipe snapshots stored in the sidecar; click to apply
  (undo-able), × to delete. Filmstrip thumbnails show an amber dot on photos
  with unsaved edits.

## Licensing notes

- Project code: MIT (see LICENSE)
- LibRaw: LGPL-2.1/CDLL — dynamically linked and bundled as a dylib inside the
  app, so the closed-source option stays open.
- GPL/AGPL codebases (darktable, Exiv2, RapidRAW) were used as references only;
  no code was copied. EXIF is read via kamadak-exif (BSD).
- GoPro GPR SDK (vendored in `third_party/gpr/`): MIT OR Apache-2.0, © GoPro —
  decodes VC-5-compressed GPR (HERO5-12) into uncompressed DNG for LibRaw.
- Sigma X3F (Foveon): own decoder derived from dcraw (public domain).
- Nikon NEF HE/HE* (TicoRAW): own bit-exact decoder (`core/src/nef_he.rs`).

## Roadmap / known limits

- Scopes: RGB parade waveform, vectorscope (Cb/Cr), CIE xy chromaticity,
  computed from the rendered frame.
- Demosaic is a generic same-colour-mean on both paths (fine for previews,
  soft at 100%); a higher-quality method is the obvious next step.
- X-Trans renders work; cross-checked on Fuji X-E1 RAF.
- No lens corrections yet (lensfun integration is the obvious next step).
- Windows/Linux shell not started — engine is portable by design
  (`safelight-cli` runs the whole pipeline headlessly).
