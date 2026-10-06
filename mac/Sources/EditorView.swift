import SwiftUI
import ImageIO
import UniformTypeIdentifiers

let labelColors: [(name: String, color: Color)] = [
    ("red", .red), ("orange", .orange), ("yellow", .yellow),
    ("green", .green), ("blue", .blue), ("purple", .purple),
]

struct EditorView: View {
    let photo: Photo

    @State private var recipe = Recipe()
    @State private var rating = 0
    @State private var label = ""
    @State private var image: CGImage?
    @State private var hist: [[UInt32]] = []
    @State private var wave: [UInt32] = []
    @State private var vec: [UInt32] = []
    @State private var cie: [UInt32] = []
    @State private var rendering = false
    @State private var dirty = false
    @State private var status = ""
    @State private var renderTask: Task<Void, Never>?
    // tool: off|heal|dodge|burn|wbpick|clone|window|grad|flare
    @State private var retouchMode = "off"
    @State private var spotSize = 0.05
    @State private var lightRadius = 0.25
    @State private var lightEV = 0.5
    @State private var cloneRadius = 0.06
    @State private var pendingClone: [Double]? = nil
    @State private var compare = false
    @State private var showScopes = false
    @State private var scopeKind = "parade"
    @State private var curveChan = 0

    /// letterboxed image rect inside the preview area
    private func imageRect(in size: CGSize) -> CGRect {
        guard let image else { return .zero }
        let iw = CGFloat(image.width), ih = CGFloat(image.height)
        let pad: CGFloat = 8
        let avail = CGSize(width: size.width - pad * 2, height: size.height - pad * 2)
        let sc = min(avail.width / iw, avail.height / ih)
        let w = iw * sc, h = ih * sc
        return CGRect(x: (size.width - w) / 2, y: (size.height - h) / 2, width: w, height: h)
    }

    /// map a tap (view coords) into frame-normalized coords; crop-adjusted for
    /// frame-normalized (pre-crop) storages like spots/windows
    private func frameCoord(_ loc: CGPoint, in size: CGSize) -> (nx: Double, ny: Double, fx: Double, fy: Double)? {
        let rect = imageRect(in: size)
        guard rect.width > 0 else { return nil }
        let nx = Double((loc.x - rect.minX) / rect.width).clamped(to: 0...1)
        let ny = Double((loc.y - rect.minY) / rect.height).clamped(to: 0...1)
        let cl = recipe.crop[0], ct = recipe.crop[1]
        let sw = 1 - recipe.crop[0] - recipe.crop[2]
        let sh = 1 - recipe.crop[1] - recipe.crop[3]
        return (nx, ny, cl + nx * sw, ct + ny * sh)
    }

    private func placeAt(_ loc: CGPoint, in size: CGSize) {
        guard retouchMode != "off" else { return }
        guard let (nx, ny, fx, fy) = frameCoord(loc, in: size) else { return }
        switch retouchMode {
        case "heal":
            recipe.spots.append([fx, fy, spotSize, 0])
        case "dodge", "burn":
            recipe.lights.append([nx, ny, lightRadius, retouchMode == "dodge" ? lightEV : -lightEV])
        case "wbpick":
            recipe.wb_pick = [fx, fy]
            recipe.wb_mode = .pick
            retouchMode = "off"
        case "clone":
            if let s = pendingClone {
                recipe.clones.append([s[0], s[1], fx, fy, cloneRadius, 0])
                pendingClone = nil
            } else {
                pendingClone = [fx, fy]
                status = "Clone: tap destination"
            }
        case "window":
            var w = PowerWindow()
            w.kind = "circle"
            w.p = [fx, fy, 0.15, 0.15, 0, 0.4]
            recipe.windows.append(w)
        case "grad":
            var w = PowerWindow()
            w.kind = "gradient"
            w.p = [0.0, fy, 1.0, fy, 0.5, 0]
            recipe.windows.append(w)
        case "flare":
            recipe.flare[0] = nx
            recipe.flare[1] = ny
        default: break
        }
    }

