import Foundation

struct Photo: Identifiable, Codable, Hashable {
    /// unique row id — variants share `path` so the slot must be mixed in
    var id: String { vslot == 0 ? path : "\(path)#v\(vslot)" }
    /// collection/stack references use the same form
    var ref: String { id }
    /// real file on disk (the key for render/thumbnail engine calls)
    let path: String
    let name: String
    let kind: String
    let size: Int64
    let mtime: Int64
    var rating: Int
    var label: String
    /// -1 rejected, 0 none, 1 picked
    var flag: Int
    var keywords: [String]
    /// virtual copy slot (0 = master, n>0 = .araware.v{n}.json sidecar)
    let vslot: Int
    /// stack id (0 = unstacked) + position within it
    let stack: Int64
    let stack_seq: Int
    let pair: String?
    let has_sidecar: Bool
    /// "make model" + lens from the file header/EXIF ("" when unknown)
    let camera: String
    let lens: String
    /// capture metadata
    let ctime: Int64
    let iso: Double
    let aperture: Double
    let focal: Double
    let shutter: Double

    var isVariant: Bool { vslot > 0 }

    enum CodingKeys: String, CodingKey {
        case path, name, kind, size, mtime, rating, label, flag, keywords
        case vslot, stack, stack_seq, pair, has_sidecar, camera, lens
        case ctime, iso, aperture, focal, shutter
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decode(String.self, forKey: .kind)
        size = try c.decode(Int64.self, forKey: .size)
        mtime = try c.decode(Int64.self, forKey: .mtime)
        rating = try c.decode(Int.self, forKey: .rating)
        label = try c.decode(String.self, forKey: .label)
        flag = (try? c.decode(Int.self, forKey: .flag)) ?? 0
        keywords = (try? c.decode([String].self, forKey: .keywords)) ?? []
        vslot = (try? c.decode(Int.self, forKey: .vslot)) ?? 0
        stack = (try? c.decode(Int64.self, forKey: .stack)) ?? 0
        stack_seq = (try? c.decode(Int.self, forKey: .stack_seq)) ?? 0
        pair = try? c.decode(String.self, forKey: .pair)
        has_sidecar = (try? c.decode(Bool.self, forKey: .has_sidecar)) ?? false
        camera = (try? c.decode(String.self, forKey: .camera)) ?? ""
        lens = (try? c.decode(String.self, forKey: .lens)) ?? ""
        ctime = (try? c.decode(Int64.self, forKey: .ctime)) ?? 0
        iso = (try? c.decode(Double.self, forKey: .iso)) ?? 0
        aperture = (try? c.decode(Double.self, forKey: .aperture)) ?? 0
        focal = (try? c.decode(Double.self, forKey: .focal)) ?? 0
        shutter = (try? c.decode(Double.self, forKey: .shutter)) ?? 0
    }
}

/// catalog collection row (manual or rules-evaluated smart)
struct CollectionInfo: Codable, Identifiable, Hashable {
    var id: Int64
    var name: String
    var smart: Int
    var rules: String
    var count: Int
}

enum WbMode: String, Codable, CaseIterable {
    case asShot = "as_shot"
    case auto = "auto"
    case manual = "manual"
    case pick = "pick"
}

/// Mirrors araware_core::recipe::PowerWindow (serde).
struct PowerWindow: Codable, Equatable, Identifiable {
    var id = UUID()
    var kind: String = "circle"      // "circle" [cx,cy,rx,ry,rot_deg,soft] | "gradient" [x1,y1,x2,y2,soft,0] | "lum" [lo,hi,lof,hif,0,0]
    var p: [Double] = [0.5, 0.5, 0.2, 0.2, 0, 0.4]
    var ev: Double = 0
    var sat: Double = 0
    var temp: Double = 0
    var invert: Bool = false
    var enabled: Bool = true         // per-window on/off
    var opacity: Double = 1          // adjustment strength 0..1
    var linkQ: Bool = false          // gate mask by the HSL qualifier matte

