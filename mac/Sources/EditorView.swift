import SwiftUI
import ImageIO
import UniformTypeIdentifiers

let labelColors: [(name: String, color: Color)] = [
    ("red", .red), ("orange", .orange), ("yellow", .yellow),
    ("green", .green), ("blue", .blue), ("purple", .purple),
]

struct EditorView: View {
    let photo: Photo
    @EnvironmentObject var store: LibraryStore

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
    @State private var showScopes = true
    @State private var scopeKind = "parade"
    @State private var curveChan = 0

    /// letterboxed image rect inside the preview area
    private func imageRect(in size: CGSize) -> CGRect {
        guard let image else { return .zero }
        let iw = CGFloat(image.width), ih = CGFloat(image.height)
        let pad: CGFloat = 10
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
            stageColumn
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            inspector
                .frame(minWidth: 300, idealWidth: 316, maxWidth: 380)
        }
        .background(Ara.bg0)
        .task { load() }
        .onChange(of: recipe) { _, _ in
            dirty = true
            scheduleRender()
        }
    }

    // MARK: stage (image + filmstrip)

    private var stageColumn: some View {
        VStack(spacing: 0) {
            GeometryReader { geo in
                ZStack {
                    Ara.bg0
                    if let image {
                        let rect = imageRect(in: geo.size)
                        Image(image, scale: 1, label: Text(photo.name))
                            .resizable()
                            .frame(width: rect.width, height: rect.height)
                            .position(x: rect.midX, y: rect.midY)
                            .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                            .overlay {
                                Rectangle()
                                    .stroke(Ara.border, lineWidth: 0.5)
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                            }
                        retouchMarkers(in: rect)
                        if compare {
                            Text("BEFORE")
                                .font(.system(size: 10, weight: .bold))
                                .tracking(1.5)
                                .padding(.horizontal, 8).padding(.vertical, 3)
                                .background(Capsule().fill(Ara.accent))
                                .foregroundStyle(Color.black.opacity(0.85))
                                .position(x: rect.minX + 42, y: rect.minY + 16)
                        }
                        if retouchMode != "off" {
                            toolBanner
                                .position(x: rect.midX, y: rect.maxY - 20)
                        }
                    } else {
                        ProgressView()
                            .tint(Ara.accent)
                    }
                }
                .contentShape(Rectangle())
                .gesture(SpatialTapGesture().onEnded { v in
                    placeAt(v.location, in: geo.size)
                })
            }
            filmstrip
        }
    }

    @ViewBuilder
    private var toolBanner: some View {
        let names: [String: String] = [
            "heal": "Heal — tap blemishes", "clone": pendingClone == nil ? "Clone — tap source" : "Clone — tap destination",
            "dodge": "Dodge — tap to lighten", "burn": "Burn — tap to darken",
            "wbpick": "Pick WB — tap a neutral point", "window": "Window — tap centre",
            "grad": "Gradient — tap edge line", "flare": "Flare — tap light position",
        ]
        Text(names[retouchMode] ?? retouchMode)
            .font(.system(size: 10.5, weight: .medium))
            .foregroundStyle(Ara.text1)
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(Capsule().fill(.black.opacity(0.72))
                .overlay(Capsule().stroke(Ara.accent.opacity(0.5), lineWidth: 0.5)))
    }

    private var filmstrip: some View {
        VStack(spacing: 0) {
            Ara.hairline.frame(height: 1)
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(store.filtered) { p in
                            FilmCell(photo: p, selected: p == store.selection)
                                .onTapGesture { store.selection = p }
                                .id(p.id)
                        }
                    }
                    .padding(.horizontal, 10).padding(.vertical, 8)
                }
                .frame(height: 74)
                .onAppear { proxy.scrollTo(photo.id, anchor: .center) }
            }
            .background(Ara.bg1)
        }
    }

    // MARK: inspector

    private var inspector: some View {
        VStack(spacing: 0) {
            // header: filename, stars, labels, compare
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Text(photo.name)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Ara.text1)
                        .lineLimit(1).truncationMode(.middle)
                    if dirty {
                        Circle().fill(Ara.accent).frame(width: 6, height: 6)
                    }
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
                    IconAction(icon: "eye", label: "Before", active: compare) {
                        compare.toggle()
                        rerender()
                    }
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .background(Ara.bg1)
            .overlay(alignment: .bottom) { Ara.hairline.frame(height: 1) }

            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    Panel("Histogram") {
                        if !hist.isEmpty {
                            HistogramView(hist: hist).frame(height: 84)
                        } else {
                            Rectangle().fill(Ara.bg3).frame(height: 84)
                                .overlay(ProgressView().tint(Ara.text3))
                        }
                    }
                    Panel("Scopes", expanded: $showScopes) {
                        SegPicker([("parade", "Parade"), ("vector", "Vector"), ("cie", "CIE")],
                                  selection: $scopeKind)
                        ScopesView(wave: wave, vec: vec, cie: cie, kind: scopeKind)
                            .frame(height: 128)
                    }
                    Panel("Light", trailing: {
                        HStack(spacing: 4) {
                            ToolChip(label: "Auto", icon: "wand.and.stars") {
                                recipe.wb_mode = .auto
                                recipe.auto_exposure = true
                                recipe.auto_contrast = true
                            }
                        }
                    }) {
                        HStack(spacing: 6) {
                            Toggle("Auto Exp", isOn: $recipe.auto_exposure)
                                .font(.system(size: 10)).foregroundStyle(Ara.text2)
                                .controlSize(.mini)
                            Toggle("Auto Contrast", isOn: $recipe.auto_contrast)
                                .font(.system(size: 10)).foregroundStyle(Ara.text2)
                                .controlSize(.mini)
                        }
                        SliderRow("Exposure", $recipe.exposure, -4...4, step: 0.05)
                        SliderRow("Contrast", $recipe.contrast, -1...1)
                        SliderRow("Pivot", $recipe.pivot, 0.05...0.5, reset: 0.18)
                        SliderRow("Highlights", $recipe.highlights, -1...1)
                        SliderRow("Shadows", $recipe.shadows, -1...1)
                        SliderRow("Whites", $recipe.whites, -1...1)
                        SliderRow("Blacks", $recipe.blacks, -1...1)
                        SliderRow("HL Roll", $recipe.highlight_rolloff, 0.5...2, reset: 1.0)
                        SliderRow("SH Roll", $recipe.shadow_rolloff, 0.5...2, reset: 1.0)
                    }
                    Panel("Color", trailing: {
                        ToolChip(label: "Pick WB", icon: "eyedropper", active: retouchMode == "wbpick") {
                            retouchMode = retouchMode == "wbpick" ? "off" : "wbpick"
                        }
                    }) {
                        SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                                   (.manual, "Manual"), (.pick, "Pick")],
                                  selection: $recipe.wb_mode)
                        SliderRow("Temp", $recipe.temperature, -1...1)
                        SliderRow("Tint", $recipe.tint, -1...1)
                        SliderRow("Saturation", $recipe.saturation, -1...1)
                        SliderRow("Vibrance", $recipe.vibrance, -1...1)
                    }
                    Panel("Wheels", trailing: { LookPicker(look: $recipe.look) }) {
                        HStack(spacing: 6) {
                            ColorWheel(title: "Lift", v: $recipe.lift, center: 0)
                            ColorWheel(title: "Gamma", v: $recipe.gamma, center: 1)
                            ColorWheel(title: "Gain", v: $recipe.gain, center: 1)
                            ColorWheel(title: "Offset", v: $recipe.offset, center: 0)
                        }
                        Text("SPLIT TONE")
                            .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                            .foregroundStyle(Ara.text3)
                        SliderRow("Shd Hue", $recipe.shadow_hue, 0...1, reset: 0.55)
                        SliderRow("Shd Sat", $recipe.shadow_sat, 0...1)
                        SliderRow("Mid Hue", $recipe.midtone_hue, 0...1, reset: 0.55)
                        SliderRow("Mid Sat", $recipe.midtone_sat, 0...1)
                        SliderRow("Hi Hue", $recipe.highlight_hue, 0...1, reset: 0.08)
                        SliderRow("Hi Sat", $recipe.highlight_sat, 0...1)
                    }
                    Panel("Zones") {
                        ZoneRow("Dark", $recipe.z_dark)
                        ZoneRow("Shadow", $recipe.z_shadow)
                        ZoneRow("Light", $recipe.z_light)
                        ZoneRow("Global", $recipe.z_global)
                    }
                    Panel("Curves") {
                        SegPicker([(0, "Y"), (1, "R"), (2, "G"), (3, "B"),
                                   (4, "H·H"), (5, "H·S"), (6, "H·L"), (7, "L·S"), (8, "S·S")],
                                  selection: $curveChan)
                        CurveEditor(points: curveBinding(curveChan),
                                    tint: curveTint(curveChan))
                            .frame(height: 132)
                        HStack {
                            ToolChip(label: "Clear", icon: "xmark") {
                                curveBinding(curveChan).wrappedValue = []
                            }
                            Spacer()
                            Text(curveName(curveChan))
                                .font(.system(size: 9.5)).foregroundStyle(Ara.text3)
                        }
                    }
                    Panel("Qualifier") {
                        HStack {
                            Text("Enable").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                            Spacer()
                            Toggle("", isOn: $recipe.q_enabled)
                                .labelsHidden().controlSize(.mini).tint(Ara.accent)
                                .onChange(of: recipe.q_enabled) { _, on in
                                    if !on { recipe.qh[1] = 0 }
                                    else if recipe.qh[1] == 0 { recipe.qh[1] = 0.1 }
                                }
                        }
                        if recipe.q_enabled {
                            SliderRow("Hue Ctr", $recipe.qh[0], 0...1)
                            SliderRow("Hue Wid", $recipe.qh[1], 0...0.5, reset: 0.1)
                            SliderRow("Hue Soft", $recipe.qh[2], 0.01...0.4, reset: 0.1)
                            SliderRow("Sat Lo", $recipe.qs[0], 0...1)
                            SliderRow("Sat Hi", $recipe.qs[1], 0...1, reset: 1)
                            SliderRow("Lum Lo", $recipe.ql[0], 0...1)
                            SliderRow("Lum Hi", $recipe.ql[1], 0...1, reset: 1)
                            Divider().overlay(Ara.hairline)
                            SliderRow("Hue Δ", $recipe.qadj[0], -0.5...0.5)
                            SliderRow("Sat Δ", $recipe.qadj[1], -1...1)
                            SliderRow("Lum Δ", $recipe.qadj[2], -1...1)
                            SliderRow("Temp Δ", $recipe.qadj[3], -1...1)
                            HStack {
                                Text("Invert mask").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                                Spacer()
                                Toggle("", isOn: $recipe.q_invert)
                                    .labelsHidden().controlSize(.mini).tint(Ara.accent)
                            }
                        }
                    }
                    Panel("Windows", trailing: {
                        HStack(spacing: 4) {
                            ToolChip(label: "Circle", icon: "plus.circle", active: retouchMode == "window") {
                                retouchMode = retouchMode == "window" ? "off" : "window"
                            }
                            ToolChip(label: "Grad", icon: "plus.rectangle", active: retouchMode == "grad") {
                                retouchMode = retouchMode == "grad" ? "off" : "grad"
                            }
                        }
                    }) {
                        if recipe.windows.isEmpty {
                            Text("Add a circle or gradient window, then tap the image to place it.")
                                .font(.system(size: 10)).foregroundStyle(Ara.text3)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        ForEach(recipe.windows) { w in
                            if let i = recipe.windows.firstIndex(where: { $0.id == w.id }) {
                                WindowRow(w: $recipe.windows[i]) {
                                    recipe.windows.remove(at: i)
                                }
                            }
                        }
                    }
                    Panel("Mixer") {
                        VStack(spacing: 5) {
                            ForEach(0..<3, id: \.self) { row in
                                HStack(spacing: 5) {
                                    Text(["R′", "G′", "B′"][row])
                                        .font(.system(size: 10, weight: .semibold))
                                        .foregroundStyle([Color.red.opacity(0.9), .green.opacity(0.9), .blue.opacity(0.9)][row])
                                        .frame(width: 14, alignment: .leading)
                                    MixRow($recipe.mixer, row: row)
                                }
                            }
                        }
                        HStack {
                            Text("Monochrome").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                            Spacer()
                            Toggle("", isOn: Binding(
                                get: { recipe.mono != [0, 0, 0] },
                                set: { recipe.mono = $0 ? [0.21, 0.72, 0.07] : [0, 0, 0] }
                            ))
                            .labelsHidden().controlSize(.mini).tint(Ara.accent)
                        }
                        if recipe.mono != [0, 0, 0] {
                            TriRow("Mono", $recipe.mono, 0...1)
                        }
                    }
                    Panel("Retouch") {
                        SegPicker([("off", "Off"), ("heal", "Heal"), ("clone", "Clone"),
                                   ("dodge", "Dodge"), ("burn", "Burn")],
                                  selection: $retouchMode)
                        if retouchMode == "heal" {
                            SliderRow("Size", $spotSize, 0.01...0.15, reset: 0.05)
                        } else if retouchMode == "clone" {
                            SliderRow("Radius", $cloneRadius, 0.02...0.2, reset: 0.06)
                            if pendingClone != nil {
                                Text("Tap destination")
                                    .font(.system(size: 10, weight: .medium))
                                    .foregroundStyle(Ara.accent)
                            }
                        } else if retouchMode == "dodge" || retouchMode == "burn" {
                            SliderRow("Radius", $lightRadius, 0.05...0.6, reset: 0.25)
                            SliderRow("EV", $lightEV, 0...2, reset: 0.5)
                        }
                        if !recipe.spots.isEmpty || !recipe.lights.isEmpty || !recipe.clones.isEmpty {
                            ForEach(recipe.spots.indices, id: \.self) { i in
                                MarkRow("Spot \(i + 1)", icon: "bandage") { recipe.spots.remove(at: i) }
                            }
                            ForEach(recipe.clones.indices, id: \.self) { i in
                                MarkRow("Clone \(i + 1)", icon: "point.topleft.down.to.point.bottomright.curvepath") {
                                    recipe.clones.remove(at: i)
                                }
                            }
                            ForEach(recipe.lights.indices, id: \.self) { i in
                                MarkRow("Light \(i + 1)  \(recipe.lights[i][3] >= 0 ? "+" : "")\(String(format: "%.1f", recipe.lights[i][3]))EV",
                                        icon: "sun.max") {
                                    recipe.lights.remove(at: i)
                                }
                            }
                        }
                        Text("CROP")
                            .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                            .foregroundStyle(Ara.text3)
                            .padding(.top, 2)
                        SliderRow("Left", $recipe.crop[0], 0...0.45)
                        SliderRow("Top", $recipe.crop[1], 0...0.45)
                        SliderRow("Right", $recipe.crop[2], 0...0.45)
                        SliderRow("Bottom", $recipe.crop[3], 0...0.45)
                    }
                    Panel("Detail") {
                        SliderRow("Sharpen", $recipe.sharpen, 0...1)
                        SliderRow("Noise", $recipe.noise_luma, 0...1)
                        SliderRow("NR Chroma", $recipe.noise_chroma, 0...1)
                        SliderRow("Deband", $recipe.deband, 0...1)
                        SliderRow("CA Fix", $recipe.ca_fix, 0...1)
                        SliderRow("Beauty", $recipe.beauty, 0...1)
                    }
                    Panel("Effects", trailing: {
                        ToolChip(label: "Place", icon: "plus", active: retouchMode == "flare") {
                            retouchMode = retouchMode == "flare" ? "off" : "flare"
                        }
                    }) {
                        SliderRow("Clarity", $recipe.clarity, -1...1)
                        SliderRow("Vignette", $recipe.vignette, -1...1)
                        SliderRow("Grain", $recipe.grain, 0...1)
                        SliderRow("Glow", $recipe.glow, 0...1)
                        SliderRow("Flare", $recipe.flare[2], 0...1)
                        SliderRow("Fl Hue", $recipe.flare[3], 0...1)
                    }
                    Panel("Transform") {
                        SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
                    }
                    Color.clear.frame(height: 4)
                }
                .padding(10)
            }
            .scrollIndicators(.visible)

            actionBar
        }
        .background(Ara.bg1)
        .overlay(alignment: .leading) { Ara.hairline.frame(width: 1) }
    }

    private var actionBar: some View {
        VStack(spacing: 0) {
            Ara.hairline.frame(height: 1)
            HStack(spacing: 6) {
                if rendering {
                    ProgressView().controlSize(.small).tint(Ara.accent)
                        .frame(width: 14, height: 14)
                }
                Text(status.isEmpty ? (dirty ? "Unsaved changes" : photo.name) : status)
                    .font(.system(size: 10).monospacedDigit())
                    .foregroundStyle(dirty && status.isEmpty ? Ara.accent : Ara.text3)
                    .lineLimit(1).truncationMode(.middle)
                Spacer()
                IconAction(icon: "doc.on.doc") { copyRecipe() }
                IconAction(icon: "clipboard") { pasteRecipe() }
                IconAction(icon: "arrow.counterclockwise") { recipe = Recipe() }
                Button("Save") { save() }.buttonStyle(AraSecondaryButton())
                Button("Export") { export() }.buttonStyle(AraPrimaryButton())
            }
            .padding(.horizontal, 10).padding(.vertical, 8)
            .background(Ara.bg1)
        }
    }

    // MARK: data plumbing (unchanged logic)

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

    private func curveTint(_ ch: Int) -> Color {
        [Color.white, .red, .green, .blue, Ara.accent, Ara.accent, Ara.accent,
         Ara.accent, Ara.accent][ch]
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

/// horizontal filmstrip thumb used at the bottom of the stage
struct FilmCell: View {
    let photo: Photo
    let selected: Bool
    @State private var image: CGImage?

    var body: some View {
        ZStack(alignment: .topLeading) {
            Ara.bg3
            if let image {
                Image(image, scale: 1, label: Text(photo.name))
                    .resizable().scaledToFill()
            }
            if let c = labelColors.first(where: { $0.name == photo.label })?.color {
                Circle().fill(c).frame(width: 6, height: 6).padding(4)
            }
        }
        .frame(width: 84, height: 56)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5)
            .stroke(selected ? Ara.accent : Ara.hairline, lineWidth: selected ? 2 : 1))
        .opacity(selected ? 1 : 0.75)
        .task {
            let img = await AraEngine.shared.work { $0.thumbnail(path: photo.path, maxPx: 160) }
            await MainActor.run { image = img }
        }
    }
}