    var body: some View {
        HSplitView {
            GeometryReader { geo in
                ZStack {
                    Color(white: 0.12)
                    if let image {
                        let rect = imageRect(in: geo.size)
                        Image(image, scale: 1, label: Text(photo.name))
                            .resizable()
                            .frame(width: rect.width, height: rect.height)
                            .position(x: rect.midX, y: rect.midY)
                        retouchMarkers(in: rect)
                        if compare {
                            Text("BEFORE").font(.caption.bold())
                                .padding(4).background(.black.opacity(0.6))
                                .foregroundStyle(.white)
                                .position(x: rect.minX + 30, y: rect.minY + 14)
                        }
                    } else {
                        ProgressView()
                    }
                }
                .contentShape(Rectangle())
                .gesture(SpatialTapGesture().onEnded { v in
                    placeAt(v.location, in: geo.size)
                })
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    HStack {
                        Text(photo.name).font(.headline).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        Stars(rating: $rating)
                            .onChange(of: rating) { _, r in
                                AraEngine.shared.setRating(path: photo.path, r)
                                dirty = true
                            }
                    }
                    HStack {
                        LabelPicker(label: $label)
                            .onChange(of: label) { _, l in
                                AraEngine.shared.setLabel(path: photo.path, l)
                                dirty = true
                            }
                        Spacer()
                        Toggle("Before", isOn: $compare)
                            .toggleStyle(.button)
                            .controlSize(.small)
                            .onChange(of: compare) { _, _ in rerender() }
                    }

                    if !hist.isEmpty {
                        HistogramView(hist: hist)
                            .frame(height: 88)
                            .padding(.vertical, 2)
                    }

                    DisclosureGroup("Scopes", isExpanded: $showScopes) {
                        VStack(spacing: 6) {
                            Picker("Scope", selection: $scopeKind) {
                                Text("Parade").tag("parade")
                                Text("Vector").tag("vector")
                                Text("CIE").tag("cie")
                            }
                            .pickerStyle(.segmented)
                            ScopesView(wave: wave, vec: vec, cie: cie, kind: scopeKind)
                                .frame(height: 120)
                        }
                    }
                    .font(.caption)

                    GroupBox("Light") {
                        VStack(spacing: 8) {
                            HStack {
                                Button("Auto") {
                                    recipe.wb_mode = .auto
                                    recipe.auto_exposure = true
                                    recipe.auto_contrast = true
                                }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                Toggle("Exp", isOn: $recipe.auto_exposure).font(.caption)
                                Toggle("Contrast", isOn: $recipe.auto_contrast).font(.caption)
                            }
                            SliderRow("Exposure", $recipe.exposure, -4...4, step: 0.05)
                            SliderRow("Contrast", $recipe.contrast, -1...1)
                            SliderRow("Pivot", $recipe.pivot, 0.05...0.5)
                            SliderRow("Highlights", $recipe.highlights, -1...1)
                            SliderRow("Shadows", $recipe.shadows, -1...1)
                            SliderRow("Whites", $recipe.whites, -1...1)
                            SliderRow("Blacks", $recipe.blacks, -1...1)
                            SliderRow("HL Roll", $recipe.highlight_rolloff, 0.5...2)
                            SliderRow("SH Roll", $recipe.shadow_rolloff, 0.5...2)
                        }
                    }
                    GroupBox("Color") {
                        VStack(spacing: 8) {
                            Picker("WB", selection: $recipe.wb_mode) {
                                Text("As shot").tag(WbMode.asShot)
                                Text("Auto").tag(WbMode.auto)
                                Text("Manual").tag(WbMode.manual)
                                Text("Pick").tag(WbMode.pick)
                            }
                            .pickerStyle(.segmented)
                            Button("Pick WB on image") { retouchMode = "wbpick" }
                                .controlSize(.small)
                                .disabled(retouchMode == "wbpick")
                            SliderRow("Temp", $recipe.temperature, -1...1)
                            SliderRow("Tint", $recipe.tint, -1...1)
                            SliderRow("Saturation", $recipe.saturation, -1...1)
                            SliderRow("Vibrance", $recipe.vibrance, -1...1)
                        }
                    }
                    GroupBox("Wheels") {
                        VStack(spacing: 8) {
                            Picker("Look", selection: $recipe.look) {
                                Text("None").tag("")
                                Text("Teal & Orange").tag("teal_orange")
                                Text("Film Fade").tag("film_fade")
                                Text("Bleach Bypass").tag("bleach")
                                Text("Noir").tag("noir")
                                Text("Matte").tag("matte")
                            }
                            .controlSize(.small)
                            HStack(spacing: 8) {
                                ColorWheel(title: "Lift", v: $recipe.lift, center: 0)
                                ColorWheel(title: "Gamma", v: $recipe.gamma, center: 1)
                                ColorWheel(title: "Gain", v: $recipe.gain, center: 1)
                                ColorWheel(title: "Offset", v: $recipe.offset, center: 0)
                            }
                            Text("Split Tone").font(.caption2).foregroundStyle(.secondary)
                            SliderRow("Shd Hue", $recipe.shadow_hue, 0...1)
                            SliderRow("Shd Sat", $recipe.shadow_sat, 0...1)
                            SliderRow("Mid Hue", $recipe.midtone_hue, 0...1)
                            SliderRow("Mid Sat", $recipe.midtone_sat, 0...1)
                            SliderRow("Hi Hue", $recipe.highlight_hue, 0...1)
                            SliderRow("Hi Sat", $recipe.highlight_sat, 0...1)
                        }
                    }
                    GroupBox("Zones") {
                        VStack(spacing: 6) {
                            ZoneRow("Dark", $recipe.z_dark)
                            ZoneRow("Shadow", $recipe.z_shadow)
                            ZoneRow("Light", $recipe.z_light)
                            ZoneRow("Global", $recipe.z_global)
                        }
                    }
                    GroupBox("Curves") {
                        VStack(spacing: 6) {
                            Picker("Ch", selection: $curveChan) {
                                Text("Y").tag(0)
                                Text("R").tag(1)
                                Text("G").tag(2)
                                Text("B").tag(3)
                                Text("H-H").tag(4)
                                Text("H-S").tag(5)
                                Text("H-L").tag(6)
                                Text("L-S").tag(7)
                                Text("S-S").tag(8)
                            }
                            .pickerStyle(.segmented)
                            .labelsHidden()
                            CurveEditor(points: curveBinding(curveChan))
                                .frame(height: 140)
                            HStack {
                                Button("Clear") { curveBinding(curveChan).wrappedValue = [] }
                                    .controlSize(.small)
                                Spacer()
                                Text(curveName(curveChan)).font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                    GroupBox("Qualifier") {
                        VStack(spacing: 8) {
                            Toggle("Enable", isOn: $recipe.q_enabled)
                                .controlSize(.small)
                                .onChange(of: recipe.q_enabled) { _, on in
                                    if !on { recipe.qh[1] = 0 }
                                    else if recipe.qh[1] == 0 { recipe.qh[1] = 0.1 }
                                }
                            if recipe.q_enabled {
                                SliderRow("Hue Ctr", $recipe.qh[0], 0...1)
                                SliderRow("Hue Wid", $recipe.qh[1], 0...0.5)
                                SliderRow("Hue Soft", $recipe.qh[2], 0.01...0.4)
                                SliderRow("Sat Lo", $recipe.qs[0], 0...1)
                                SliderRow("Sat Hi", $recipe.qs[1], 0...1)
                                SliderRow("Lum Lo", $recipe.ql[0], 0...1)
                                SliderRow("Lum Hi", $recipe.ql[1], 0...1)
                                Divider()
                                SliderRow("Hue Δ", $recipe.qadj[0], -0.5...0.5)
                                SliderRow("Sat Δ", $recipe.qadj[1], -1...1)
                                SliderRow("Lum Δ", $recipe.qadj[2], -1...1)
                                SliderRow("Temp Δ", $recipe.qadj[3], -1...1)
                                Toggle("Invert mask", isOn: $recipe.q_invert).font(.caption)
                            }
                        }
                    }
                    GroupBox("Windows") {
                        VStack(spacing: 6) {
                            HStack {
                                Button("＋ Circle") { retouchMode = "window" }
                                Button("＋ Grad") { retouchMode = "grad" }
                                Spacer()
                            }
                            .controlSize(.small)
                            .font(.caption)
                            ForEach(recipe.windows) { w in
                                if let i = recipe.windows.firstIndex(where: { $0.id == w.id }) {
                                    WindowRow(w: $recipe.windows[i]) {
                                        recipe.windows.remove(at: i)
                                    }
                                }
                            }
                        }
                    }
                    GroupBox("Mixer") {
                        VStack(spacing: 6) {
                            Text("R′ =").font(.caption2).foregroundStyle(.secondary)
                            MixRow($recipe.mixer, row: 0)
                            Text("G′ =").font(.caption2).foregroundStyle(.secondary)
                            MixRow($recipe.mixer, row: 1)
                            Text("B′ =").font(.caption2).foregroundStyle(.secondary)
                            MixRow($recipe.mixer, row: 2)
                            Toggle("Monochrome", isOn: Binding(
                                get: { recipe.mono != [0, 0, 0] },
                                set: { recipe.mono = $0 ? [0.21, 0.72, 0.07] : [0, 0, 0] }
                            ))
                            .font(.caption)
                            .controlSize(.small)
                            if recipe.mono != [0, 0, 0] {
                                TriRow("Mono", $recipe.mono, 0...1)
                            }
                        }
                    }
                    GroupBox("Retouch") {
                        VStack(spacing: 8) {
                            Picker("Tool", selection: $retouchMode) {
                                Text("Off").tag("off")
                                Text("Heal").tag("heal")
                                Text("Clone").tag("clone")
                                Text("Dodge").tag("dodge")
                                Text("Burn").tag("burn")
                            }
                            .pickerStyle(.segmented)
                            if retouchMode == "heal" {
                                SliderRow("Size", $spotSize, 0.01...0.15)
                            } else if retouchMode == "clone" {
                                SliderRow("Radius", $cloneRadius, 0.02...0.2)
                                if pendingClone != nil {
                                    Text("Tap destination").font(.caption2).foregroundStyle(.orange)
                                }
                            } else if retouchMode == "dodge" || retouchMode == "burn" {
                                SliderRow("Radius", $lightRadius, 0.05...0.6)
                                SliderRow("EV", $lightEV, 0...2)
                            }
                            if !recipe.spots.isEmpty || !recipe.lights.isEmpty || !recipe.clones.isEmpty {
                                ForEach(recipe.spots.indices, id: \.self) { i in
                                    MarkRow("Spot \(i + 1)") { recipe.spots.remove(at: i) }
                                }
                                ForEach(recipe.clones.indices, id: \.self) { i in
                                    MarkRow("Clone \(i + 1)") { recipe.clones.remove(at: i) }
                                }
                                ForEach(recipe.lights.indices, id: \.self) { i in
                                    MarkRow("Light \(i + 1) \(recipe.lights[i][3] >= 0 ? "+" : "")\(String(format: "%.1f", recipe.lights[i][3]))EV") {
                                        recipe.lights.remove(at: i)
                                    }
                                }
                            }
                            SliderRow("Crop L", $recipe.crop[0], 0...0.45)
                            SliderRow("Crop T", $recipe.crop[1], 0...0.45)
                            SliderRow("Crop R", $recipe.crop[2], 0...0.45)
                            SliderRow("Crop B", $recipe.crop[3], 0...0.45)
                        }
                    }
                    GroupBox("Detail") {
                        VStack(spacing: 8) {
                            SliderRow("Sharpen", $recipe.sharpen, 0...1)
                            SliderRow("Noise", $recipe.noise_luma, 0...1)
                            SliderRow("NR Chroma", $recipe.noise_chroma, 0...1)
                            SliderRow("Deband", $recipe.deband, 0...1)
                            SliderRow("CA Fix", $recipe.ca_fix, 0...1)
                            SliderRow("Beauty", $recipe.beauty, 0...1)
                        }
                    }
                    GroupBox("Effects") {
                        VStack(spacing: 8) {
                            SliderRow("Clarity", $recipe.clarity, -1...1)
                            SliderRow("Vignette", $recipe.vignette, -1...1)
                            SliderRow("Grain", $recipe.grain, 0...1)
                            SliderRow("Glow", $recipe.glow, 0...1)
                            SliderRow("Flare", $recipe.flare[2], 0...1)
                            SliderRow("Fl Hue", $recipe.flare[3], 0...1)
                            Button("Place flare on image") { retouchMode = "flare" }
                                .controlSize(.small)
                                .font(.caption)
                        }
                    }
                    GroupBox("Transform") {
                        VStack(spacing: 8) {
                            SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
                        }
                    }

                    HStack(spacing: 8) {
                        Button("Copy") { copyRecipe() }
                        Button("Paste") { pasteRecipe() }
                        Button("Reset") { recipe = Recipe() }
                    }
                    HStack {
                        Button("Save") { save() }
                        Button("Export JPEG…") { export() }
                        Spacer()
                        if rendering { ProgressView().controlSize(.small) }
                    }
                    if !status.isEmpty {
                        Text(status).font(.caption).foregroundStyle(.secondary)
                    }
                }
                .padding(14)
            }
            .frame(minWidth: 280, idealWidth: 300, maxWidth: 340)
        }
        .task { load() }
        .onChange(of: recipe) { _, _ in
            dirty = true
            scheduleRender()
        }
    }

    private func curveBinding(_ ch: Int) -> Binding<[[Double]]> {
        switch ch {
        case 0: return $recipe.curve
        case 1: return $recipe.curve_r
        case 2: return $recipe.curve_g
        case 3: return $recipe.curve_b
        case 4: return $recipe.hue_hue
        case 5: return $recipe.hue_sat
        case 6: return $recipe.hue_lum
        case 7: return $recipe.lum_sat
        default: return $recipe.sat_sat
        }
    }

    private func curveName(_ ch: Int) -> String {
        ["Luma", "Red", "Green", "Blue", "Hue vs Hue", "Hue vs Sat",
         "Hue vs Lum", "Lum vs Sat", "Sat vs Sat"][ch]
    }

    private func load() {
        let sc = AraEngine.shared.sidecar(path: photo.path)
        recipe = sc.recipe
        rating = sc.rating
        label = sc.label
        rerender()
    }

    private func save() {
        var sc = Sidecar()
        sc.rating = rating
        sc.label = label
        sc.recipe = recipe
        let stem = URL(fileURLWithPath: photo.path).deletingPathExtension().lastPathComponent
        status = AraEngine.shared.writeSidecar(path: photo.path, sc)
            ? "Saved \(stem).araware.json"
            : "Save failed: \(AraEngine.shared.lastError)"
        dirty = false
    }

    private func copyRecipe() {
        guard let data = try? JSONEncoder().encode(recipe),
              let js = String(data: data, encoding: .utf8) else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(js, forType: .string)
        status = "Recipe copied"
    }

    private func pasteRecipe() {
        guard let js = NSPasteboard.general.string(forType: .string),
              let data = js.data(using: .utf8),
              let r = try? JSONDecoder().decode(Recipe.self, from: data) else {
            status = "No recipe on clipboard"
            return
        }
        recipe = r
        status = "Recipe pasted"
    }

    private func scheduleRender() {
        renderTask?.cancel()
        renderTask = Task {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard !Task.isCancelled else { return }
            rerender()
        }
    }

    private func rerender() {
        rendering = true
        let r = compare ? Recipe() : recipe
        Task.detached { [path = photo.path] in
            let (img, bins, wv, vc, ce) = await AraEngine.shared.work {
                $0.renderScopes(path: path, recipe: r, maxPx: 1400)
            }
            await MainActor.run {
                if let img { image = img }
                hist = (0..<4).map { ch in Array(bins[(ch * 256)..<(ch * 256 + 256)]) }
                wave = wv
                vec = vc
                cie = ce
                rendering = false
            }
        }
    }

    private func export() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.jpeg]
        panel.nameFieldStringValue = photo.name.replacingOccurrences(
            of: "." + (photo.name.split(separator: ".").last.map(String.init) ?? ""),
            with: ".jpg")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        rendering = true
        status = "Exporting…"
        let r = recipe
        Task.detached { [path = photo.path] in
            let img = await AraEngine.shared.work { $0.export(path: path, recipe: r) }
            await MainActor.run {
                rendering = false
                guard let img,
                      let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
                else {
                    status = "Export failed: \(AraEngine.shared.lastError)"
                    return
                }
                CGImageDestinationAddImage(dest, img, [kCGImageDestinationLossyCompressionQuality: 0.92] as CFDictionary)
                status = CGImageDestinationFinalize(dest) ? "Exported \(url.lastPathComponent)" : "Export failed"
            }
        }
    }
}