    enum CodingKeys: String, CodingKey {
        case kind, p, ev, sat, temp, invert, enabled, opacity
        case linkQ = "link_q"
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        kind = (try? c.decode(String.self, forKey: .kind)) ?? "circle"
        p = (try? c.decode([Double].self, forKey: .p)) ?? [0.5, 0.5, 0.2, 0.2, 0, 0.4]
        ev = (try? c.decode(Double.self, forKey: .ev)) ?? 0
        sat = (try? c.decode(Double.self, forKey: .sat)) ?? 0
        temp = (try? c.decode(Double.self, forKey: .temp)) ?? 0
        invert = (try? c.decode(Bool.self, forKey: .invert)) ?? false
        enabled = (try? c.decode(Bool.self, forKey: .enabled)) ?? true
        opacity = (try? c.decode(Double.self, forKey: .opacity)) ?? 1
        linkQ = (try? c.decode(Bool.self, forKey: .linkQ)) ?? false
    }
}

/// Mirrors araware_core::recipe::BrushStroke (serde).
struct BrushStroke: Codable, Equatable, Identifiable {
    var id = UUID()
    var pts: [[Double]] = []   // frame-normalized polyline
    var radius: Double = 0.05  // fraction of frame height
    var soft: Double = 0.5     // feather 0..1
    var opacity: Double = 1    // stroke flow 0..1
    var erase: Bool = false

    enum CodingKeys: String, CodingKey { case pts, radius, soft, opacity, erase }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pts = (try? c.decode([[Double]].self, forKey: .pts)) ?? []
        radius = (try? c.decode(Double.self, forKey: .radius)) ?? 0.05
        soft = (try? c.decode(Double.self, forKey: .soft)) ?? 0.5
        opacity = (try? c.decode(Double.self, forKey: .opacity)) ?? 1
        erase = (try? c.decode(Bool.self, forKey: .erase)) ?? false
    }
}

/// Mirrors araware_core::recipe::BrushLayer (serde).
struct BrushLayer: Codable, Equatable, Identifiable {
    var id = UUID()
    var enabled: Bool = true
    var opacity: Double = 1
    var ev: Double = 0
    var sat: Double = 0
    var temp: Double = 0
    var linkQ: Bool = false
    // edge-aware masking: mask only covers pixels similar to the colour
    // under each stroke's first dab (engine edge_sim, ±edge_tol)
    var edgeAware: Bool = false
    var edgeTol: Double = 0.5
    var strokes: [BrushStroke] = []

    enum CodingKeys: String, CodingKey {
        case enabled, opacity, ev, sat, temp
        case linkQ = "link_q"
        case edgeAware = "edge_aware"
        case edgeTol = "edge_tol"
        case strokes
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = (try? c.decode(Bool.self, forKey: .enabled)) ?? true
        opacity = (try? c.decode(Double.self, forKey: .opacity)) ?? 1
        ev = (try? c.decode(Double.self, forKey: .ev)) ?? 0
        sat = (try? c.decode(Double.self, forKey: .sat)) ?? 0
        temp = (try? c.decode(Double.self, forKey: .temp)) ?? 0
        linkQ = (try? c.decode(Bool.self, forKey: .linkQ)) ?? false
        edgeAware = (try? c.decode(Bool.self, forKey: .edgeAware)) ?? false
        edgeTol = (try? c.decode(Double.self, forKey: .edgeTol)) ?? 0.5
        strokes = (try? c.decode([BrushStroke].self, forKey: .strokes)) ?? []
    }
}

