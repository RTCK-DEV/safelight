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
    @State private var rendering = false
    @State private var dirty = false
    @State private var status = ""
    @State private var renderTask: Task<Void, Never>?
    @State private var retouchMode = "off" // off|heal|dodge|burn
    @State private var spotSize = 0.05
    @State private var lightRadius = 0.25
    @State private var lightEV = 0.5

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

    private func placeAt(_ loc: CGPoint, in size: CGSize) {
        guard retouchMode != "off" else { return }
        let rect = imageRect(in: size)
        guard rect.width > 0 else { return }
        let nx = ((loc.x - rect.minX) / rect.width).clamped(to: 0...1)
        let ny = ((loc.y - rect.minY) / rect.height).clamped(to: 0...1)
        switch retouchMode {
        case "heal":
            // spots are frame-normalized (pre-crop) coords
            let cl = recipe.crop[0], ct = recipe.crop[1]
            let sw = 1 - recipe.crop[0] - recipe.crop[2]
            let sh = 1 - recipe.crop[1] - recipe.crop[3]
            recipe.spots.append([cl + nx * sw, ct + ny * sh, spotSize, 0])
        case "dodge", "burn":
            recipe.lights.append([nx, ny, lightRadius, retouchMode == "dodge" ? lightEV : -lightEV])
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
                    }

                    if !hist.isEmpty {
                        HistogramView(hist: hist)
                            .frame(height: 88)
                            .padding(.vertical, 2)
                    }

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
                            SliderRow("Highlights", $recipe.highlights, -1...1)
                            SliderRow("Shadows", $recipe.shadows, -1...1)
                            SliderRow("Whites", $recipe.whites, -1...1)
                            SliderRow("Blacks", $recipe.blacks, -1...1)
                        }
                    }
                    GroupBox("Color") {
                        VStack(spacing: 8) {
                            Picker("WB", selection: $recipe.wb_mode) {
                                Text("As shot").tag(WbMode.asShot)
                                Text("Auto").tag(WbMode.auto)
                                Text("Manual").tag(WbMode.manual)
                            }
                            .pickerStyle(.segmented)
                            SliderRow("Temp", $recipe.temperature, -1...1)
                            SliderRow("Tint", $recipe.tint, -1...1)
                            SliderRow("Saturation", $recipe.saturation, -1...1)
                            SliderRow("Vibrance", $recipe.vibrance, -1...1)
                        }
                    }
                    GroupBox("Grade") {
                        VStack(spacing: 8) {
                            Picker("Look", selection: $recipe.look) {
                                Text("None").tag("")
                                Text("Teal & Orange").tag("teal_orange")
                                Text("Film Fade").tag("film_fade")
                                Text("Bleach Bypass").tag("bleach")
                                Text("Noir").tag("noir")
                                Text("Matte").tag("matte")
                            }
                            Text("Lift / Gamma / Gain (R G B)").font(.caption2).foregroundStyle(.secondary)
                            TriRow("Lift", $recipe.lift, -0.25...0.25)
                            TriRow("Gamma", $recipe.gamma, 0.5...2)
                            TriRow("Gain", $recipe.gain, 0.5...2)
                            Text("Split Tone").font(.caption2).foregroundStyle(.secondary)
                            SliderRow("Shd Hue", $recipe.shadow_hue, 0...1)
                            SliderRow("Shd Sat", $recipe.shadow_sat, 0...1)
                            SliderRow("Hi Hue", $recipe.highlight_hue, 0...1)
                            SliderRow("Hi Sat", $recipe.highlight_sat, 0...1)
                        }
                    }
                    GroupBox("Retouch") {
                        VStack(spacing: 8) {
                            Picker("Tool", selection: $retouchMode) {
                                Text("Off").tag("off")
                                Text("Heal").tag("heal")
                                Text("Dodge").tag("dodge")
                                Text("Burn").tag("burn")
                            }
                            .pickerStyle(.segmented)
                            if retouchMode == "heal" {
                                SliderRow("Size", $spotSize, 0.01...0.15)
                            } else if retouchMode != "off" {
                                SliderRow("Radius", $lightRadius, 0.05...0.6)
                                SliderRow("EV", $lightEV, 0...2)
                            }
                            if !recipe.spots.isEmpty || !recipe.lights.isEmpty {
                                ForEach(recipe.spots.indices, id: \.self) { i in
                                    MarkRow("Spot \(i + 1)") { recipe.spots.remove(at: i) }
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
                        }
                    }
                    GroupBox("Transform") {
                        VStack(spacing: 8) {
                            SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
                        }
                    }
                    GroupBox("Effects") {
                        VStack(spacing: 8) {
                            SliderRow("Clarity", $recipe.clarity, -1...1)
                            SliderRow("Vignette", $recipe.vignette, -1...1)
                            SliderRow("Grain", $recipe.grain, 0...1)
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
        let r = recipe
        Task.detached { [path = photo.path] in
            let (img, h) = await AraEngine.shared.work { $0.render(path: path, recipe: r, maxPx: 1800) }
            await MainActor.run {
                if let img { image = img }
                if !h.isEmpty { hist = h }
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
    /// marker overlay for heal spots / dodge-burn lights
    @ViewBuilder
    func retouchMarkers(in rect: CGRect) -> some View {
        let cl = recipe.crop[0], ct = recipe.crop[1]
        let sw = (1 - recipe.crop[0] - recipe.crop[2])
        let sh = (1 - recipe.crop[1] - recipe.crop[3])
        Canvas { ctx, _ in
        for s in recipe.spots {
            let nx = (s[0] - cl) / max(sw, 0.01)
            let ny = (s[1] - ct) / max(sh, 0.01)
            let cx = rect.minX + nx * rect.width
            let cy = rect.minY + ny * rect.height
            let r = s[2] * rect.width
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            ctx.stroke(path, with: .color(.red.opacity(0.9)), lineWidth: 1.5)
        }
        for l in recipe.lights {
            let cx = rect.minX + l[0] * rect.width
            let cy = rect.minY + l[1] * rect.height
            let r = l[2] * rect.width
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            let col: Color = l[3] >= 0 ? .yellow : .purple
            ctx.stroke(path, with: .color(col.opacity(0.9)), lineWidth: 1.5)
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