/// DaVinci-style color wheel: drag sets chroma offset; the vertical slider on
/// the right sets the luma/mean component. `v` holds absolute rgb values:
/// lift/offset are offsets around 0, gamma/gain multipliers around 1.
struct ColorWheel: View {
    let title: String
    @Binding var v: [Double]
    /// neutral mean: 0 for additive wheels, 1 for multiplicative
    let center: Double

    private var mean: Double { (v[0] + v[1] + v[2]) / 3 }
    private var chroma: (u: Double, w: Double) {
        // project rgb onto a zero-mean chroma plane
        let m = mean
        let d = (v[0] - m, v[1] - m, v[2] - m)
        return (d.0 - (d.1 + d.2) / 2, (d.1 - d.2) * 0.8660)
    }
    private var amplitude: Double {
        let c = chroma
        return (c.u * c.u + c.w * c.w).squareRoot()
    }

    var body: some View {
        VStack(spacing: 2) {
            GeometryReader { geo in
                let s = min(geo.size.width, geo.size.height)
                let cx = geo.size.width / 2, cy = geo.size.height / 2
                let r = s / 2 - 4
                ZStack {
                    Circle().fill(
                        AngularGradient(colors: [
                            .red, .yellow, .green, .cyan, .blue, Color(red: 1, green: 0, blue: 1), .red,
                        ], center: .center)
                    )
                    .opacity(0.35)
                    Circle().stroke(.quaternary)
                    let c = chroma
                    let bx = cx + CGFloat(c.u) * r * 4
                    let by = cy - CGFloat(c.w) * r * 4
                    Circle()
                        .fill(.white)
                        .frame(width: 8, height: 8)
                        .overlay(Circle().stroke(.black.opacity(0.6), lineWidth: 0.5))
                        .position(x: bx, y: by)
                }
                .frame(width: s, height: s)
                .position(x: geo.size.width / 2, y: geo.size.height / 2)
                .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                    let dx = Double(g.location.x - cx) / Double(r)
                    let dy = Double(cy - g.location.y) / Double(r)
                    let m = min((dx * dx + dy * dy).squareRoot(), 1.0) / 4
                    let a = atan2(dy, dx)
                    // zero-mean hue vector -> chroma offset
                    let h = a / (2 * .pi)
                    let rgb = hueRgb(h)
                    let cm = (rgb.0 + rgb.1 + rgb.2) / 3
                    v = [
                        mean + m * (rgb.0 - cm) * 1.7,
                        mean + m * (rgb.1 - cm) * 1.7,
                        mean + m * (rgb.2 - cm) * 1.7,
                    ]
                })
            }
            .aspectRatio(1, contentMode: .fit)
            Slider(value: Binding(
                get: { mean },
                set: { m in
                    let d = (v[0] - mean, v[1] - mean, v[2] - mean)
                    v = [m + d.0, m + d.1, m + d.2]
                }),
                in: center == 0 ? -0.25...0.25 : 0.4...1.6)
            .controlSize(.mini)
            Text(title).font(.caption2).foregroundStyle(.secondary)
        }
    }
}