/// Mirrors araware_core::recipe::Recipe (serde snake_case).
struct Recipe: Codable, Equatable {
    var exposure: Double = 0
    var contrast: Double = 0
    var highlights: Double = 0
    var shadows: Double = 0
    var whites: Double = 0
    var blacks: Double = 0
    var saturation: Double = 0
    var vibrance: Double = 0
    var temperature: Double = 0
    var tint: Double = 0
    var wb_mode: WbMode = .asShot
    var curve: [[Double]] = []
    var sharpen: Double = 0
    var noise_luma: Double = 0
    var rotation_deg: Double = 0
    var clarity: Double = 0
    var vignette: Double = 0
    var grain: Double = 0
    // grading
    var lift: [Double] = [0, 0, 0]
    var gamma: [Double] = [1, 1, 1]
    var gain: [Double] = [1, 1, 1]
    var shadow_hue: Double = 0.55
    var shadow_sat: Double = 0
    var highlight_hue: Double = 0.08
    var highlight_sat: Double = 0
    var look: String = ""
    // auto correction
    var auto_exposure: Bool = false
    var auto_contrast: Bool = false
    // retouch
    var crop: [Double] = [0, 0, 0, 0]
    var spots: [[Double]] = []
    var lights: [[Double]] = []
    // WB eyedropper
    var wb_pick: [Double] = [0.5, 0.5]
    // color page
    var offset: [Double] = [0, 0, 0]
    var midtone_hue: Double = 0.55
    var midtone_sat: Double = 0
    var curve_r: [[Double]] = []
    var curve_g: [[Double]] = []
    var curve_b: [[Double]] = []
    var hue_hue: [[Double]] = []
    var hue_sat: [[Double]] = []
    var hue_lum: [[Double]] = []
    var lum_sat: [[Double]] = []
    var sat_sat: [[Double]] = []
    // HSL qualifier: [center/lo, width/hi, softness] — width 0 = disabled
    // (matches Rust Recipe::default; a nonzero width silently enables the
    // qualifier and gates serial-stage keys to a hue band)
    var qh: [Double] = [0.0, 0.0, 0.05]
    var qs: [Double] = [0.0, 1.0, 0.1]
    var ql: [Double] = [0.0, 1.0, 0.1]
    var qadj: [Double] = [0, 0, 0, 0]  // hue shift, sat, lum, temp
    var q_invert: Bool = false
    var q_clean: [Double] = [0, 1]     // matte finesse: clean black/white remap
    var q_blur: Double = 0             // matte finesse: edge blur/dilate
    var q_show: Bool = false           // highlight/isolate preview of the key
    var q_enabled: Bool = false        // UI-side gate; cleared qualifiers are no-ops
    var windows: [PowerWindow] = []
    // HDR zone wheels [hue, amount, ev, sat]
    var z_dark: [Double] = [0, 0, 0, 0]
    var z_shadow: [Double] = [0, 0, 0, 0]
    var z_light: [Double] = [0, 0, 0, 0]
    var z_global: [Double] = [0, 0, 0, 0]
    var pivot: Double = 0.18
    var highlight_rolloff: Double = 1.0
    var shadow_rolloff: Double = 1.0
    // RGB mixer (identity) + monochrome weights (all zero = off)
    var mixer: [Double] = [1, 0, 0, 0, 1, 0, 0, 0, 1]
    var mono: [Double] = [0, 0, 0]
    // clone stamp [sx,sy,dx,dy,r,0] frame-normalized
    var clones: [[Double]] = []
    // restoration & light effects
    var beauty: Double = 0
    var noise_chroma: Double = 0.2  // mild chroma NR by default (LR/Affinity convention)
    var dehaze: Double = 0
    var ca_fix: Double = 0
    var deband: Double = 0
    var glow: Double = 0
    var flare: [Double] = [0, 0, 0, 0]  // cx, cy, strength, hue
    // tone equalizer: 9 log2-luma zones centered at -4..+4 EV
    var zones_ev: [Double] = [Double](repeating: 0, count: 9)
    // WB eyedropper half-width as a fraction of the frame (0.002..0.2)
    var wb_pick_size: Double = 0.025
    // imported .cube 3D LUT ("" = none) + blend amount
    var lut_file: String = ""
    var lut_amount: Double = 1
    // keystone: vertical/horizontal trapezoid warp -0.4..0.4
    var key_v: Double = 0
    var key_h: Double = 0
    // lensfun profile correction strength 0..1 (1 = on, LR-style default)
    var lens_corr: Double = 1
    // adjustment-brush layers (LR-style, up to 4 honoured by the engine)
    var brushes: [BrushLayer] = []
    // AI denoise amount 0..1 — blends against the prepared denoise cache
    var ai_denoise: Double = 0
    // serial correction stages (DaVinci serial nodes, up to 4 honoured)
    var stages: [Stage] = []
    // ColorSlice: 7 hue wedges (R/Skin/Y/G/C/B/M) [hue_shift, sat_delta, lum_delta, enabled]
    var color_slice: [[Double]] = [[0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]]
    // ColorWarper control points [h, s, dh, ds, radius], max 8 honoured
    var warper: [[Double]] = []

}

