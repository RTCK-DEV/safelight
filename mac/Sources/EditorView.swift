import SwiftUI
import ImageIO
import UniformTypeIdentifiers

struct EditorView: View {
    let photo: Photo

    @State private var recipe = Recipe()
    @State private var rating = 0
    @State private var label = ""
    @State private var image: CGImage?
    @State private var rendering = false
    @State private var dirty = false
    @State private var status = ""
    @State private var renderTask: Task<Void, Never>?

    var body: some View {
        HSplitView {
            ZStack {
                Color(white: 0.12)
                if let image {
                    Image(image, scale: 1, label: Text(photo.name))
                        .resizable()
                        .scaledToFit()
                        .padding(8)
                } else {
                    ProgressView()
                }
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

                    GroupBox("Light") {
                        VStack(spacing: 8) {
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
                    GroupBox("Detail") {
                        VStack(spacing: 8) {
                            SliderRow("Sharpen", $recipe.sharpen, 0...1)
                            SliderRow("Noise", $recipe.noise_luma, 0...1)
                        }
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
            let img = await AraEngine.shared.work { $0.render(path: path, recipe: r, maxPx: 1800) }
            await MainActor.run {
                if let img { image = img }
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