private func hueRgb(_ h: Double) -> (Double, Double, Double) {
    let h6 = (h - h.rounded(.down)) * 6
    let i = Int(h6) % 6
    let f = h6 - h6.rounded(.down)
    let seg = [(1.0, 0.0, 0.0), (1.0, 1.0, 0.0), (0.0, 1.0, 0.0),
               (0.0, 1.0, 1.0), (0.0, 0.0, 1.0), (1.0, 0.0, 1.0)]
    let a = seg[i], b = seg[(i + 1) % 6]
    return (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
}

/// HDR/log zone wheel row: hue, amount, exposure, sat for one tonal band.
struct ZoneRow: View {
    let title: String
    @Binding var z: [Double]
    init(_ t: String, _ z: Binding<[Double]>) {
        title = t
        _z = z
    }
    var body: some View {
        VStack(spacing: 4) {
            HStack {
                Text(title).font(.caption).frame(width: 44, alignment: .leading)
                Slider(value: $z[2], in: -1...1)
                Text(String(format: "%+.2f", z[2]))
                    .font(.caption2.monospacedDigit()).frame(width: 40)
            }
            HStack {
                Text("Hue").font(.caption2).frame(width: 44, alignment: .leading)
                    .foregroundStyle(.secondary)
                Slider(value: $z[0], in: 0...1)
                Slider(value: $z[1], in: 0...1)
                Slider(value: $z[3], in: -1...1)
            }
        }
    }
}

/// curve editor: click to add, drag to move, double-click a point to delete
struct CurveEditor: View {
    @Binding var points: [[Double]]

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width, h = geo.size.height
            ZStack {
                Rectangle().fill(Color(white: 0.16))
                Canvas { ctx, size in
                    // grid
                    for i in 1..<4 {
                        let f = CGFloat(i) / 4
                        var gp = Path()
                        gp.move(to: .init(x: f * size.width, y: 0))
                        gp.addLine(to: .init(x: f * size.width, y: size.height))
                        gp.move(to: .init(x: 0, y: f * size.height))
                        gp.addLine(to: .init(x: size.width, y: f * size.height))
                        ctx.stroke(gp, with: .color(.white.opacity(0.08)), lineWidth: 0.5)
                    }
                    // diagonal reference
                    var dp = Path()
                    dp.move(to: .init(x: 0, y: size.height))
                    dp.addLine(to: .init(x: size.width, y: 0))
                    ctx.stroke(dp, with: .color(.white.opacity(0.15)), lineWidth: 0.5)
                    let pts = sorted()
                    if pts.count > 1 {
                        var p = Path()
                        // draw catmull-rom-ish via sampled cubic smooth
                        for i in 0...64 {
                            let x = Double(i) / 64
                            let y = eval(pts, x)
                            let px = x * size.width
                            let py = (1 - y) * size.height
                            if i == 0 { p.move(to: .init(x: px, y: py)) }
                            else { p.addLine(to: .init(x: px, y: py)) }
                        }
                        ctx.stroke(p, with: .color(.white), lineWidth: 1.5)
                    }
                    for pt in pts {
                        let px = pt[0] * size.width
                        let py = (1 - pt[1]) * size.height
                        let dot = Circle().path(in: CGRect(x: px - 4, y: py - 4, width: 8, height: 8))
                        ctx.fill(dot, with: .color(.orange))
                    }
                }
            }
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { g in
                    let x = Double(g.location.x / w).clamped(to: 0...1)
                    let y = Double(1 - g.location.y / h).clamped(to: 0...1)
                    if g.translation == .zero, points.isEmpty == false,
                       let i = nearest(x, y), dist(points[i], x, y) < 0.06 {
                        points[i] = [x, y]
                        return
                    }
                    if let i = dragging(points, g.startLocation, w, h) {
                        points[i] = [x, y]
                    }
                }
                .onEnded { g in
                    let x = Double(g.location.x / w).clamped(to: 0...1)
                    let y = Double(1 - g.location.y / h).clamped(to: 0...1)
                    if nearest(x, y) == nil || dist(points[nearest(x, y)!], x, y) > 0.06 {
                        points.append([x, y])
                    }
                })
            .onTapGesture(count: 2) { loc in
                let x = Double(loc.x / w), y = Double(1 - loc.y / h)
                if let i = nearest(x, y), dist(points[i], x, y) < 0.08, points.count > 0 {
                    points.remove(at: i)
                }
            }
        }
    }

    private func sorted() -> [[Double]] {
        points.sorted { $0[0] < $1[0] }
    }
    private func nearest(_ x: Double, _ y: Double) -> Int? {
        guard !points.isEmpty else { return nil }
        var bi = 0, bd = Double.greatestFiniteMagnitude
        for (i, p) in points.enumerated() {
            let d = dist(p, x, y)
            if d < bd { bd = d; bi = i }
        }
        return bi
    }
    private func dist(_ p: [Double], _ x: Double, _ y: Double) -> Double {
        (p[0] - x) * (p[0] - x) + (p[1] - y) * (p[1] - y)
    }
    private func dragging(_ pts: [[Double]], _ start: CGPoint, _ w: CGFloat, _ h: CGFloat) -> Int? {
        let x = Double(start.x / w), y = Double(1 - start.y / h)
        return nearest(x, y)
    }
    /// catmull-rom evaluation matching the engine's curve_lut
    private func eval(_ pts: [[Double]], _ x: Double) -> Double {
        if pts.isEmpty { return x }
        if x <= pts.first![0] { return pts.first![1] }
        if x >= pts.last![0] { return pts.last![1] }
        var i = 0
        while i + 1 < pts.count && pts[i + 1][0] < x { i += 1 }
        let p0 = pts[max(i - 1, 0)], p1 = pts[i], p2 = pts[min(i + 1, pts.count - 1)],
            p3 = pts[min(i + 2, pts.count - 1)]
        let t = (x - p1[0]) / max(p2[0] - p1[0], 1e-6)
        let t2 = t * t, t3 = t2 * t
        return 0.5 * (2 * p1[1] + (-p0[1] + p2[1]) * t
            + (2 * p0[1] - 5 * p1[1] + 4 * p2[1] - p3[1]) * t2
            + (-p0[1] + 3 * p1[1] - 3 * p2[1] + p3[1]) * t3)
    }
}