/// Look preset menu (chip-style).
struct LookPicker: View {
    @Binding var look: String
    private let options: [(String, String)] = [
        ("", "None"), ("teal_orange", "Teal & Orange"), ("film_fade", "Film Fade"),
        ("bleach", "Bleach Bypass"), ("noir", "Noir"), ("matte", "Matte"),
    ]
    var body: some View {
        Menu {
            ForEach(options, id: \.0) { v, name in
                Button(name) { look = v }
            }
        } label: {
            HStack(spacing: 4) {
                Text(options.first(where: { $0.0 == look })?.1 ?? "Look")
                    .font(.system(size: 9.5, weight: .medium))
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 7))
            }
            .foregroundStyle(look.isEmpty ? Ara.text2 : Ara.accent)
            .padding(.horizontal, 8).padding(.vertical, 3)
            .background(Capsule().fill(look.isEmpty ? Ara.bg3 : Ara.accentSoft)
                .overlay(Capsule().stroke(look.isEmpty ? Ara.hairline : Ara.accent.opacity(0.4), lineWidth: 0.5)))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }
}

/// DaVinci-style color wheel: drag sets chroma offset; the luma slider under
/// the wheel sets the mean component. Double-click resets chroma to neutral.
struct ColorWheel: View {
    let title: String
    @Binding var v: [Double]
    /// neutral mean: 0 for additive wheels, 1 for multiplicative
    let center: Double