/// Grade library still (DaVinci Gallery): a saved recipe + thumbnail.
/// Persisted app-wide at ~/.araware/gallery/stills.json with JPEG thumbs
/// beside it (<id>.jpg).
struct GradeStill: Codable, Equatable, Identifiable {
    var id = UUID()
    var name: String = ""
    var recipe: Recipe = Recipe()
    var saved: TimeInterval = 0
}
/// the full adjust-domain recipe applied on top of the previous result,
/// gated by its own qualifier/window key and opacity.
struct Stage: Codable, Equatable, Identifiable {
    var id = UUID()
    var name: String = ""
    var enabled: Bool = true
    var opacity: Double = 1     // node key output gain 0..1
    var invert: Bool = false    // apply the grade outside the key
    var params: Recipe = Recipe()

    enum CodingKeys: String, CodingKey {
        case name, enabled, opacity, invert, params
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = (try? c.decode(String.self, forKey: .name)) ?? ""
        enabled = (try? c.decode(Bool.self, forKey: .enabled)) ?? true
        opacity = (try? c.decode(Double.self, forKey: .opacity)) ?? 1
        invert = (try? c.decode(Bool.self, forKey: .invert)) ?? false
        params = (try? c.decode(Recipe.self, forKey: .params)) ?? Recipe()
    }
}