/// scopes: parade waveform / vectorscope / CIE xy rendered as image planes
struct ScopesView: View {
    let wave: [UInt32]
    let vec: [UInt32]
    let cie: [UInt32]
    let kind: String

    var body: some View {
        Canvas { ctx, size in
            switch kind {
            case "parade":
                if wave.count >= 196608 {
                    let cols: [Color] = [.red, .green, .blue]
                    for c in 0..<3 {
                        let maxv = Double(wave[(c * 65536)..<(c * 65536 + 65536)].max() ?? 1).squareRoot()
                        for v in 0..<256 {
                            for x in 0..<256 {
                                let n = wave[c * 65536 + v * 256 + x]
                                if n == 0 { continue }
                                let a = min(Double(n).squareRoot() / maxv, 1)
                                let px = CGFloat(x) / 256 * size.width
                                let py = size.height - CGFloat(v) / 256 * size.height
                                ctx.fill(
                                    Path(CGRect(x: px, y: py, width: size.width / 256 + 0.5, height: size.height / 256 + 0.5)),
                                    with: .color(cols[c].opacity(a)))
                            }
                        }
                    }
                }
            case "vector":
                if vec.count >= 65536 {
                    // graticule
                    ctx.stroke(Circle().path(in: CGRect(
                        x: size.width * 0.15, y: size.height * 0.15,
                        width: size.width * 0.7, height: size.height * 0.7)),
                        with: .color(.white.opacity(0.15)))
                    let maxv = Double(vec.max() ?? 1).squareRoot()
                    for y in 0..<256 {
                        for x in 0..<256 {
                            let n = vec[y * 256 + x]
                            if n == 0 { continue }
                            let a = min(Double(n).squareRoot() / maxv, 1)
                            let px = CGFloat(x) / 256 * size.width
                            let py = CGFloat(255 - y) / 256 * size.height
                            ctx.fill(
                                Path(CGRect(x: px, y: py, width: size.width / 256 + 0.5, height: size.height / 256 + 0.5)),
                                with: .color(.cyan.opacity(a)))
                        }
                    }
                }
            default:
                if cie.count >= 65536 {
                    let maxv = Double(cie.max() ?? 1).squareRoot()
                    for y in 0..<256 {
                        for x in 0..<256 {
                            let n = cie[y * 256 + x]
                            if n == 0 { continue }
                            let a = min(Double(n).squareRoot() / maxv, 1)
                            let px = CGFloat(x) / 256 * size.width
                            let py = size.height - CGFloat(y) / 256 * size.height
                            ctx.fill(
                                Path(CGRect(x: px, y: py, width: size.width / 256 + 0.5, height: size.height / 256 + 0.5)),
                                with: .color(.green.opacity(a)))
                        }
                    }
                }
            }
        }
        .background(Color(white: 0.13))
        .clipShape(RoundedRectangle(cornerRadius: 4))
        .overlay(RoundedRectangle(cornerRadius: 4).stroke(.quaternary))
    }
}