    private var mean: Double { (v[0] + v[1] + v[2]) / 3 }
    private var chroma: (u: Double, w: Double) {
        let m = mean
        let d = (v[0] - m, v[1] - m, v[2] - m)
        return (d.0 - (d.1 + d.2) / 2, (d.1 - d.2) * 0.8660)
    }

    var body: some View {
        VStack(spacing: 3) {
            GeometryReader { geo in
                let s = min(geo.size.width, geo.size.height)
                let cx = geo.size.width / 2, cy = geo.size.height / 2
                let r = s / 2 - 3
                ZStack {
                    Circle().fill(
                        AngularGradient(colors: [
                            .red, .yellow, .green, .cyan, .blue,
                            Color(red: 1, green: 0, blue: 1), .red,
                        ], center: .center)
                    )
                    .opacity(0.55)
                    Circle().fill(
                        RadialGradient(colors: [Ara.bg2.opacity(0.85), .clear],
                                       center: .center, startRadius: 0, endRadius: r * 0.75)
                    )
                    Circle().stroke(Ara.border, lineWidth: 0.5)
                    // crosshair
                    Path { p in
                        p.move(to: .init(x: cx - r, y: cy)); p.addLine(to: .init(x: cx + r, y: cy))
                        p.move(to: .init(x: cx, y: cy - r)); p.addLine(to: .init(x: cx, y: cy + r))
                    }
                    .stroke(Ara.text3.opacity(0.4), lineWidth: 0.5)
                    let c = chroma
                    let bx = cx + CGFloat(c.u) * r * 4
                    let by = cy - CGFloat(c.w) * r * 4
                    Circle()
                        .fill(.white)
                        .frame(width: 9, height: 9)
                        .overlay(Circle().stroke(.black.opacity(0.65), lineWidth: 1))
                        .shadow(color: .black.opacity(0.6), radius: 1.5)
                        .position(x: bx.clamped(to: cx - r...cx + r),
                                  y: by.clamped(to: cy - r...cy + r))
                }
                .frame(width: s, height: s)
                .position(x: cx, y: cy)
                .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                    let dx = Double(g.location.x - cx) / Double(r)
                    let dy = Double(cy - g.location.y) / Double(r)
                    let m = min((dx * dx + dy * dy).squareRoot(), 1.0) / 4
                    let a = atan2(dy, dx)
                    let h = a / (2 * .pi)
                    let rgb = hueRgb(h)
                    let cm = (rgb.0 + rgb.1 + rgb.2) / 3
                    v = [
                        mean + m * (rgb.0 - cm) * 1.7,
                        mean + m * (rgb.1 - cm) * 1.7,
                        mean + m * (rgb.2 - cm) * 1.7,
                    ]
                })
                .onTapGesture(count: 2) { _ in
                    v = [mean, mean, mean]
                }
            }
            .aspectRatio(1, contentMode: .fit)
            TrackSlider(value: Binding(
                get: { mean },
                set: { m in
                    let d = (v[0] - mean, v[1] - mean, v[2] - mean)
                    v = [m + d.0, m + d.1, m + d.2]
                }),
                range: center == 0 ? -0.25...0.25 : 0.4...1.6,
                step: 0.01, reset: center, height: 12)
            Text(title.uppercased())
                .font(.system(size: 8, weight: .semibold)).tracking(1)
                .foregroundStyle(Ara.text3)
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
            HStack(spacing: 7) {
                Text(title.uppercased())
                    .font(.system(size: 9.5, weight: .semibold)).tracking(0.8)
                    .foregroundStyle(Ara.text2)
                    .frame(width: 52, alignment: .leading)
                TrackSlider(value: $z[2], range: -1...1)
                Text(String(format: "%+.2f", z[2]))
                    .font(.system(size: 10).monospacedDigit())
                    .foregroundStyle(z[2] == 0 ? Ara.text3 : Ara.accent)
                    .frame(width: 38, alignment: .trailing)
            }
            HStack(spacing: 5) {
                Text("hue")
                    .font(.system(size: 8.5)).foregroundStyle(Ara.text3)
                    .frame(width: 52, alignment: .leading)
                TrackSlider(value: $z[0], range: 0...1, height: 12)
                TrackSlider(value: $z[1], range: 0...1, height: 12)
                TrackSlider(value: $z[3], range: -1...1, height: 12)
            }
        }
    }
}