extension Recipe {
    /// Persisted fields mirror the Rust Recipe. `q_enabled` is intentionally
    /// absent — it's a UI-only gate derived from qh[1] > 0, and the Rust struct
    /// has no such field (its presence used to break sidecar decode).
    enum CodingKeys: String, CodingKey {
        case exposure, contrast, highlights, shadows, whites, blacks
        case saturation, vibrance, temperature, tint, wb_mode, curve
        case sharpen, noise_luma, rotation_deg, clarity, vignette, grain
        case lift, gamma, gain, shadow_hue, shadow_sat
        case highlight_hue, highlight_sat, look
        case auto_exposure, auto_contrast
        case crop, spots, lights, wb_pick
        case offset, midtone_hue, midtone_sat
        case curve_r, curve_g, curve_b
        case hue_hue, hue_sat, hue_lum, lum_sat, sat_sat
        case qh, qs, ql, qadj, q_invert, q_clean, q_blur, q_show, windows
        case z_dark, z_shadow, z_light, z_global
        case pivot, highlight_rolloff, shadow_rolloff
        case mixer, mono, clones
        case beauty, noise_chroma, dehaze, ca_fix, deband, glow, flare
        case zones_ev, wb_pick_size, lut_file, lut_amount, key_v, key_h
        case lens_corr, brushes
        case ai_denoise
        case stages, color_slice, warper
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        self.init()
        func opt<T: Decodable>(_ k: CodingKeys, _ t: T.Type) -> T? { try? c.decode(T.self, forKey: k) }
        exposure = opt(.exposure, Double.self) ?? 0
        contrast = opt(.contrast, Double.self) ?? 0
        highlights = opt(.highlights, Double.self) ?? 0
        shadows = opt(.shadows, Double.self) ?? 0
        whites = opt(.whites, Double.self) ?? 0
        blacks = opt(.blacks, Double.self) ?? 0
        saturation = opt(.saturation, Double.self) ?? 0
        vibrance = opt(.vibrance, Double.self) ?? 0
        temperature = opt(.temperature, Double.self) ?? 0
        tint = opt(.tint, Double.self) ?? 0
        wb_mode = opt(.wb_mode, WbMode.self) ?? .asShot
        curve = opt(.curve, [[Double]].self) ?? []
        sharpen = opt(.sharpen, Double.self) ?? 0
        noise_luma = opt(.noise_luma, Double.self) ?? 0
        rotation_deg = opt(.rotation_deg, Double.self) ?? 0
        clarity = opt(.clarity, Double.self) ?? 0
        vignette = opt(.vignette, Double.self) ?? 0
        grain = opt(.grain, Double.self) ?? 0
        lift = opt(.lift, [Double].self) ?? [0, 0, 0]
        gamma = opt(.gamma, [Double].self) ?? [1, 1, 1]
        gain = opt(.gain, [Double].self) ?? [1, 1, 1]
        shadow_hue = opt(.shadow_hue, Double.self) ?? 0.55
        shadow_sat = opt(.shadow_sat, Double.self) ?? 0
        highlight_hue = opt(.highlight_hue, Double.self) ?? 0.08
        highlight_sat = opt(.highlight_sat, Double.self) ?? 0
        look = opt(.look, String.self) ?? ""
        auto_exposure = opt(.auto_exposure, Bool.self) ?? false
        auto_contrast = opt(.auto_contrast, Bool.self) ?? false
        crop = opt(.crop, [Double].self) ?? [0, 0, 0, 0]
        spots = opt(.spots, [[Double]].self) ?? []
        lights = opt(.lights, [[Double]].self) ?? []
        wb_pick = opt(.wb_pick, [Double].self) ?? [0.5, 0.5]
        offset = opt(.offset, [Double].self) ?? [0, 0, 0]
        midtone_hue = opt(.midtone_hue, Double.self) ?? 0.55
        midtone_sat = opt(.midtone_sat, Double.self) ?? 0
        curve_r = opt(.curve_r, [[Double]].self) ?? []
        curve_g = opt(.curve_g, [[Double]].self) ?? []
        curve_b = opt(.curve_b, [[Double]].self) ?? []
        hue_hue = opt(.hue_hue, [[Double]].self) ?? []
        hue_sat = opt(.hue_sat, [[Double]].self) ?? []
        hue_lum = opt(.hue_lum, [[Double]].self) ?? []
        lum_sat = opt(.lum_sat, [[Double]].self) ?? []
        sat_sat = opt(.sat_sat, [[Double]].self) ?? []
        qh = opt(.qh, [Double].self) ?? [0.0, 0.0, 0.05]
        qs = opt(.qs, [Double].self) ?? [0.0, 1.0, 0.1]
        ql = opt(.ql, [Double].self) ?? [0.0, 1.0, 0.1]
        qadj = opt(.qadj, [Double].self) ?? [0, 0, 0, 0]
        q_invert = opt(.q_invert, Bool.self) ?? false
        q_clean = opt(.q_clean, [Double].self) ?? [0, 1]
        q_blur = opt(.q_blur, Double.self) ?? 0
        q_show = opt(.q_show, Bool.self) ?? false
        windows = opt(.windows, [PowerWindow].self) ?? []
        z_dark = opt(.z_dark, [Double].self) ?? [0, 0, 0, 0]
        z_shadow = opt(.z_shadow, [Double].self) ?? [0, 0, 0, 0]
        z_light = opt(.z_light, [Double].self) ?? [0, 0, 0, 0]
        z_global = opt(.z_global, [Double].self) ?? [0, 0, 0, 0]
        pivot = opt(.pivot, Double.self) ?? 0.18
        highlight_rolloff = opt(.highlight_rolloff, Double.self) ?? 1.0
        shadow_rolloff = opt(.shadow_rolloff, Double.self) ?? 1.0
        mixer = opt(.mixer, [Double].self) ?? [1, 0, 0, 0, 1, 0, 0, 0, 1]
        mono = opt(.mono, [Double].self) ?? [0, 0, 0]
        clones = opt(.clones, [[Double]].self) ?? []
        beauty = opt(.beauty, Double.self) ?? 0
        noise_chroma = opt(.noise_chroma, Double.self) ?? 0.2
        dehaze = opt(.dehaze, Double.self) ?? 0
        ca_fix = opt(.ca_fix, Double.self) ?? 0
        deband = opt(.deband, Double.self) ?? 0
        glow = opt(.glow, Double.self) ?? 0
        flare = opt(.flare, [Double].self) ?? [0, 0, 0, 0]
        var z = opt(.zones_ev, [Double].self) ?? [Double](repeating: 0, count: 9)
        if z.count != 9 { z = [Double](repeating: 0, count: 9) }
        zones_ev = z
        wb_pick_size = opt(.wb_pick_size, Double.self) ?? 0.025
        lut_file = opt(.lut_file, String.self) ?? ""
        lut_amount = opt(.lut_amount, Double.self) ?? 1
        brushes = opt(.brushes, [BrushLayer].self) ?? []
        key_v = opt(.key_v, Double.self) ?? 0
        key_h = opt(.key_h, Double.self) ?? 0
        stages = opt(.stages, [Stage].self) ?? []
        var cs = opt(.color_slice, [[Double]].self) ?? []
        if cs.count != 7 { cs = [[0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]] }
        color_slice = cs
        warper = opt(.warper, [[Double]].self) ?? []
        lens_corr = opt(.lens_corr, Double.self) ?? 1
        ai_denoise = opt(.ai_denoise, Double.self) ?? 0
        q_enabled = qh[1] > 0
    }
}