/// R,G,B + luma overlay histogram (log scale).
struct HistogramView: View {
    let hist: [[UInt32]]

    var body: some View {
        Canvas { ctx, size in
            let chans: [(Int, Color)] = [
                (0, .red.opacity(0.55)), (1, .green.opacity(0.55)), (2, .blue.opacity(0.55)),
                (3, .white.opacity(0.7)),
            ]
            let maxv = hist.flatMap { $0 }.map { log1p(Double($0)) }.max() ?? 1
            for (ch, color) in chans where hist.count > ch {
                var p = Path()
                for x in 0..<256 {
                    let v = log1p(Double(hist[ch][x])) / maxv
                    let px = CGFloat(x) / 255 * size.width
                    let py = size.height - CGFloat(v) * size.height
                    if x == 0 { p.move(to: CGPoint(x: px, y: py)) } else { p.addLine(to: CGPoint(x: px, y: py)) }
                }
                ctx.stroke(p, with: .color(color), lineWidth: 1)
            }
        }
        .background(Color(white: 0.15))
        .clipShape(RoundedRectangle(cornerRadius: 4))
        .overlay(RoundedRectangle(cornerRadius: 4).stroke(.quaternary))
    }
}

struct LabelPicker: View {
    @Binding var label: String

    var body: some View {
        HStack(spacing: 4) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 14, height: 14)
                    .overlay(Circle().stroke(.primary, lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.45)
                    .onTapGesture { label = (label == l.name) ? "" : l.name }
            }
            if !label.isEmpty {
                Button("Clear") { label = "" }
                    .font(.caption2)
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

extension EditorView {
    /// marker overlay for heal spots / dodge-burn lights / clones / windows / flare
    @ViewBuilder
    func retouchMarkers(in rect: CGRect) -> some View {
        let cl = recipe.crop[0], ct = recipe.crop[1]
        let sw = (1 - recipe.crop[0] - recipe.crop[2])
        let sh = (1 - recipe.crop[1] - recipe.crop[3])
        Canvas { ctx, _ in
        let fx2sx = { (fx: Double) -> CGFloat in rect.minX + CGFloat((fx - cl) / max(sw, 0.01)) * rect.width }
        let fy2sy = { (fy: Double) -> CGFloat in rect.minY + CGFloat((fy - ct) / max(sh, 0.01)) * rect.height }
        for s in recipe.spots {
            let cx = fx2sx(s[0])
            let cy = fy2sy(s[1])
            let r = s[2] * rect.width / max(sw, 0.01)
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            ctx.stroke(path, with: .color(.red.opacity(0.9)), lineWidth: 1.5)
        }
        for c in recipe.clones {
            let sx = fx2sx(c[0]), sy = fy2sy(c[1])
            let dx = fx2sx(c[2]), dy = fy2sy(c[3])
            let r = c[4] * rect.width / max(sw, 0.01)
            ctx.stroke(Circle().path(in: CGRect(x: sx - r, y: sy - r, width: r * 2, height: r * 2)),
                       with: .color(.green.opacity(0.9)), lineWidth: 1.5)
            ctx.stroke(Circle().path(in: CGRect(x: dx - r, y: dy - r, width: r * 2, height: r * 2)),
                       with: .color(.orange.opacity(0.9)), lineWidth: 1.5)
            var ln = Path()
            ln.move(to: .init(x: sx, y: sy))
            ln.addLine(to: .init(x: dx, y: dy))
            ctx.stroke(ln, with: .color(.white.opacity(0.5)), lineWidth: 0.8)
        }
        for l in recipe.lights {
            let cx = rect.minX + l[0] * rect.width
            let cy = rect.minY + l[1] * rect.height
            let r = l[2] * rect.width
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            let col: Color = l[3] >= 0 ? .yellow : .purple
            ctx.stroke(path, with: .color(col.opacity(0.9)), lineWidth: 1.5)
        }
        for w in recipe.windows {
            if w.kind == "gradient" {
                var ln = Path()
                ln.move(to: .init(x: rect.minX + w.p[0] * rect.width, y: rect.minY + w.p[1] * rect.height))
                ln.addLine(to: .init(x: rect.minX + w.p[2] * rect.width, y: rect.minY + w.p[3] * rect.height))
                ctx.stroke(ln, with: .color(.cyan.opacity(0.9)), lineWidth: 1.5)
            } else {
                let cx = fx2sx(w.p[0])
                let cy = fy2sy(w.p[1])
                let rx = w.p[2] * rect.width / max(sw, 0.01)
                let ry = w.p[3] * rect.height / max(sh, 0.01)
                ctx.stroke(Ellipse().path(in: CGRect(x: cx - rx, y: cy - ry, width: rx * 2, height: ry * 2)),
                           with: .color(.cyan.opacity(0.9)), lineWidth: 1.5)
            }
        }
        if recipe.wb_mode == .pick {
            let cx = fx2sx(recipe.wb_pick[0])
            let cy = fy2sy(recipe.wb_pick[1])
            var cross = Path()
            cross.move(to: .init(x: cx - 8, y: cy)); cross.addLine(to: .init(x: cx + 8, y: cy))
            cross.move(to: .init(x: cx, y: cy - 8)); cross.addLine(to: .init(x: cx, y: cy + 8))
            ctx.stroke(cross, with: .color(.white), lineWidth: 1.5)
        }
        if recipe.flare[2] > 0 {
            let cx = rect.minX + recipe.flare[0] * rect.width
            let cy = rect.minY + recipe.flare[1] * rect.height
            var cross = Path()
            cross.move(to: .init(x: cx - 10, y: cy)); cross.addLine(to: .init(x: cx + 10, y: cy))
            cross.move(to: .init(x: cx, y: cy - 10)); cross.addLine(to: .init(x: cx, y: cy + 10))
            ctx.stroke(cross, with: .color(.yellow), lineWidth: 1.5)
        }
        }
    }
}

/// delete row for a placed spot/light
struct MarkRow: View {
    let title: String
    let onDelete: () -> Void
    init(_ title: String, onDelete: @escaping () -> Void) {
        self.title = title
        self.onDelete = onDelete
    }
    var body: some View {
        HStack {
            Text(title).font(.caption)
            Spacer()
            Button("×") { onDelete() }.buttonStyle(.plain).foregroundStyle(.red)
        }
    }
}

/// three-channel row (R G B) bound to a [Double] of length 3
struct TriRow: View {
    let title: String
    @Binding var v: [Double]
    let range: ClosedRange<Double>
    init(_ title: String, _ v: Binding<[Double]>, _ range: ClosedRange<Double>) {
        self.title = title
        self._v = v
        self.range = range
    }
    var body: some View {
        if v.count == 3 {
            HStack(spacing: 6) {
                Text(title).font(.caption).frame(width: 40, alignment: .leading)
                ForEach(0..<3, id: \.self) { i in
                    Slider(value: $v[i], in: range)
                }
            }
        }
    }
}

/// one row of the RGB mixer matrix
struct MixRow: View {
    @Binding var m: [Double]
    let row: Int
    init(_ m: Binding<[Double]>, row: Int) {
        _m = m
        self.row = row
    }
    var body: some View {
        if m.count == 9 {
            HStack(spacing: 6) {
                ForEach(0..<3, id: \.self) { c in
                    Slider(value: $m[row * 3 + c], in: -1...2)
                }
            }
        }
    }
}

/// per-window editor rows
struct WindowRow: View {
    @Binding var w: PowerWindow
    let onDelete: () -> Void
    var body: some View {
        VStack(spacing: 4) {
            HStack {
                Text(w.kind == "gradient" ? "Gradient" : "Circle").font(.caption)
                Spacer()
                Toggle("Inv", isOn: $w.invert).font(.caption2).controlSize(.mini)
                Button("×") { onDelete() }.buttonStyle(.plain).foregroundStyle(.red)
            }
            SliderRow("EV", $w.ev, -2...2)
            SliderRow("Sat", $w.sat, -1...1)
            SliderRow("Temp", $w.temp, -1...1)
            if w.kind == "circle" {
                SliderRow("Size", $w.p[2], 0.02...0.6)
                SliderRow("Ratio", $w.p[3], 0.02...0.6)
                SliderRow("Rot", $w.p[4], -90...90)
            }
            SliderRow("Soft", $w.p[w.kind == "circle" ? 5 : 4], 0.02...1)
        }
        .padding(6)
        .background(Color(white: 0.14))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}

private extension Double {
    func clamped(to r: ClosedRange<Double>) -> Double { min(max(self, r.lowerBound), r.upperBound) }
}

struct SliderRow: View {
    let title: String
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.01

    init(_ title: String, _ value: Binding<Double>, _ range: ClosedRange<Double>, step: Double = 0.01) {
        self.title = title
        self._value = value
        self.range = range
        self.step = step
    }

    var body: some View {
        HStack(spacing: 8) {
            Text(title).font(.caption).frame(width: 70, alignment: .leading)
            Slider(value: $value, in: range)
            Text(String(format: "%.2f", value))
                .font(.caption.monospacedDigit())
                .frame(width: 44, alignment: .trailing)
        }
    }
}

struct Stars: View {
    @Binding var rating: Int
    var body: some View {
        HStack(spacing: 2) {
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .foregroundStyle(.yellow)
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
        }
    }
}