/// curve editor: click to add, drag to move, double-click a point to delete
struct CurveEditor: View {
    @Binding var points: [[Double]]
    var tint: Color = .white

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width, h = geo.size.height
            ZStack {
                Rectangle().fill(Color(red: 0.05, green: 0.05, blue: 0.065))
                Canvas { ctx, size in
                    // grid
                    for i in 1..<4 {
                        let f = CGFloat(i) / 4
                        var gp = Path()
                        gp.move(to: .init(x: f * size.width, y: 0))
                        gp.addLine(to: .init(x: f * size.width, y: size.height))
                        gp.move(to: .init(x: 0, y: f * size.height))
                        gp.addLine(to: .init(x: size.width, y: f * size.height))
                        ctx.stroke(gp, with: .color(.white.opacity(0.07)), lineWidth: 0.5)
                    }
                    // diagonal reference (dashed)
                    var dp = Path()
                    dp.move(to: .init(x: 0, y: size.height))
                    dp.addLine(to: .init(x: size.width, y: 0))
                    ctx.stroke(dp, with: .color(.white.opacity(0.12)),
                               style: StrokeStyle(lineWidth: 0.5, dash: [3, 3]))
                    let pts = sorted()
                    if pts.count > 1 {
                        var p = Path()
                        var fill = Path()
                        for i in 0...64 {
                            let x = Double(i) / 64
                            let y = eval(pts, x)
                            let px = x * size.width
                            let py = (1 - y) * size.height
                            if i == 0 {
                                p.move(to: .init(x: px, y: py))
                                fill.move(to: .init(x: px, y: py))
                            } else {
                                p.addLine(to: .init(x: px, y: py))
                                fill.addLine(to: .init(x: px, y: py))
                            }
                        }
                        fill.addLine(to: .init(x: size.width, y: size.height))
                        fill.addLine(to: .init(x: 0, y: size.height))
                        fill.closeSubpath()
                        ctx.fill(fill, with: .color(tint.opacity(0.10)))
                        ctx.stroke(p, with: .color(tint), lineWidth: 1.5)
                    }
                    for pt in pts {
                        let px = pt[0] * size.width
                        let py = (1 - pt[1]) * size.height
                        let dot = Circle().path(in: CGRect(x: px - 4, y: py - 4, width: 8, height: 8))
                        ctx.fill(dot, with: .color(.white))
                        let ring = Circle().path(in: CGRect(x: px - 4.5, y: py - 4.5, width: 9, height: 9))
                        ctx.stroke(ring, with: .color(tint), lineWidth: 1.5)
                    }
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(Ara.hairline, lineWidth: 1))
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

    /// approx CIE 1931 spectral locus (x, y) from violet through green to red,
    /// closed by the purple line — normalised 0..1 coords
    private let locus: [(Double, Double)] = [
        (0.1741, 0.0050), (0.1733, 0.0048), (0.1703, 0.0058), (0.1566, 0.0177),
        (0.1440, 0.0297), (0.1247, 0.0578), (0.0913, 0.1327), (0.0454, 0.2950),
        (0.0082, 0.5384), (0.0039, 0.6548), (0.0139, 0.7502), (0.0743, 0.8338),
        (0.1147, 0.8262), (0.1547, 0.8059), (0.1929, 0.7816), (0.2296, 0.7543),
        (0.2658, 0.7243), (0.3016, 0.6923), (0.3373, 0.6589), (0.3731, 0.6245),
        (0.4087, 0.5896), (0.4441, 0.5547), (0.4788, 0.5202), (0.5125, 0.4866),
        (0.5448, 0.4544), (0.5752, 0.4242), (0.6029, 0.3965), (0.6270, 0.3725),
        (0.6482, 0.3514), (0.6658, 0.3340), (0.6801, 0.3197), (0.6915, 0.3083),
        (0.7006, 0.2993), (0.7079, 0.2920), (0.7132, 0.2868), (0.7173, 0.2827),
        (0.7190, 0.2809), (0.7206, 0.2794), (0.7260, 0.2740), (0.7300, 0.2700),
        (0.7311, 0.2689), (0.7320, 0.2680), (0.7327, 0.2673), (0.7334, 0.2666),
        (0.7340, 0.2660), (0.7344, 0.2656), (0.7347, 0.2653),
    ]

    var body: some View {
        Canvas { ctx, size in
            switch kind {
            case "parade":
                // channel separators + level guides
                for c in 0..<2 {
                    var sep = Path()
                    let x = size.width * CGFloat(c + 1) / 3
                    sep.move(to: .init(x: x, y: 0)); sep.addLine(to: .init(x: x, y: size.height))
                    ctx.stroke(sep, with: .color(.white.opacity(0.10)), lineWidth: 0.5)
                }
                for f in [0.25, 0.5, 0.75] {
                    var gl = Path()
                    let y = size.height * CGFloat(f)
                    gl.move(to: .init(x: 0, y: y)); gl.addLine(to: .init(x: size.width, y: y))
                    ctx.stroke(gl, with: .color(.white.opacity(0.08)), lineWidth: 0.5)
                }
                if wave.count >= 196608 {
                    let cols: [Color] = [.red, .green, .blue]
                    for c in 0..<3 {
                        let maxv = Double(wave[(c * 65536)..<(c * 65536 + 65536)].max() ?? 1).squareRoot()
                        for v in 0..<256 {
                            for x in 0..<256 {
                                let n = wave[c * 65536 + v * 256 + x]
                                if n == 0 { continue }
                                let a = min(Double(n).squareRoot() / maxv, 1)
                                let px = (CGFloat(c * 256 + x) / 768) * size.width
                                let py = size.height - CGFloat(v) / 256 * size.height
                                ctx.fill(
                                    Path(CGRect(x: px, y: py, width: size.width / 256 + 0.6, height: size.height / 256 + 0.6)),
                                    with: .color(cols[c].opacity(a)))
                            }
                        }
                    }
                }
            case "vector":
                let c = CGPoint(x: size.width / 2, y: size.height / 2)
                // graticule: outer + inner circles, cross axes, skin-tone line
                for f in [0.7, 0.35] {
                    let d = size.width * f
                    ctx.stroke(Circle().path(in: CGRect(
                        x: c.x - d / 2, y: c.y - d / 2, width: d, height: d)),
                        with: .color(.white.opacity(0.12)), lineWidth: 0.5)
                }
                var ax = Path()
                ax.move(to: .init(x: c.x - size.width * 0.35, y: c.y))
                ax.addLine(to: .init(x: c.x + size.width * 0.35, y: c.y))
                ax.move(to: .init(x: c.x, y: c.y - size.height * 0.35))
                ax.addLine(to: .init(x: c.x, y: c.y + size.height * 0.35))
                ctx.stroke(ax, with: .color(.white.opacity(0.10)), lineWidth: 0.5)
                // skin tone line: Cr+, Cb- → upper-left
                var sk = Path()
                sk.move(to: c)
                sk.addLine(to: .init(x: c.x - size.width * 0.16, y: c.y - size.height * 0.30))
                ctx.stroke(sk, with: .color(.orange.opacity(0.6)),
                           style: StrokeStyle(lineWidth: 1, dash: [3, 2]))
                if vec.count >= 65536 {
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
                // CIE xy: spectral locus outline + D65 marker
                var lp = Path()
                for (i, p) in locus.enumerated() {
                    let px = p.0 * size.width
                    let py = (1 - p.1) * size.height
                    if i == 0 { lp.move(to: .init(x: px, y: py)) }
                    else { lp.addLine(to: .init(x: px, y: py)) }
                }
                lp.closeSubpath()
                ctx.stroke(lp, with: .color(.white.opacity(0.35)), lineWidth: 0.75)
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
                // D65 white point
                let d65 = CGRect(x: 0.3127 * size.width - 3, y: (1 - 0.3290) * size.height - 3,
                                 width: 6, height: 6)
                ctx.stroke(Circle().path(in: d65), with: .color(.white.opacity(0.6)), lineWidth: 0.75)
            }
        }
        .background(Color(red: 0.045, green: 0.045, blue: 0.055))
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Ara.hairline, lineWidth: 1))
    }
}