/// Mirrors araware_core::recipe::GradeVersion: named recipe snapshot
/// (DaVinci grade version / gallery still).
struct GradeVersion: Codable, Equatable, Identifiable {
    var id = UUID()
    var name: String = ""
    var recipe: Recipe = Recipe()

    enum CodingKeys: String, CodingKey { case name, recipe }

    init(name: String = "", recipe: Recipe = Recipe()) {
        self.name = name
        self.recipe = recipe
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = (try? c.decode(String.self, forKey: .name)) ?? ""
        recipe = (try? c.decode(Recipe.self, forKey: .recipe)) ?? Recipe()
    }
}

/// Mirrors araware_core::recipe::Sidecar.
struct Sidecar: Codable {
    var version: Int = 1
    var rating: Int = 0
    var label: String = ""
    /// -1 rejected, 0 none, 1 picked
    var flag: Int = 0
    var keywords: [String] = []
    var recipe: Recipe = Recipe()
    var versions: [GradeVersion] = []

    enum CodingKeys: String, CodingKey {
        case version, rating, label, flag, keywords, recipe, versions
    }

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = (try? c.decode(Int.self, forKey: .version)) ?? 1
        rating = (try? c.decode(Int.self, forKey: .rating)) ?? 0
        label = (try? c.decode(String.self, forKey: .label)) ?? ""
        flag = (try? c.decode(Int.self, forKey: .flag)) ?? 0
        keywords = (try? c.decode([String].self, forKey: .keywords)) ?? []
        recipe = (try? c.decode(Recipe.self, forKey: .recipe)) ?? Recipe()
        versions = (try? c.decode([GradeVersion].self, forKey: .versions)) ?? []
    }
}

/// Suggested corrections from `araware_auto_analyze` (core/src/auto.rs).
struct AutoSuggestion: Codable {
    var rotation_deg: Double = 0
    var key_v: Double = 0
    var key_h: Double = 0
    var noise_luma: Double = 0
    var noise_chroma: Double = 0
    var ca_fix: Double = 0
    var dehaze: Double = 0
    var vibrance: Double = 0
    var zones_ev: [Double] = [Double](repeating: 0, count: 9)
    var straighten_conf: Double = 0
    var keystone_conf: Double = 0
    var noise_sigma: Double = 0
    var ca_score: Double = 0

    /// one-line human summary of the substantive suggestions
    var summary: String {
        var parts: [String] = []
        if rotation_deg != 0 { parts.append(String(format: "straighten %+.1f°", rotation_deg)) }
        if key_v != 0 { parts.append(String(format: "keystone %+.2f", key_v)) }
        if noise_luma > 0 { parts.append(String(format: "NR %.2f (σ=%.1f)", noise_luma, noise_sigma)) }
        if ca_fix > 0 { parts.append(String(format: "CA %.2f", ca_fix)) }
        if dehaze > 0 { parts.append(String(format: "dehaze %.2f", dehaze)) }
        if vibrance > 0 { parts.append(String(format: "vibrance %+.2f", vibrance)) }
        if zones_ev.contains(where: { $0 != 0 }) { parts.append("zone EQ set") }
        return parts.isEmpty ? "no suggestions" : parts.joined(separator: " · ")
    }
}