/// R,G,B + luma overlay histogram (log scale, filled areas).
struct HistogramView: View {
    let hist: [[UInt32]]

    var body: some View {
        Canvas { ctx, size in
            // 25% guides
            for f in [0.25, 0.5, 0.75] {
                var gl = Path()
                let y = size.height * CGFloat(f)
                gl.move(to: .init(x: 0, y: y)); gl.addLine(to: .init(x: size.width, y: y))
                ctx.stroke(gl, with: .color(.white.opacity(0.07)), lineWidth: 0.5)
            }
            let chans: [(Int, Color)] = [
                (0, .red), (1, .green), (2, .blue), (3, .white),
            ]
            let maxv = hist.flatMap { $0 }.map { log1p(Double($0)) }.max() ?? 1
            for (ch, color) in chans where hist.count > ch {
                var p = Path()
                var fill = Path()
                for x in 0..<256 {
                    let v = log1p(Double(hist[ch][x])) / maxv
                    let px = CGFloat(x) / 255 * size.width
                    let py = size.height - CGFloat(v) * size.height
                    if x == 0 {
                        p.move(to: CGPoint(x: px, y: py))
                        fill.move(to: CGPoint(x: px, y: py))
                    } else {
                        p.addLine(to: CGPoint(x: px, y: py))
                        fill.addLine(to: CGPoint(x: px, y: py))
                    }
                }
                fill.addLine(to: .init(x: size.width, y: size.height))
                fill.addLine(to: .init(x: 0, y: size.height))
                fill.closeSubpath()
                ctx.fill(fill, with: .color(color.opacity(ch == 3 ? 0.22 : 0.18)))
                ctx.stroke(p, with: .color(color.opacity(ch == 3 ? 0.8 : 0.6)), lineWidth: 1)
            }
        }
        .background(Color(red: 0.05, green: 0.05, blue: 0.06))
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Ara.hairline, lineWidth: 1))
    }
}

struct LabelPicker: View {
    @Binding var label: String

    var body: some View {
        HStack(spacing: 5) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 11, height: 11)
                    .overlay(Circle().stroke(.white.opacity(0.9), lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.4)
                    .onTapGesture { label = (label == l.name) ? "" : l.name }
            }
            if !label.isEmpty {
                Button { label = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 10))
                        .foregroundStyle(Ara.text3)
                }
                .buttonStyle(.plain)
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
    let icon: String
    let onDelete: () -> Void
    init(_ title: String, icon: String = "circle.fill", onDelete: @escaping () -> Void) {
        self.title = title
        self.icon = icon
        self.onDelete = onDelete
    }
    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: icon).font(.system(size: 9)).foregroundStyle(Ara.text3)
            Text(title).font(.system(size: 10.5)).foregroundStyle(Ara.text1)
            Spacer()
            Button { onDelete() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Ara.text3)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 5).fill(Ara.bg3))
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
            HStack(spacing: 5) {
                Text(title)
                    .font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                    .frame(width: 34, alignment: .leading)
                ForEach(0..<3, id: \.self) { i in
                    TrackSlider(value: $v[i], range: range, reset: 0.5)
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
            HStack(spacing: 5) {
                ForEach(0..<3, id: \.self) { c in
                    TrackSlider(value: $m[row * 3 + c], range: -1...2,
                                reset: c == row ? 1.0 : 0.0)
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
        VStack(spacing: 5) {
            HStack {
                Image(systemName: w.kind == "gradient" ? "rectangle.lefthalf.filled" : "circle")
                    .font(.system(size: 9))
                    .foregroundStyle(.cyan)
                Text(w.kind == "gradient" ? "Gradient" : "Circle")
                    .font(.system(size: 10.5, weight: .medium)).foregroundStyle(Ara.text1)
                Spacer()
                Text("Inv").font(.system(size: 9.5)).foregroundStyle(Ara.text2)
                Toggle("", isOn: $w.invert).labelsHidden().controlSize(.mini).tint(Ara.accent)
                Button { onDelete() } label: {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Ara.text3)
                }
                .buttonStyle(.plain)
            }
            SliderRow("EV", $w.ev, -2...2)
            SliderRow("Sat", $w.sat, -1...1)
            SliderRow("Temp", $w.temp, -1...1)
            if w.kind == "circle" {
                SliderRow("Size", $w.p[2], 0.02...0.6, reset: 0.15)
                SliderRow("Ratio", $w.p[3], 0.02...0.6, reset: 0.15)
                SliderRow("Rot", $w.p[4], -90...90)
            }
            SliderRow("Soft", $w.p[w.kind == "circle" ? 5 : 4], 0.02...1, reset: 0.4)
        }
        .padding(8)
        .background(Ara.bg3)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Ara.hairline, lineWidth: 1))
    }
}

struct Stars: View {
    @Binding var rating: Int
    var body: some View {
        HStack(spacing: 3) {
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .font(.system(size: 12))
                    .foregroundStyle(i <= rating ? Ara.gold : Ara.text3)
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
        }
    }
}
