import SwiftUI
import AppKit
import ImageIO
import UniformTypeIdentifiers

let labelColors: [(name: String, color: Color)] = [
    ("red", .red), ("orange", .orange), ("yellow", .yellow),
    ("green", .green), ("blue", .blue), ("purple", .purple),
]

enum CompareMode: String, CaseIterable, Identifiable {
    case off, before, wipeV, wipeH, diff, mix
    var id: String { rawValue }
    var label: String {
        switch self {
        case .off: return "Off"
        case .before: return "Before"
        case .wipeV: return "Wipe Vertical"
        case .wipeH: return "Wipe Horizontal"
        case .diff: return "Difference"
        case .mix: return "50/50 Mix"
        }
    }
}

/// Inspector palettes, DaVinci color-page style: one at a time via the icon strip.
enum Palette: String, CaseIterable, Identifiable {
    case quick, meters, light, wheels, curves, zones, slice, warp, qualifier, windows, mixer,
         retouch, detail, fx, xform, versions
    var id: String { rawValue }
    var icon: String {
        switch self {
        case .quick: return "speedometer"
        case .meters: return "waveform.path.ecg"
        case .light: return "sun.max"
        case .wheels: return "circle.circle"
        case .curves: return "chart.line.uptrend.xyaxis"
        case .zones: return "square.split.bottomhalf.filled"
        case .slice: return "circle.hexagongrid"
        case .warp: return "point.3.connected.trianglepath.dotted"
        case .qualifier: return "eyedropper.halffull"
        case .windows: return "circle.dashed"
        case .mixer: return "slider.horizontal.below.square.filled.and.square"
        case .retouch: return "bandage"
        case .detail: return "sparkle.magnifyingglass"
        case .fx: return "sparkles"
        case .xform: return "crop.rotate"
        case .versions: return "square.stack"
        }
    }
    var title: String {
        switch self {
        case .quick: return "Quick"
        case .meters: return "Meters"
        case .light: return "Light"
        case .wheels: return "Wheels"
        case .curves: return "Curves"
        case .zones: return "HDR Zones"
        case .slice: return "ColorSlice"
        case .warp: return "Warper"
        case .qualifier: return "Qualifier"
        case .windows: return "Windows"
        case .mixer: return "Mixer"
        case .retouch: return "Retouch"
        case .detail: return "Detail"
        case .fx: return "Effects"
        case .xform: return "Transform"
        case .versions: return "Versions"
        }
    }
    /// darktable contextual hint: one line shown under the palette title.
    var hint: String {
        switch self {
        case .quick: return "Everyday controls in one place"
        case .meters: return "Drag vertically on the parade to set exposure"
        case .light: return "White balance, tonal range and colour"
        case .wheels: return "Drag inside a wheel to tint — the slider sets the mean"
        case .curves: return "Click to add a point, drag to move, double-click deletes"
        case .zones: return "Enable Image mode, then scroll on the photo to move the band under the cursor"
        case .slice: return "Density, saturation and hue per colour wedge"
        case .warp: return "Click the graph to pin a colour, drag to displace it — double-click deletes"
        case .qualifier: return "Key a colour range with the droppers, adjust inside it"
        case .windows: return "Tap or drag on the photo to place local masks"
        case .mixer: return "Channel mixing and monochrome conversion"
        case .retouch: return "Tap the photo with a tool armed — Esc backs out"
        case .detail: return "Sharpening, noise reduction and restoration"
        case .fx: return "Clarity, vignette, grain and light effects"
        case .xform: return "Rotation, keystone and crop"
        case .versions: return "Named snapshots and the edit history stack"
        }
    }
}

struct EditorView: View {
    let photo: Photo
    @EnvironmentObject var store: LibraryStore

    @State private var recipe = Recipe()
    @State private var rating = 0
    @State private var label = ""
    @State private var flag = 0
    // sidecar values as loaded — guards the initial-bind onChange from
    // rewriting the sidecar file on every photo open
    @State private var loadedRating = -1
    @State private var loadedLabel = "\u{0}"
    @State private var image: CGImage?
    @State private var hist: [[UInt32]] = []
    @State private var wave: [UInt32] = []
    @State private var vec: [UInt32] = []
    @State private var cie: [UInt32] = []
    @State private var rendering = false
    /// monotonically increasing render id — stale detached results are dropped
    @State private var renderGen = 0
    @State private var dirty = false
    @State private var baseline = Recipe()
    @State private var status = ""
    @State private var renderTask: Task<Void, Never>?
    // tool: off|heal|dodge|burn|wbpick|clone|window|grad|flare|qpick|qadd|qsub
    @State private var retouchMode = "off"
    // adjustment brush
    @State private var brushRadius: Double = 0.04
    @State private var brushSoft: Double = 0.5
    @State private var brushFlow: Double = 1.0
    @State private var brushErase = false
    @State private var selBrush = 0              // active brush layer index
    @State private var liveFrame: [[Double]] = [] // in-progress stroke (frame-norm)
    @State private var spotSize = 0.05
    @State private var lightRadius = 0.25
    @State private var lightEV = 0.5
    @State private var cloneRadius = 0.06
    @State private var pendingClone: [Double]? = nil
    @State private var cmp: CompareMode = .off
    @State private var wipePos: Double = 0.5
    @State private var baselineImg: CGImage?
    @State private var scopeKind = "parade"
    @State private var curveChan = 0
    // viewer
    @State private var zoom: CGFloat = 1
    @State private var pan = CGSize.zero
    @State private var stageHover = false
    // undo / redo (recipe snapshots, bursts coalesce at 0.8s)
    @State private var undoStack: [Recipe] = []
    @State private var redoStack: [Recipe] = []
    @State private var lastEditTime = Date.distantPast
    @State private var applyingHistory = false
    /// lensfun profile name matched to this file's EXIF lens ("" = none)
    @State private var lensProfile = ""
    // palettes
    @State private var palette: Palette = .quick
    @State private var selWindow: UUID?
    // grade library (DaVinci Gallery stills) + wipe reference recipe
    @State private var stills: [GradeStill] = []
    @State private var stillThumbs: [UUID: NSImage] = [:]
    @State private var refRecipe: Recipe?
    @State private var refName = ""
    // serial correction stages (DaVinci serial nodes): 0 = base recipe,
    // 1..4 = recipe.stages[selStage-1].params — every colour-domain palette
    // binds through `edit` so it edits the selected node.
    @State private var selStage = 0
    @State private var showStageRename = false
    @State private var stageRenameText = ""
    private var edit: Binding<Recipe> {
        if selStage > 0, selStage <= recipe.stages.count {
            return $recipe.stages[selStage - 1].params
        }
        return $recipe
    }
    // grade versions
    @State private var versions: [GradeVersion] = []
    @State private var baselineVersions: [GradeVersion] = []
    @State private var showVersionName = false
    @State private var versionName = ""
    // cursor probe / status bar (RawTherapee-style readout)
    @State private var probeText = ""
    @State private var probeBuf: [UInt8] = []
    @State private var probeW = 0
    @State private var probeH = 0
    @State private var lastHover: CGPoint = .zero
    @State private var stageSize: CGSize = .zero
    // darktable tone-equalizer image interaction: scroll on the photo to
    // adjust the EV band under the cursor
    @State private var zoneImgMode = false
    @State private var zoneHover: Int? = nil
    // 360° viewer (PanoView): equirect pano — Insta360 .insp, DJI Osmo 360
    // and other GPano 2:1 stills
    @State private var panoMode = false
    /// equirect-candidate: .insp by extension, or a 2:1 developed frame
    private var panoLike: Bool {
        if photo.name.lowercased().hasSuffix(".insp") { return true }
        guard let image else { return false }
        let a = Double(image.width) / Double(image.height)
        return a > 1.985 && a < 2.015
    }
    // scope-as-control drag state
    @State private var scopeDragBase: Double? = nil
    @State private var scopeTapTime = Date.distantPast
    // collapsible chrome (darktable panel-edge arrows)
    @State private var showInspector = true
    @State private var showStrip = true
    @State private var autoBusy = false
    @State private var aidnBusy = false
    @State private var aidnReady = false
    @State private var aidnInfo = ""
    @State private var subjBusy = false
    @State private var subjReady = false
    @State private var showExportSheet = false
    @State private var expFormat = "JPEG"
    @State private var expQuality: Double = 0.92
    @State private var expResize = false
    @State private var expLongEdge: Double = 2048
    @State private var expSharpen: Double = 0.25
    // output colour space for export — engine emits sRGB; convert+tag via
    // ColorSync into the chosen ICC profile
    @State private var expSpace = "sRGB"

    /// letterboxed image rect inside the preview area (zoom/pan applied)
    private func imageRect(in size: CGSize) -> CGRect {
        guard let image else { return .zero }
        let iw = CGFloat(image.width), ih = CGFloat(image.height)
        let pad: CGFloat = 10
        let avail = CGSize(width: size.width - pad * 2, height: size.height - pad * 2)
        let sc = min(avail.width / iw, avail.height / ih) * zoom
        let w = iw * sc, h = ih * sc
        return CGRect(x: (size.width - w) / 2 + pan.width,
                      y: (size.height - h) / 2 + pan.height, width: w, height: h)
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
            edit.wrappedValue.lights.append([nx, ny, lightRadius, retouchMode == "dodge" ? lightEV : -lightEV])
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
            edit.wrappedValue.windows.append(w)
            selWindow = w.id
        case "grad":
            var w = PowerWindow()
            w.kind = "gradient"
            w.p = [0.0, fy, 1.0, fy, 0, 0.5]
            edit.wrappedValue.windows.append(w)
            selWindow = w.id
        case "brush":
            // tap = single dab (zero-length segment resolves to a disc)
            if edit.wrappedValue.brushes.isEmpty { edit.wrappedValue.brushes.append(BrushLayer()) }
            if selBrush >= edit.wrappedValue.brushes.count { selBrush = edit.wrappedValue.brushes.count - 1 }
            var st = BrushStroke()
            st.pts = [[fx, fy], [fx, fy]]
            st.radius = brushRadius
            st.soft = brushSoft
            st.opacity = brushFlow
            st.erase = brushErase
            edit.wrappedValue.brushes[selBrush].strokes.append(st)
        case "flare":
            recipe.flare[0] = nx
            recipe.flare[1] = ny
        case "qpick", "qadd", "qsub":
            if let c = samplePixel(nx: nx, ny: ny) {
                qualifierPick(c, mode: retouchMode)
            }
        default: break
        }
    }

    var body: some View {
        HSplitView {
            stageColumn
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            if showInspector {
                inspector
                    .frame(minWidth: 300, idealWidth: 316, maxWidth: 380)
            }
        }
        .background(Ara.bg0)
        .background(shortcutLayer)
        .sheet(isPresented: $showExportSheet) { exportSheet }
        .task { load() }
        .onChange(of: recipe) { old, new in
            dirty = (new != baseline) || versions != baselineVersions
            store.unsavedEdits[photo.id] = dirty ? new : nil
            if applyingHistory {
                // programmatic recipe assignment (undo/redo/version apply)
                applyingHistory = false
            } else {
                recordUndo(old)
            }
            scheduleRender()
        }
        .onChange(of: versions) { _, _ in
            dirty = (recipe != baseline) || versions != baselineVersions
            store.unsavedVersions[photo.id] = (versions != baselineVersions) ? versions : nil
        }
        .onChange(of: cmp) { _, m in
            if m != .off { ensureBaseline() }
        }
        // Thumbnail hover-stars (and any other writer through
        // store.setRating) keep the open editor's rating mirror in sync —
        // otherwise a later save() would serialize the stale value back.
        .onChange(of: store.photos) { _, ps in
            if let p = ps.first(where: { $0.id == photo.id }) {
                if p.rating != rating { rating = p.rating; loadedRating = p.rating }
                if p.flag != flag { flag = p.flag }
                if p.label != label { label = p.label; loadedLabel = p.label }
            }
        }
        .alert("Save Version", isPresented: $showVersionName) {
            TextField("Name", text: $versionName)
            Button("Save") { commitVersion() }
            Button("Cancel", role: .cancel) { versionName = "" }
        }
    }

    /// Global-ish keys via invisible command buttons (Cmd-modified keys only —
    /// bare digits/arrows go through the NSEvent monitor so text fields work).
    private var shortcutLayer: some View {
        Group {
            Button("") { undo() }.keyboardShortcut("z", modifiers: .command)
            Button("") { redo() }.keyboardShortcut("z", modifiers: [.command, .shift])
            Button("") { save() }.keyboardShortcut("s", modifiers: .command)
            Button("") { export() }.keyboardShortcut("e", modifiers: .command)
            Button("") { applyPrevRecipe() }.keyboardShortcut("=", modifiers: .command)
            Button("") { runAuto(.all) }.keyboardShortcut("c", modifiers: [.command, .option])
            Button("") { addStage() }.keyboardShortcut("s", modifiers: .option)
            Button("") { selStage = max(0, selStage - 1) }.keyboardShortcut(.leftArrow, modifiers: [.command, .option])
            Button("") { selStage = min(recipe.stages.count, selStage + 1) }.keyboardShortcut(.rightArrow, modifiers: [.command, .option])
        }
        .opacity(0)
        .frame(width: 0, height: 0)
        .allowsHitTesting(false)
    }

    // MARK: stage (image + filmstrip)

    private var stageColumn: some View {
        VStack(spacing: 0) {
            stageStrip
            GeometryReader { geo in
                ZStack {
                    Ara.bg0
                    if let image {
                        let rect = imageRect(in: geo.size)
                        if panoMode && panoLike {
                            // 360° viewer: developed image mapped onto a sphere
                            ZStack {
                                PanoView(image: image)
                                    .frame(width: rect.width, height: rect.height)
                                    .clipShape(RoundedRectangle(cornerRadius: 4))
                                    .position(x: rect.midX, y: rect.midY)
                                    .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                                Rectangle()
                                    .stroke(Ara.border, lineWidth: 0.5)
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                                    .allowsHitTesting(false)
                                Text("360°")
                                    .font(.system(size: 10, weight: .bold))
                                    .tracking(1.5)
                                    .padding(.horizontal, 8).padding(.vertical, 3)
                                    .background(Capsule().fill(Ara.accent))
                                    .foregroundStyle(Color.black.opacity(0.85))
                                    .position(x: rect.minX + 34, y: rect.minY + 16)
                                    .allowsHitTesting(false)
                                Text("drag to look · pinch to zoom · double-click to reset")
                                    .font(.system(size: 10))
                                    .foregroundStyle(Ara.text2)
                                    .padding(.horizontal, 8).padding(.vertical, 3)
                                    .background(Capsule().fill(.black.opacity(0.55)))
                                    .position(x: rect.midX, y: rect.maxY - 14)
                                    .allowsHitTesting(false)
                            }
                        } else {
                        // compare modes: baseline underneath (or alone)
                        if cmp != .off && cmp != .before {
                            if let baselineImg {
                                Image(baselineImg, scale: 1,
                                      label: Text(refRecipe != nil ? refName : "before"))
                                    .resizable()
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                            }
                        }
                        if cmp == .before {
                            if let baselineImg {
                                Image(baselineImg, scale: 1,
                                      label: Text(refRecipe != nil ? refName : photo.name))
                                    .resizable()
                                    .frame(width: rect.width, height: rect.height)
                                    .position(x: rect.midX, y: rect.midY)
                                    .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                            } else {
                                ProgressView().tint(Ara.accent)
                            }
                        } else {
                            Image(image, scale: 1, label: Text(photo.name))
                                .resizable()
                                .frame(width: rect.width, height: rect.height)
                                .position(x: rect.midX, y: rect.midY)
                                .shadow(color: .black.opacity(0.6), radius: 12, y: 4)
                                .mask(wipeMask(in: rect))
                                .blendMode(cmp == .diff ? .difference : .normal)
                                .opacity(cmp == .mix ? 0.5 : 1)
                        }
                        Rectangle()
                            .stroke(Ara.border, lineWidth: 0.5)
                            .frame(width: rect.width, height: rect.height)
                            .position(x: rect.midX, y: rect.midY)
                        retouchMarkers(in: rect)
                        if cmp == .wipeV || cmp == .wipeH {
                            wipeDivider(in: rect)
                        }
                        if cmp != .off {
                            Text(cmp.label.uppercased())
                                .font(.system(size: 10, weight: .bold))
                                .tracking(1.5)
                                .padding(.horizontal, 8).padding(.vertical, 3)
                                .background(Capsule().fill(Ara.accent))
                                .foregroundStyle(Color.black.opacity(0.85))
                                .position(x: rect.minX + 46, y: rect.minY + 16)
                        }
                        if retouchMode != "off" {
                            toolBanner
                                .position(x: rect.midX, y: rect.maxY - 20)
                        }
                        // darktable tone-equalizer badge: hovering band + EV
                        if zoneImgMode, let zi = zoneHover {
                            Text(String(format: "%+d EV band   %+.2f", zi - 4, edit.wrappedValue.zones_ev[zi]))
                                .font(.system(size: 10, weight: .semibold).monospacedDigit())
                                .foregroundStyle(Ara.text1)
                                .padding(.horizontal, 9).padding(.vertical, 4)
                                .background(Capsule().fill(.black.opacity(0.72))
                                    .overlay(Capsule().stroke(Ara.accent.opacity(0.5), lineWidth: 0.5)))
                                .position(x: min(lastHover.x + 78, geo.size.width - 90),
                                          y: max(lastHover.y - 22, 14))
                                .allowsHitTesting(false)
                        }
                        // RawTherapee navigator: minimap + draggable viewport
                        if zoom > 1.01 {
                            navigator(in: rect, stage: geo.size)
                                .position(x: 12 + 75, y: 12 + 75 * rect.height / max(rect.width, 1) / 2)
                        }
                        zoomControls
                            // pinned to the stage edge — the image rect grows
                            // past the viewport once zoomed in
                            .position(x: geo.size.width - 96, y: geo.size.height - 24)
                        }
                    } else {
                        ProgressView()
                            .tint(Ara.accent)
                    }
                    // chrome toggles, darktable panel-edge arrows
                    HStack(spacing: 4) {
                        IconAction(icon: "rectangle.bottomhalf.filled", active: showStrip) {
                            showStrip.toggle()
                        }
                        .help("Filmstrip")
                        IconAction(icon: "sidebar.right", active: showInspector) {
                            withAnimation(.easeInOut(duration: 0.15)) { showInspector.toggle() }
                        }
                        .help("Inspector")
                        if panoLike {
                            IconAction(icon: "globe", active: panoMode) {
                                panoMode.toggle()
                            }
                            .help("360° viewer")
                        }
                    }
                    .padding(.horizontal, 6).padding(.vertical, 4)
                    .background(Capsule().fill(.black.opacity(0.6))
                        .overlay(Capsule().stroke(Ara.hairline, lineWidth: 0.5)))
                    .position(x: geo.size.width - 44, y: 18)
                }
                .contentShape(Rectangle())
                .onContinuousHover { phase in
                    switch phase {
                    case .active(let loc): updateProbe(loc, in: geo.size)
                    case .ended: stageHover = false; probeText = ""; zoneHover = nil
                    }
                }
                .onAppear { stageSize = geo.size }
                .onChange(of: geo.size) { _, s in stageSize = s }
                .gesture(SpatialTapGesture().onEnded { v in
                    placeAt(v.location, in: geo.size)
                }, including: panoMode ? .none : .all)
                .gesture(DragGesture(minimumDistance: 4)
                    .onChanged { g in stageDrag(g, in: geo.size) }
                    .onEnded { _ in
                        if retouchMode == "brush" { commitBrushStroke() }
                        dragBase = nil
                    }, including: panoMode ? .none : .all)
                .gesture(MagnifyGesture()
                    .onChanged { v in
                        if !pinching { pinching = true; pinchBase = zoom }
                        zoom = (pinchBase * v.magnification).clamped(to: 0.5...8)
                        if zoom <= 1 { pan = .zero }
                    }
                    .onEnded { _ in pinching = false }, including: panoMode ? .none : .all)
                .gesture(TapGesture(count: 2).onEnded { _ in
                    // only reached when the spatial tap for tools didn't claim it
                    if retouchMode == "off" {
                        zoom = zoom > 1.01 ? 1 : 2
                        if zoom <= 1 { pan = .zero }
                    }
                }, including: panoMode ? .none : .all)
                .onHover { stageHover = $0 }
            }
            if showStrip {
                filmstrip
            }
            statusBar
        }
        .onAppear {
            keyMon.handler = { [self] ev in handleKey(ev) }
            keyMon.install()
            scrollMon.handler = { [self] ev in
                guard stageHover else { return true }
                // 360 viewer owns its own zoom (pinch); scroll does nothing
                if panoMode { return true }
                // darktable tone equalizer: scroll over the photo adjusts the
                // luminance band under the cursor instead of zooming
                if zoneImgMode, let zi = zoneIndex(at: lastHover) {
                    let d = Double(ev.scrollingDeltaY + ev.scrollingDeltaX)
                    if d != 0 {
                        var z = edit.wrappedValue.zones_ev
                        z[zi] = (z[zi] + d * 0.03).clamped(to: -4...4)
                        edit.wrappedValue.zones_ev = z
                        zoneHover = zi
                    }
                    return false
                }
                let nz = (zoom * (1 + ev.scrollingDeltaY * 0.003)).clamped(to: 0.5...8)
                if nz != zoom { zoom = nz; if zoom <= 1 { pan = .zero } }
                return true
            }
            scrollMon.install()
        }
        .onDisappear {
            keyMon.uninstall()
            scrollMon.uninstall()
        }
    }

    @State private var pinching = false
    @State private var pinchBase: CGFloat = 1
    @State private var dragBase: [Double]?
    @State private var keyMon = KeyMonitor()
    @State private var scrollMon = ScrollMonitor()

    /// wipe divider line + drag handle
    private func wipeDivider(in rect: CGRect) -> some View {
        let isV = cmp == .wipeV
        let pos = CGFloat(wipePos)
        let line: Path = isV
            ? Path { p in p.move(to: .init(x: rect.minX + pos * rect.width, y: rect.minY))
                          p.addLine(to: .init(x: rect.minX + pos * rect.width, y: rect.maxY)) }
            : Path { p in p.move(to: .init(x: rect.minX, y: rect.minY + pos * rect.height))
                          p.addLine(to: .init(x: rect.maxX, y: rect.minY + pos * rect.height)) }
        return ZStack {
            line.stroke(.white.opacity(0.85), lineWidth: 1.5)
            // invisible fat handle along the divider for dragging
            if isV {
                Rectangle().fill(.white.opacity(0.001))
                    .frame(width: 18, height: rect.height)
                    .position(x: rect.minX + pos * rect.width, y: rect.midY)
                    .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                        wipePos = Double((g.location.x - rect.minX) / rect.width).clamped(to: 0.02...0.98)
                    })
            } else {
                Rectangle().fill(.white.opacity(0.001))
                    .frame(width: rect.width, height: 18)
                    .position(x: rect.midX, y: rect.minY + pos * rect.height)
                    .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                        wipePos = Double((g.location.y - rect.minY) / rect.height).clamped(to: 0.02...0.98)
                    })
            }
        }
    }

    /// mask for the edited image, in the image's own layout space: the
    /// divider's "before" half is knocked out so the baseline shows through.
    private func wipeMask(in rect: CGRect) -> some View {
        GeometryReader { g in
            ZStack(alignment: .topLeading) {
                Color.white
                if cmp == .wipeV {
                    Color.black
                        .frame(width: g.size.width * CGFloat(wipePos), height: g.size.height)
                } else if cmp == .wipeH {
                    Color.black
                        .frame(width: g.size.width, height: g.size.height * CGFloat(wipePos))
                }
            }
        }
    }

    private var zoomControls: some View {
        HStack(spacing: 4) {
            IconAction(icon: "arrow.down.right.and.arrow.up.left") { zoom = 1; pan = .zero }
            IconAction(icon: "minus") { zoom = max(0.5, zoom - 0.25); if zoom <= 1 { pan = .zero } }
            Text(String(format: "%.0f%%", zoom * 100))
                .font(.system(size: 10).monospacedDigit())
                .foregroundStyle(Ara.text1)
                .frame(width: 34)
            IconAction(icon: "plus") { zoom = min(8, zoom + 0.25) }
            IconAction(icon: "1.magnifyingglass") { zoom = min(8, 1400 / CGFloat(image?.width ?? 1400)) }
        }
        .padding(.horizontal, 6).padding(.vertical, 4)
        .background(Capsule().fill(.black.opacity(0.6))
            .overlay(Capsule().stroke(Ara.hairline, lineWidth: 0.5)))
    }

    /// Stage drag routing: window move / gradient draw / pan when zoomed.
    private func stageDrag(_ g: DragGesture.Value, in size: CGSize) {
        if retouchMode == "brush" {
            guard let (_, _, fx, fy) = frameCoord(g.location, in: size) else { return }
            // sample the polyline at ~1/4-radius spacing so long strokes
            // stay smooth without flooding the segment buffer
            let minD = brushRadius * 0.25
            if let last = liveFrame.last {
                let dx = fx - last[0], dy = fy - last[1]
                if dx * dx + dy * dy < minD * minD { return }
            }
            liveFrame.append([fx, fy])
            if liveFrame.count > 1024 { liveFrame.removeFirst(liveFrame.count - 1024) }
            return
        }
        if retouchMode == "window" || retouchMode == "grad" {
            guard let (_, _, sfx, sfy) = frameCoord(g.startLocation, in: size),
                  let (_, _, fx, fy) = frameCoord(g.location, in: size) else { return }
            if retouchMode == "grad" {
                // drag defines the gradient line start→current
                if let i = edit.wrappedValue.windows.lastIndex(where: { $0.id == selWindow })
                    ?? edit.wrappedValue.windows.indices.last {
                    edit.wrappedValue.windows[i].kind = "gradient"
                    edit.wrappedValue.windows[i].p = [sfx, sfy, fx, fy, 0, 0.35]
                    selWindow = edit.wrappedValue.windows[i].id
                }
                return
            }
            // move the selected (or newest) window
            guard let i = edit.wrappedValue.windows.lastIndex(where: { $0.id == selWindow })
                ?? edit.wrappedValue.windows.indices.last else { return }
            if dragBase == nil { dragBase = edit.wrappedValue.windows[i].p }
            guard let base = dragBase else { return }
            var p = base
            let dfx = fx - sfx, dfy = fy - sfy
            if edit.wrappedValue.windows[i].kind == "gradient" {
                p[0] = base[0] + dfx; p[1] = base[1] + dfy
                p[2] = base[2] + dfx; p[3] = base[3] + dfy
            } else {
                p[0] = (base[0] + dfx).clamped(to: 0...1)
                p[1] = (base[1] + dfy).clamped(to: 0...1)
            }
            edit.wrappedValue.windows[i].p = p
        } else if retouchMode == "off" && zoom > 1.001 {
            if dragBase == nil { dragBase = [Double(pan.width), Double(pan.height)] }
            guard let base = dragBase, base.count == 2 else { return }
            pan = CGSize(width: base[0] + g.translation.width,
                         height: base[1] + g.translation.height)
        }
    }

    /// end of a brush drag: fold the sampled points into a stroke on the
    /// active layer (LR-style: strokes in one layer share its adjustment).
    private func commitBrushStroke() {
        guard liveFrame.count >= 2 else { liveFrame = []; return }
        if edit.wrappedValue.brushes.isEmpty { edit.wrappedValue.brushes.append(BrushLayer()) }
        if selBrush >= edit.wrappedValue.brushes.count { selBrush = edit.wrappedValue.brushes.count - 1 }
        var st = BrushStroke()
        st.pts = liveFrame
        st.radius = brushRadius
        st.soft = brushSoft
        st.opacity = brushFlow
        st.erase = brushErase
        edit.wrappedValue.brushes[selBrush].strokes.append(st)
        liveFrame = []
    }

    /// Bare-key shortcuts. Returns true when the key was consumed.
    private func handleKey(_ ev: NSEvent) -> Bool {
        // let text fields own the keyboard
        if let fr = ev.window?.firstResponder, fr is NSTextView || fr is NSTextField {
            return false
        }
        // DaVinci Printer Lights: Option+nudge offsets the whole grade toward a
        // printer-light colour. ⌥←/→ red−/+ (cyan↔red), ⌥↑/↓ yellow↔blue,
        // ⌥,/⌥. magenta↔green. Step 0.01 per press.
        if ev.modifierFlags.contains(.option),
           ev.modifierFlags.intersection(.deviceIndependentFlagsMask)
               .isDisjoint(with: [.command, .control]) {
            let step = 0.01
            let k = ev.keyCode
            var ch = -1, dir = 0.0
            switch k {
            case 123: ch = 0; dir = -step          // ⌥←
            case 124: ch = 0; dir = step           // ⌥→
            case 126: ch = 2; dir = step           // ⌥↑
            case 125: ch = 2; dir = -step          // ⌥↓
            default:
                if let c = ev.charactersIgnoringModifiers?.lowercased().first {
                    if c == "," { ch = 1; dir = -step }
                    else if c == "." { ch = 1; dir = step }
                }
            }
            if ch >= 0 {
                edit.wrappedValue.offset[ch] = min(0.25, max(-0.25, edit.wrappedValue.offset[ch] + dir))
                status = String(format: "Printer %@ %+.2f", ["R", "G", "B"][ch], edit.wrappedValue.offset[ch])
                return true
            }
            return false
        }
        guard ev.modifierFlags.intersection(.deviceIndependentFlagsMask)
            .isDisjoint(with: [.command, .control, .option]) else { return false }
        if let c = ev.charactersIgnoringModifiers?.lowercased().first {
            switch c {
            case "0": setRating(0); return true
            case "1"..."5": setRating(Int(String(c))!); return true
            case "b": cmp = (cmp == .before ? .off : .before); return true
            case "w":
                cmp = cmp == .wipeV ? .wipeH : cmp == .wipeH ? .off : .wipeV
                return true
            case "p": store.setFlag(path: photo.path, vslot: photo.vslot, flag == 1 ? 0 : 1); flag = flag == 1 ? 0 : 1; status = flag == 1 ? "Picked" : "Unflagged"; return true
            case "x": store.setFlag(path: photo.path, vslot: photo.vslot, flag == -1 ? 0 : -1); flag = flag == -1 ? 0 : -1; status = flag == -1 ? "Rejected" : "Unflagged"; return true
            case "u": store.setFlag(path: photo.path, vslot: photo.vslot, 0); flag = 0; status = "Unflagged"; return true
            default: break
            }
        }
        switch ev.keyCode {
        case 123: stepPhoto(-1); return true    // ←
        case 124: stepPhoto(1); return true     // →
        case 53:                                // esc
            if retouchMode != "off" { retouchMode = "off"; return true }
            if cmp != .off { cmp = .off; return true }
            return false
        default: return false
        }
    }

    @ViewBuilder
    private var toolBanner: some View {
        let names: [String: String] = [
            "heal": "Heal — tap blemishes", "clone": pendingClone == nil ? "Clone — tap source" : "Clone — tap destination",
            "dodge": "Dodge — tap to lighten", "burn": "Burn — tap to darken",
            "wbpick": "Pick WB — tap a neutral point", "window": "Window — tap to add, drag to move",
            "grad": "Gradient — drag to draw the line", "flare": "Flare — tap light position",
            "qpick": "Qualifier — tap a colour to key it",
            "qadd": "Qualifier + — tap to add to the key",
            "qsub": "Qualifier − — tap to remove from the key",
            "brush": brushErase ? "Brush — drag to erase" : "Brush — drag to paint, tap to dab",
        ]
        Text(names[retouchMode] ?? retouchMode)
            .font(.system(size: 10.5, weight: .medium))
            .foregroundStyle(Ara.text1)
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(Capsule().fill(.black.opacity(0.72))
                .overlay(Capsule().stroke(Ara.accent.opacity(0.5), lineWidth: 0.5)))
    }

    /// Cursor probe: sample the small sRGB probe buffer under the pointer
    /// and format an RT-style readout. Also feeds the zone-EQ badge.
    private func updateProbe(_ loc: CGPoint, in size: CGSize) {
        guard !panoMode else { probeText = ""; return }
        stageHover = true
        lastHover = loc
        let rect = imageRect(in: size)
        guard rect.contains(loc), probeW > 0, !probeBuf.isEmpty else {
            probeText = ""; zoneHover = nil; return
        }
        let nx = Double((loc.x - rect.minX) / rect.width)
        let ny = Double((loc.y - rect.minY) / rect.height)
        let px = min(max(Int(nx * Double(probeW)), 0), probeW - 1)
        let py = min(max(Int(ny * Double(probeH)), 0), probeH - 1)
        let o = (py * probeW + px) * 4
        guard o + 2 < probeBuf.count else { return }
        let r = Double(probeBuf[o]), g = Double(probeBuf[o + 1]), b = Double(probeBuf[o + 2])
        let (hh, ss, vv) = rgbHsv((r / 255, g / 255, b / 255))
        probeText = String(format: "%4d,%4d  ·  R%3d G%3d B%3d  ·  H%3.0f° S%2.0f%% V%2.0f%%",
                           Int(nx * Double(image?.width ?? 0)), Int(ny * Double(image?.height ?? 0)),
                           Int(r), Int(g), Int(b), hh * 360, ss * 100, vv * 100)
        if zoneImgMode { zoneHover = zoneIndex(luma: 0.2126 * r + 0.7152 * g + 0.0722 * b) }
    }

    /// sRGB luma byte → tone-equalizer band index 0...8 (centres −4…+4 EV).
    private func zoneIndex(luma l: Double) -> Int {
        min(max(Int((log2(max(l / 255, 0.015)) + 4).rounded()), 0), 8)
    }

    private func zoneIndex(at loc: CGPoint) -> Int? {
        guard let (nx, ny, _, _) = frameCoord(loc, in: stageSize),
              nx >= 0, nx <= 1, ny >= 0, ny <= 1,
              probeW > 0 else { return nil }
        let px = min(max(Int(nx * Double(probeW)), 0), probeW - 1)
        let py = min(max(Int(ny * Double(probeH)), 0), probeH - 1)
        let o = (py * probeW + px) * 4
        guard o + 2 < probeBuf.count else { return nil }
        return zoneIndex(luma: 0.2126 * Double(probeBuf[o]) + 0.7152 * Double(probeBuf[o + 1])
            + 0.0722 * Double(probeBuf[o + 2]))
    }

    /// RawTherapee navigator: mini preview + draggable viewport rectangle,
    /// shown top-left whenever zoomed in.
    private func navigator(in rect: CGRect, stage: CGSize) -> some View {
        let nw: CGFloat = 150
        let nh = nw * rect.height / max(rect.width, 1)
        let fx0 = Double((0 - rect.minX) / rect.width).clamped(to: 0...1)
        let fx1 = Double((stage.width - rect.minX) / rect.width).clamped(to: 0...1)
        let fy0 = Double((0 - rect.minY) / rect.height).clamped(to: 0...1)
        let fy1 = Double((stage.height - rect.minY) / rect.height).clamped(to: 0...1)
        return ZStack(alignment: .topLeading) {
            if let image {
                Image(image, scale: 1, label: Text("navigator"))
                    .resizable().frame(width: nw, height: nh).opacity(0.8)
            }
            Rectangle()
                .fill(Color.white.opacity(0.07))
                .overlay(Rectangle().stroke(Ara.accent, lineWidth: 1.2))
                .frame(width: max(CGFloat(fx1 - fx0) * nw, 6),
                       height: max(CGFloat(fy1 - fy0) * nh, 6))
                .offset(x: CGFloat(fx0) * nw, y: CGFloat(fy0) * nh)
        }
        .frame(width: nw, height: nh)
        .background(Ara.bg1.opacity(0.9))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Ara.border, lineWidth: 0.75))
        .shadow(color: .black.opacity(0.5), radius: 6, y: 2)
        .contentShape(Rectangle())
        .gesture(DragGesture(minimumDistance: 0).onChanged { g in
            // centre the viewport on the dragged point
            let fx = Double(g.location.x / nw).clamped(to: 0...1)
            let fy = Double(g.location.y / nh).clamped(to: 0...1)
            pan = CGSize(
                width: stage.width / 2 - CGFloat(fx) * rect.width - (stage.width - rect.width) / 2,
                height: stage.height / 2 - CGFloat(fy) * rect.height - (stage.height - rect.height) / 2)
        })
    }

    /// Bottom status strip (RawTherapee): probe readout / filename, render
    /// spinner and zoom readout — always visible, no inspector needed.
    private var statusBar: some View {
        VStack(spacing: 0) {
            Ara.hairline.frame(height: 1)
            HStack(spacing: 10) {
                Text(probeText.isEmpty ? photo.name : probeText)
                    .font(.system(size: 9.5).monospacedDigit())
                    .foregroundStyle(probeText.isEmpty ? Ara.text3 : Ara.text2)
                    .lineLimit(1).truncationMode(.middle)
                Spacer()
                if rendering {
                    ProgressView().controlSize(.mini).tint(Ara.accent)
                        .frame(width: 10, height: 10)
                }
                Text(zoneImgMode ? "zone scroll" : String(format: "%.0f%%", zoom * 100))
                    .font(.system(size: 9.5).monospacedDigit())
                    .foregroundStyle(zoneImgMode ? Ara.accent : Ara.text3)
            }
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(Ara.bg1)
        }
    }

    private var filmstrip: some View {
        VStack(spacing: 0) {
            Ara.hairline.frame(height: 1)
            ScrollViewReader { proxy in
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        ForEach(store.filtered) { p in
                            FilmCell(photo: p, selected: store.selection.contains(p.id),
                                     dirty: store.unsavedEdits[p.id] != nil)
                                .onTapGesture { store.select(p) }
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

    // MARK: stage strip (DaVinci serial nodes)

    /// Append a serial correction stage and select it (⌥S).
    private func addStage() {
        guard recipe.stages.count < 4 else { return }
        var st = Stage()
        st.name = "S\(recipe.stages.count + 1)"
        recipe.stages.append(st)
        selStage = recipe.stages.count
    }

    private func stageChip(_ idx: Int) -> some View {
        let isBase = idx == 0
        let st = isBase ? nil : recipe.stages[idx - 1]
        let sel = selStage == idx
        return HStack(spacing: 6) {
            Text("\(idx + 1)")
                .font(.system(size: 9, weight: .bold, design: .monospaced))
                .foregroundStyle(sel ? Color.black : Ara.text2)
                .frame(width: 15, height: 15)
                .background(Circle().fill(sel ? Ara.accent : Ara.bg3))
            Text(isBase ? "Base" : (st!.name.isEmpty ? "Stage \(idx)" : st!.name))
                .font(.system(size: 10.5, weight: sel ? .semibold : .regular))
                .foregroundStyle(st?.enabled == false ? Ara.text2.opacity(0.5) : Ara.text1)
                .lineLimit(1)
            if let s = st {
                if s.invert {
                    Image(systemName: "arrow.triangle.swap")
                        .font(.system(size: 7)).foregroundStyle(Color.orange)
                }
                Button {
                    recipe.stages[idx - 1].enabled.toggle()
                } label: {
                    Image(systemName: s.enabled ? "eye" : "eye.slash")
                        .font(.system(size: 8))
                        .foregroundStyle(s.enabled ? Ara.text2 : Color.orange)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 5)
        .background(RoundedRectangle(cornerRadius: 5)
            .fill(sel ? Ara.bg3 : Ara.bg2))
        .overlay(RoundedRectangle(cornerRadius: 5)
            .stroke(sel ? Ara.accent : Ara.border, lineWidth: sel ? 1.2 : 0.6))
        .contentShape(Rectangle())
        .onTapGesture { selStage = idx }
        .contextMenu {
            if !isBase {
                Button("Rename…") {
                    stageRenameText = st!.name
                    showStageRename = true
                }
                Button(st!.invert ? "Uninvert key" : "Invert key") {
                    recipe.stages[idx - 1].invert.toggle()
                }
                Button("Duplicate stage") {
                    if recipe.stages.count < 4 {
                        var c = st!
                        c.id = UUID(); c.name += " copy"
                        recipe.stages.insert(c, at: idx)
                    }
                }
                Divider()
                Button("Delete stage", role: .destructive) {
                    recipe.stages.remove(at: idx - 1)
                    if selStage > recipe.stages.count { selStage = recipe.stages.count }
                }
            }
        }
    }

    private var stageStrip: some View {
        HStack(spacing: 4) {
            stageChip(0)
            ForEach(Array(recipe.stages.indices), id: \.self) { i in
                Image(systemName: "chevron.right")
                    .font(.system(size: 7, weight: .bold))
                    .foregroundStyle(Ara.text2.opacity(0.6))
                stageChip(i + 1)
            }
            if recipe.stages.count < 4 {
                Button(action: addStage) {
                    Image(systemName: "plus")
                        .font(.system(size: 9, weight: .bold))
                        .foregroundStyle(Ara.text2)
                        .frame(width: 20, height: 20)
                        .background(Circle().stroke(Ara.border, lineWidth: 0.8))
                }
                .buttonStyle(.plain)
                .help("Add serial stage (⌥S)")
            }
            Spacer()
            if selStage > 0, selStage <= recipe.stages.count {
                let i = selStage - 1
                Text("Opacity")
                    .font(.system(size: 9)).foregroundStyle(Ara.text2)
                Slider(value: $recipe.stages[i].opacity, in: 0...1)
                    .frame(width: 90)
                Text(String(format: "%.2f", recipe.stages[i].opacity))
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(Ara.text2)
                    .frame(width: 32)
                Toggle("Invert", isOn: $recipe.stages[i].invert)
                    .toggleStyle(.checkbox)
                    .font(.system(size: 9))
                    .foregroundStyle(Ara.text2)
                // reliable path to stage ops — the chip's .contextMenu
                // doesn't fire on some macOS/SwiftUI combos
                Menu {
                    Button("Rename…") {
                        stageRenameText = recipe.stages[i].name
                        showStageRename = true
                    }
                    Button("Duplicate stage") {
                        if recipe.stages.count < 4 {
                            var c = recipe.stages[i]
                            c.id = UUID(); c.name += " copy"
                            recipe.stages.insert(c, at: selStage)
                        }
                    }
                    Divider()
                    Button("Delete stage", role: .destructive) {
                        recipe.stages.remove(at: i)
                        if selStage > recipe.stages.count { selStage = recipe.stages.count }
                    }
                } label: {
                    Image(systemName: "ellipsis.circle")
                        .font(.system(size: 11))
                        .foregroundStyle(Ara.text2)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .frame(width: 18)
                .help("Stage options")
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 5)
        .background(Ara.bg1)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Ara.border).frame(height: 0.5)
        }
        .alert("Stage name", isPresented: $showStageRename) {
            TextField("Name", text: $stageRenameText)
            Button("OK") {
                if selStage > 0, selStage <= recipe.stages.count {
                    recipe.stages[selStage - 1].name = stageRenameText
                }
            }
            Button("Cancel", role: .cancel) {}
        }
    }

    // MARK: inspector

    private var inspector: some View {
        VStack(spacing: 0) {
            // header: filename, stars, labels, undo, compare
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
                            guard r != loadedRating else { return }
                            loadedRating = r
                            store.setRating(path: photo.path, vslot: photo.vslot, r)
                        }
                }
                HStack(spacing: 6) {
                    LabelPicker(label: $label)
                        .onChange(of: label) { _, l in
                            guard l != loadedLabel else { return }
                            loadedLabel = l
                            store.setLabel(path: photo.path, vslot: photo.vslot, l)
                        }
                    Spacer()
                    IconAction(icon: "arrow.uturn.backward") { undo() }
                        .opacity(undoStack.isEmpty ? 0.35 : 1)
                    IconAction(icon: "arrow.uturn.forward") { redo() }
                        .opacity(redoStack.isEmpty ? 0.35 : 1)
                    Menu {
                        ForEach(CompareMode.allCases) { m in
                            Button {
                                cmp = m
                                if m != .off { ensureBaseline() }
                            } label: {
                                if cmp == m {
                                    Label(m.label, systemImage: "checkmark")
                                } else {
                                    Text(m.label)
                                }
                            }
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "rectangle.split.2x1")
                                .font(.system(size: 11, weight: .medium))
                            Text(cmp == .off ? "Compare" : cmp.label)
                                .font(.system(size: 10.5, weight: .medium))
                        }
                        .foregroundStyle(cmp == .off ? Ara.text2 : Ara.accent)
                        .padding(.horizontal, 8).padding(.vertical, 5)
                        .background(RoundedRectangle(cornerRadius: 6)
                            .fill(cmp == .off ? Ara.bg3 : Ara.accentSoft)
                            .overlay(RoundedRectangle(cornerRadius: 6).stroke(
                                cmp == .off ? Ara.hairline : Ara.accent.opacity(0.4), lineWidth: 0.5)))
                    }
                    .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 10)
            .background(Ara.bg1)
            .overlay(alignment: .bottom) { Ara.hairline.frame(height: 1) }

            paletteStrip

            // palette header: title + hint + per-palette reset
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(palette.title.uppercased())
                        .font(.system(size: 9.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Ara.text2)
                    Spacer()
                    if paletteDirty(palette) {
                        Button {
                            resetPalette(palette)
                        } label: {
                            Image(systemName: "arrow.counterclockwise")
                                .font(.system(size: 9.5, weight: .bold))
                                .foregroundStyle(Ara.accent)
                        }
                        .buttonStyle(.plain)
                        .help("Reset this palette")
                    }
                }
                Text(palette.hint)
                    .font(.system(size: 8.5))
                    .foregroundStyle(Ara.text3)
                    .lineLimit(2).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
            .background(Ara.bg1)
            .overlay(alignment: .bottom) { Ara.hairline.frame(height: 1) }
            ScrollView {
                VStack(alignment: .leading, spacing: 10) {
                    paletteContent
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

    /// DaVinci palette tab strip: icon per palette, underline when active,
    /// amber dot top-right when that section has non-default values.
    private var paletteStrip: some View {
        HStack(spacing: 0) {
            ForEach(Palette.allCases) { p in
                Button {
                    palette = p
                    if p == .windows || p == .qualifier { /* keep tool context */ }
                } label: {
                    VStack(spacing: 2) {
                        ZStack(alignment: .topTrailing) {
                            Image(systemName: p.icon)
                                .font(.system(size: 12))
                                .foregroundStyle(palette == p ? Ara.accent : Ara.text2)
                                .frame(width: 18, height: 16)
                            if paletteDirty(p) {
                                Circle().fill(Ara.accent)
                                    .frame(width: 4, height: 4)
                                    .offset(x: 3, y: -2)
                            }
                        }
                        Rectangle()
                            .fill(palette == p ? Ara.accent : .clear)
                            .frame(height: 2)
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 5)
                    .background(palette == p ? Ara.bg2 : .clear)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help(p.title)
            }
        }
        .padding(.horizontal, 4)
        .background(Ara.bg1)
        .overlay(alignment: .bottom) { Ara.hairline.frame(height: 1) }
    }

    @ViewBuilder
    private var paletteContent: some View {
        switch palette {
        case .quick: quickPalette
        case .meters: metersPalette
        case .light: lightPalette
        case .wheels: wheelsPalette
        case .curves: curvesPalette
        case .zones: zonesPalette
        case .slice: slicePalette
        case .warp: warpPalette
        case .qualifier: qualifierPalette
        case .windows: windowsPalette
        case .mixer: mixerPalette
        case .retouch: retouchPalette
        case .detail: detailPalette
        case .fx: fxPalette
        case .xform: xformPalette
        case .versions: versionsPalette
        }
    }

    /// darktable quick-access panel: the 20% of controls used for 80% of
    /// edits, curated in one flat scroll — histogram, WB, tone, colour.
    private var quickPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Histogram") {
                if !hist.isEmpty {
                    HistogramView(hist: hist).frame(height: 72)
                } else {
                    Rectangle().fill(Ara.bg3).frame(height: 72)
                        .overlay(ProgressView().tint(Ara.text3))
                }
            }
            Panel("White Balance", trailing: {
                ToolChip(label: "Pick", icon: "eyedropper", active: retouchMode == "wbpick") {
                    retouchMode = retouchMode == "wbpick" ? "off" : "wbpick"
                }
            }) {
                SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                           (.manual, "Manual"), (.pick, "Pick")],
                          selection: $recipe.wb_mode)
                SliderRow("Temp", $recipe.temperature, -1...1, track: Ara.tempTrack)
                SliderRow("Tint", $recipe.tint, -1...1, track: Ara.tintTrack)
            }
            Panel("Tone", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.all) }
                    .help("Analyze the photo and apply every suggestion")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Exposure", edit.exposure, -4...4, step: 0.05)
                SliderRow("Contrast", edit.contrast, -1...1)
                SliderRow("Highlights", edit.highlights, -1...1)
                SliderRow("Shadows", edit.shadows, -1...1)
                SliderRow("Whites", edit.whites, -1...1)
                SliderRow("Blacks", edit.blacks, -1...1)
            }
            Panel("Colour", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.colour) }
                    .help("Estimate vibrance from the chroma distribution")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Saturation", edit.saturation, -1...1)
                SliderRow("Vibrance", edit.vibrance, -1...1)
                SliderRow("Clarity", $recipe.clarity, -1...1)
            }
            Panel("Geometry", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.geometry) }
                    .help("Detect horizon tilt and converging verticals")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
            }
        }
    }

    private var metersPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Histogram") {
                if !hist.isEmpty {
                    HistogramView(hist: hist).frame(height: 84)
                } else {
                    Rectangle().fill(Ara.bg3).frame(height: 84)
                        .overlay(ProgressView().tint(Ara.text3))
                }
            }
            Panel("Scopes") {
                SegPicker([("parade", "Parade"), ("vector", "Vector"), ("cie", "CIE")],
                          selection: $scopeKind)
                ZStack {
                    ScopesView(wave: wave, vec: vec, cie: cie, kind: scopeKind)
                    // darktable: the waveform itself is a control — vertical
                    // drag sets exposure, double-click resets it. The
                    // minimumDistance:0 drag swallows onTapGesture, so the
                    // double-click is detected manually like in TrackSlider.
                    if scopeKind == "parade" {
                        Color.white.opacity(0.001)
                            .contentShape(Rectangle())
                            .gesture(DragGesture(minimumDistance: 0)
                                .onChanged { g in
                                    if scopeDragBase == nil {
                                        scopeDragBase = edit.wrappedValue.exposure
                                        if g.time.timeIntervalSince(scopeTapTime) < 0.3 {
                                            edit.wrappedValue.exposure = 0
                                            scopeDragBase = 0
                                            scopeTapTime = .distantPast
                                            return
                                        }
                                        scopeTapTime = g.time
                                    }
                                    edit.wrappedValue.exposure = ((scopeDragBase ?? 0)
                                        - Double(g.translation.height) * 0.015)
                                        .clamped(to: -4...4)
                                }
                                .onEnded { _ in scopeDragBase = nil })
                    }
                }
                .frame(height: 128)
                Text(scopeKind == "parade"
                     ? "Drag up/down on the parade = exposure · double-click resets"
                     : "Switch to Parade for drag-to-expose")
                    .font(.system(size: 8.5)).foregroundStyle(Ara.text3)
            }
        }
    }

    private var lightPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("White Balance", trailing: {
                ToolChip(label: "Pick", icon: "eyedropper", active: retouchMode == "wbpick") {
                    retouchMode = retouchMode == "wbpick" ? "off" : "wbpick"
                }
            }) {
                SegPicker([(WbMode.asShot, "As Shot"), (.auto, "Auto"),
                           (.manual, "Manual"), (.pick, "Pick")],
                          selection: $recipe.wb_mode)
                if recipe.wb_mode == .pick || retouchMode == "wbpick" {
                    SliderRow("Pick Area", $recipe.wb_pick_size, 0.002...0.2, reset: 0.025)
                        .help("Sampling half-width of the WB eyedropper as a fraction of the frame")
                }
                SliderRow("Temp", $recipe.temperature, -1...1, track: Ara.tempTrack)
                SliderRow("Tint", $recipe.tint, -1...1, track: Ara.tintTrack)
            }
            Panel("Tone", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") {
                    recipe.wb_mode = .auto
                    edit.wrappedValue.auto_exposure = true
                    edit.wrappedValue.auto_contrast = true
                    status = "Auto: WB + exposure + contrast"
                }
            }) {
                HStack(spacing: 6) {
                    Toggle("Auto Exp", isOn: edit.auto_exposure)
                        .font(.system(size: 10)).foregroundStyle(Ara.text2)
                        .controlSize(.mini)
                    Toggle("Auto Contrast", isOn: edit.auto_contrast)
                        .font(.system(size: 10)).foregroundStyle(Ara.text2)
                        .controlSize(.mini)
                }
                SliderRow("Exposure", edit.exposure, -4...4, step: 0.05)
                SliderRow("Contrast", edit.contrast, -1...1)
                SliderRow("Pivot", edit.pivot, 0.05...0.5, reset: 0.18)
                SliderRow("Highlights", edit.highlights, -1...1)
                SliderRow("Shadows", edit.shadows, -1...1)
                SliderRow("Whites", edit.whites, -1...1)
                SliderRow("Blacks", edit.blacks, -1...1)
                SliderRow("HL Roll", edit.highlight_rolloff, 0.5...2, reset: 1.0)
                SliderRow("SH Roll", edit.shadow_rolloff, 0.5...2, reset: 1.0)
            }
            Panel("Color") {
                SliderRow("Saturation", edit.saturation, -1...1)
                SliderRow("Vibrance", edit.vibrance, -1...1)
            }
        }
    }

    private var wheelsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Primaries", trailing: { LookPicker(look: edit.look, lutFile: edit.lut_file, photo: photo, recipe: edit.wrappedValue) }) {
                HStack(spacing: 6) {
                    ColorWheel(title: "Lift", v: edit.lift, center: 0)
                    ColorWheel(title: "Gamma", v: edit.gamma, center: 1)
                    ColorWheel(title: "Gain", v: edit.gain, center: 1)
                    ColorWheel(title: "Offset", v: edit.offset, center: 0)
                }
                HStack(spacing: 6) {
                    Text("LUT")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(Ara.text2)
                        .frame(width: 60, alignment: .leading)
                    ToolChip(label: edit.wrappedValue.lut_file.isEmpty
                             ? "Choose .cube…"
                             : URL(fileURLWithPath: edit.wrappedValue.lut_file)
                                .deletingPathExtension().lastPathComponent,
                             icon: "doc.badge.plus") { pickLut() }
                    if !edit.wrappedValue.lut_file.isEmpty {
                        ToolChip(label: "", icon: "xmark") { edit.wrappedValue.lut_file = "" }
                            .help("Clear LUT")
                    }
                    Spacer()
                }
                if !edit.wrappedValue.lut_file.isEmpty {
                    SliderRow("Amount", edit.lut_amount, 0...1, reset: 1)
                }
            }
            Panel("Split Tone") {
                SliderRow("Shd Hue", edit.shadow_hue, 0...1, reset: 0.55, track: Ara.hueTrack)
                SliderRow("Shd Sat", edit.shadow_sat, 0...1)
                SliderRow("Mid Hue", edit.midtone_hue, 0...1, reset: 0.55, track: Ara.hueTrack)
                SliderRow("Mid Sat", edit.midtone_sat, 0...1)
                SliderRow("Hi Hue", edit.highlight_hue, 0...1, reset: 0.08, track: Ara.hueTrack)
                SliderRow("Hi Sat", edit.highlight_sat, 0...1)
            }
        }
    }

    private var curvesPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Curves") {
                SegPicker([(0, "Y"), (1, "R"), (2, "G"), (3, "B"),
                           (4, "H·H"), (5, "H·S"), (6, "H·L"), (7, "L·S"), (8, "S·S")],
                          selection: $curveChan)
                CurveEditor(points: curveBinding(curveChan),
                            tint: curveTint(curveChan), hist: hist.count > 3 ? hist[3] : [],
                            spectrum: curveChan >= 4 && curveChan <= 6)
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
        }
    }

    private var zonesPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Tone Equalizer", trailing: {
                HStack(spacing: 4) {
                    ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.zones) }
                        .help("Set band gains from the luma histogram")
                        .opacity(autoBusy ? 0.5 : 1)
                    ToolChip(label: "Image", icon: "hand.draw", active: zoneImgMode) {
                        zoneImgMode.toggle()
                    }
                    .help("Scroll on the photo to adjust the band under the cursor")
                    ToolChip(label: "Reset", icon: "arrow.counterclockwise") {
                        edit.wrappedValue.zones_ev = [Double](repeating: 0, count: 9)
                    }
                }
            }) {
                ZoneEQ(zones: edit.zones_ev)
                Text(zoneImgMode
                     ? "Hover the photo and scroll — the EV badge marks the band under the cursor"
                     : "EV gain per luminance band (−4…+4 EV around mid grey)")
                    .font(.system(size: 8.5)).foregroundStyle(zoneImgMode ? Ara.accent : Ara.text3)
            }
            Panel("HDR Zones") {
                ZoneRow("Dark", edit.z_dark)
                ZoneRow("Shadow", edit.z_shadow)
                ZoneRow("Light", edit.z_light)
                ZoneRow("Global", edit.z_global)
            }
        }
    }

    // MARK: ColorSlice (DaVinci)

    static let sliceNames = ["Red", "Skin", "Yellow", "Green", "Cyan", "Blue", "Magenta"]
    static let sliceHues: [Double] = [0.0, 0.0833, 0.1667, 0.3333, 0.5, 0.6667, 0.8333]
    @State private var selSlice = 0

    private var slicePalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("ColorSlice", trailing: {
                ToolChip(label: "Reset", icon: "arrow.counterclockwise") {
                    edit.wrappedValue.color_slice = Recipe().color_slice
                }
            }) {
                // wedge chips: pick the vector to edit
                HStack(spacing: 4) {
                    ForEach(0..<7, id: \.self) { i in
                        let on = edit.wrappedValue.color_slice[i][3] > 0.5
                        VStack(spacing: 2) {
                            Circle()
                                .fill(Color(hue: Self.sliceHues[i], saturation: 0.85, brightness: on ? 1.0 : 0.55))
                                .frame(width: 16, height: 16)
                                .overlay(Circle().stroke(selSlice == i ? Ara.accent : Ara.border,
                                                         lineWidth: selSlice == i ? 1.6 : 0.5))
                            Text(Self.sliceNames[i].prefix(1))
                                .font(.system(size: 7.5))
                                .foregroundStyle(selSlice == i ? Ara.accent : Ara.text2)
                        }
                        .frame(maxWidth: .infinity)
                        .contentShape(Rectangle())
                        .onTapGesture { selSlice = i }
                    }
                }
                .padding(.vertical, 2)
                // per-slice controls: [hue_shift, sat_delta, lum_delta, enabled]
                HStack {
                    Text(Self.sliceNames[selSlice])
                        .font(.system(size: 10.5, weight: .semibold))
                        .foregroundStyle(Ara.text1)
                    Spacer()
                    Toggle("", isOn: Binding(
                        get: { edit.wrappedValue.color_slice[selSlice][3] > 0.5 },
                        set: { edit.wrappedValue.color_slice[selSlice][3] = $0 ? 1 : 0 }))
                        .toggleStyle(.checkbox)
                        .labelsHidden()
                }
                SliderRow("Hue", Binding(
                    get: { edit.wrappedValue.color_slice[selSlice][0] * 360 },
                    set: { edit.wrappedValue.color_slice[selSlice][0] = $0 / 360; edit.wrappedValue.color_slice[selSlice][3] = 1 }),
                    -30...30, step: 0.5, reset: 0)
                SliderRow("Saturation", Binding(
                    get: { edit.wrappedValue.color_slice[selSlice][1] },
                    set: { edit.wrappedValue.color_slice[selSlice][1] = $0; edit.wrappedValue.color_slice[selSlice][3] = 1 }),
                    -0.8...0.8, reset: 0)
                SliderRow("Density", Binding(
                    get: { -edit.wrappedValue.color_slice[selSlice][2] },
                    set: { edit.wrappedValue.color_slice[selSlice][2] = -$0; edit.wrappedValue.color_slice[selSlice][3] = 1 }),
                    -0.6...0.6, reset: 0)
                Text("Density adds depth — positive darkens the wedge, negative lifts it")
                    .font(.system(size: 8.5)).foregroundStyle(Ara.text3)
            }
        }
    }

    // MARK: ColorWarper (DaVinci Hue-Sat spider graph)

    @State private var warpDrag = -1
    @State private var warpDragNew = false
    @State private var warpTapIdx = -1
    @State private var warpTapTime = Date.distantPast

    /// graph coords → (h, s); radius = saturation 0..1
    private func warpHS(_ loc: CGPoint, in size: CGSize) -> (h: Double, s: Double) {
        let c = CGPoint(x: size.width / 2, y: size.height / 2)
        let r = min(size.width, size.height) / 2 - 8
        let dx = Double(loc.x - c.x) / r, dy = Double(loc.y - c.y) / r
        let s = min(1.0, (dx * dx + dy * dy).squareRoot())
        var h = atan2(dy, dx) / (2 * .pi)
        if h < 0 { h += 1 }
        return (h, s)
    }
    private func warpPt(_ h: Double, _ s: Double, in size: CGSize) -> CGPoint {
        let c = CGPoint(x: size.width / 2, y: size.height / 2)
        let r = min(size.width, size.height) / 2 - 8
        return CGPoint(x: c.x + CGFloat(s * cos(h * 2 * .pi)) * r,
                       y: c.y + CGFloat(s * sin(h * 2 * .pi)) * r)
    }

    private var warpPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Color Warper — Hue vs Saturation", trailing: {
                ToolChip(label: "Reset", icon: "arrow.counterclockwise") {
                    edit.wrappedValue.warper = []
                }
            }) {
                GeometryReader { geo in
                    let sz = geo.size
                    Canvas { ctx, size in
                        let c = CGPoint(x: size.width / 2, y: size.height / 2)
                        let r = min(size.width, size.height) / 2 - 8
                        // concentric sat rings at .25/.5/.75/1
                        for k in 1...4 {
                            let rr = r * CGFloat(k) / 4
                            ctx.stroke(Circle().path(in: CGRect(x: c.x - rr, y: c.y - rr, width: rr * 2, height: rr * 2)),
                                       with: .color(Ara.text2.opacity(k == 4 ? 0.5 : 0.22)), lineWidth: k == 4 ? 1 : 0.5)
                        }
                        // spokes every 30° + wedge tint ticks on the rim
                        for k in 0..<12 {
                            let a = CGFloat(k) * .pi / 6
                            var p = Path()
                            p.move(to: c)
                            p.addLine(to: CGPoint(x: c.x + cos(a) * r, y: c.y + sin(a) * r))
                            ctx.stroke(p, with: .color(Ara.text2.opacity(0.15)), lineWidth: 0.5)
                            ctx.fill(Circle().path(in: CGRect(
                                x: c.x + cos(a) * r - 2.5, y: c.y + sin(a) * r - 2.5,
                                width: 5, height: 5)),
                                with: .color(Color(hue: Double(k) / 12, saturation: 0.9, brightness: 0.95)))
                        }
                        // control points: anchor ring + line to displaced dot
                        for w in edit.wrappedValue.warper {
                            guard w.count >= 5 else { continue }
                            let ah = w[0], as_ = w[1]
                            let dh = w[2], ds = w[3]
                            let a = warpPt(ah, as_, in: size)
                            let d = warpPt((ah + dh).truncatingRemainder(dividingBy: 1), (as_ + ds).clamped(to: 0...1), in: size)
                            var ln = Path(); ln.move(to: a); ln.addLine(to: d)
                            ctx.stroke(ln, with: .color(.white.opacity(0.6)), lineWidth: 1)
                            ctx.stroke(Circle().path(in: CGRect(x: a.x - 3.5, y: a.y - 3.5, width: 7, height: 7)),
                                       with: .color(.white.opacity(0.5)), lineWidth: 1)
                            ctx.fill(Circle().path(in: CGRect(x: d.x - 5, y: d.y - 5, width: 10, height: 10)),
                                     with: .color(Color(hue: ah, saturation: 0.85, brightness: 1.0)))
                            ctx.stroke(Circle().path(in: CGRect(x: d.x - 5, y: d.y - 5, width: 10, height: 10)),
                                       with: .color(.white), lineWidth: 1)
                        }
                    }
                    .gesture(DragGesture(minimumDistance: 0)
                        .onChanged { g in
                            if warpDrag < 0 {
                                // hit-test displaced dots first (12px), else create a point
                                let hit = edit.wrappedValue.warper.indices.first { i in
                                    let w = edit.wrappedValue.warper[i]
                                    guard w.count >= 5 else { return false }
                                    let d = warpPt((w[0] + w[2]).truncatingRemainder(dividingBy: 1),
                                                   (w[1] + w[3]).clamped(to: 0...1), in: sz)
                                    return abs(d.x - g.startLocation.x) < 12 && abs(d.y - g.startLocation.y) < 12
                                }
                                if let i = hit { warpDrag = i }
                                else if edit.wrappedValue.warper.count < 8 {
                                    let (h, s) = warpHS(g.startLocation, in: sz)
                                    edit.wrappedValue.warper.append([h, s, 0, 0, 0.15])
                                    warpDrag = edit.wrappedValue.warper.count - 1
                                    warpDragNew = true
                                } else { return }
                            }
                            guard warpDrag >= 0, warpDrag < edit.wrappedValue.warper.count else { return }
                            let (h2, s2) = warpHS(g.location, in: sz)
                            var dh = h2 - edit.wrappedValue.warper[warpDrag][0]
                            if dh > 0.5 { dh -= 1 } else if dh < -0.5 { dh += 1 }
                            edit.wrappedValue.warper[warpDrag][2] = dh
                            edit.wrappedValue.warper[warpDrag][3] = (s2 - edit.wrappedValue.warper[warpDrag][1]).clamped(to: -1...1)
                        }
                        .onEnded { g in
                            // a near-zero drag on an existing point counts as a
                            // tap — a second one within 0.4s deletes the point
                            let moved = abs(g.location.x - g.startLocation.x) > 3
                                || abs(g.location.y - g.startLocation.y) > 3
                            if warpDrag >= 0, !moved, !warpDragNew {
                                let now = Date()
                                if warpDrag == warpTapIdx,
                                   now.timeIntervalSince(warpTapTime) < 0.4 {
                                    edit.wrappedValue.warper.remove(at: warpDrag)
                                    warpTapIdx = -1
                                } else {
                                    warpTapIdx = warpDrag
                                    warpTapTime = now
                                }
                            }
                            warpDrag = -1
                            warpDragNew = false
                        })
                }
                .aspectRatio(1, contentMode: .fit)
                .background(Ara.bg0)
                .clipShape(RoundedRectangle(cornerRadius: 6))
                Text("\(edit.wrappedValue.warper.count) point\(edit.wrappedValue.warper.count == 1 ? "" : "s") — ring = pinned colour, dot = where it moves")
                    .font(.system(size: 8.5)).foregroundStyle(Ara.text3)
            }
        }
    }

    /// DaVinci qualifier: eyedroppers + HSL gradient range bars + finesse.
    private var qualifierPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Qualifier", trailing: {
                Toggle("", isOn: edit.q_enabled)
                    .labelsHidden().controlSize(.mini).tint(Ara.accent)
                    .onChange(of: edit.wrappedValue.q_enabled) { _, on in
                        if !on { edit.wrappedValue.qh[1] = 0; edit.wrappedValue.q_show = false }
                        else if edit.wrappedValue.qh[1] == 0 { edit.wrappedValue.qh[1] = 0.1 }
                    }
            }) {
                // eyedropper row: pick / add / subtract
                HStack(spacing: 6) {
                    Text("Pick").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                    ToolChip(label: "New", icon: "eyedropper",
                             active: retouchMode == "qpick") {
                        retouchMode = retouchMode == "qpick" ? "off" : "qpick"
                    }
                    ToolChip(label: "+", icon: "plus.circle",
                             active: retouchMode == "qadd") {
                        retouchMode = retouchMode == "qadd" ? "off" : "qadd"
                    }
                    ToolChip(label: "−", icon: "minus.circle",
                             active: retouchMode == "qsub") {
                        retouchMode = retouchMode == "qsub" ? "off" : "qsub"
                    }
                    Spacer()
                    ToolChip(label: "View", icon: "eye", active: edit.wrappedValue.q_show) {
                        edit.wrappedValue.q_show.toggle()
                    }
                    .help("Highlight: show the matte — keyed area in colour, rest grey")
                }
                if edit.wrappedValue.q_enabled {
                    // HSL gradient bars
                    RangeBar(title: "Hue",
                             lo: Binding(
                                 get: { (edit.wrappedValue.qh[0] - edit.wrappedValue.qh[1]).clamped(to: 0...1) },
                                 set: { v in
                                     let hi = (edit.wrappedValue.qh[0] + edit.wrappedValue.qh[1]).clamped(to: 0...1)
                                     edit.wrappedValue.qh[0] = (v + hi) / 2
                                     edit.wrappedValue.qh[1] = max(0.002, (hi - v) / 2)
                                 }),
                             hi: Binding(
                                 get: { (edit.wrappedValue.qh[0] + edit.wrappedValue.qh[1]).clamped(to: 0...1) },
                                 set: { v in
                                     let lo = (edit.wrappedValue.qh[0] - edit.wrappedValue.qh[1]).clamped(to: 0...1)
                                     edit.wrappedValue.qh[0] = (lo + v) / 2
                                     edit.wrappedValue.qh[1] = max(0.002, (v - lo) / 2)
                                 }),
                             gradient: LinearGradient(
                                 colors: (0...12).map { Color(hue: Double($0) / 12, saturation: 0.8, brightness: 0.9) },
                                 startPoint: .leading, endPoint: .trailing))
                    RangeBar(title: "Sat",
                             lo: edit.qs[0], hi: edit.qs[1],
                             gradient: LinearGradient(colors: [.gray.opacity(0.4), .orange],
                                                      startPoint: .leading, endPoint: .trailing))
                    RangeBar(title: "Lum",
                             lo: edit.ql[0], hi: edit.ql[1],
                             gradient: LinearGradient(colors: [.black, .white],
                                                      startPoint: .leading, endPoint: .trailing))
                    SliderRow("Hue Soft", edit.qh[2], 0.01...0.4, reset: 0.1)
                    SliderRow("Sat Soft", edit.qs[2], 0.01...0.4, reset: 0.1)
                    SliderRow("Lum Soft", edit.ql[2], 0.01...0.4, reset: 0.1)
                    Text("MATTE FINESSE")
                        .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Ara.text3)
                    SliderRow("Clean Blk", edit.q_clean[0], 0...1)
                    SliderRow("Clean Wht", edit.q_clean[1], 0...1, reset: 1)
                    SliderRow("Blur", edit.q_blur, 0...1)
                    HStack {
                        Text("Invert mask").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                        Spacer()
                        Toggle("", isOn: edit.q_invert)
                            .labelsHidden().controlSize(.mini).tint(Ara.accent)
                    }
                    Text("ADJUST INSIDE KEY")
                        .font(.system(size: 8.5, weight: .semibold)).tracking(1.2)
                        .foregroundStyle(Ara.text3)
                    SliderRow("Hue Δ", edit.qadj[0], -0.5...0.5, track: Ara.hueTrack)
                    SliderRow("Sat Δ", edit.qadj[1], -1...1)
                    SliderRow("Lum Δ", edit.qadj[2], -1...1)
                    SliderRow("Temp Δ", edit.qadj[3], -1...1)
                }
            }
        }
    }

    private var windowsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Power Windows", trailing: {
                HStack(spacing: 4) {
                    ToolChip(label: "Circle", icon: "plus.circle", active: retouchMode == "window") {
                        retouchMode = retouchMode == "window" ? "off" : "window"
                        if retouchMode == "window" { palette = .windows }
                    }
                    ToolChip(label: "Grad", icon: "plus.rectangle", active: retouchMode == "grad") {
                        retouchMode = retouchMode == "grad" ? "off" : "grad"
                    }
                    ToolChip(label: "Lum", icon: "circle.lefthalf.filled") {
                        var w = PowerWindow()
                        w.kind = "lum"
                        w.p = [0.0, 0.6, 0.1, 0.1, 0, 0]
                        edit.wrappedValue.windows.append(w)
                        selWindow = w.id
                    }
                    .help("Luminance-range mask: selects a band of the image by brightness")
                    ToolChip(label: "Subj", icon: "person.crop.square", active: subjBusy) {
                        runSubjectMask()
                    }
                    .help(subjReady
                          ? "Add an AI subject mask window (U-2-Net matte is prepared)"
                          : "Run AI subject detection (seconds), then add a mask window")
                    .opacity(subjBusy ? 0.5 : 1)
                }
            }) {
                if edit.wrappedValue.windows.isEmpty {
                    Text("Add a circle, gradient, or luminance window, then tap or drag on the image. " +
                         "Select a window row, then drag on the image to move it.")
                        .font(.system(size: 10)).foregroundStyle(Ara.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(edit.wrappedValue.windows) { w in
                    if let i = edit.wrappedValue.windows.firstIndex(where: { $0.id == w.id }) {
                        WindowRow(w: edit.windows[i],
                                  selected: selWindow == w.id,
                                  onSelect: { selWindow = w.id }) {
                            if selWindow == w.id { selWindow = nil }
                            edit.wrappedValue.windows.remove(at: i)
                        }
                    }
                }
            }
        }
    }

    private var mixerPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("RGB Mixer") {
                VStack(spacing: 5) {
                    ForEach(0..<3, id: \.self) { row in
                        HStack(spacing: 5) {
                            Text(["R′", "G′", "B′"][row])
                                .font(.system(size: 10, weight: .semibold))
                                .foregroundStyle([Color.red.opacity(0.9), .green.opacity(0.9), .blue.opacity(0.9)][row])
                                .frame(width: 14, alignment: .leading)
                            MixRow(edit.mixer, row: row)
                        }
                    }
                }
                HStack {
                    Text("Monochrome").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                    Spacer()
                    Toggle("", isOn: Binding(
                        get: { edit.wrappedValue.mono != [0, 0, 0] },
                        set: { edit.wrappedValue.mono = $0 ? [0.21, 0.72, 0.07] : [0, 0, 0] }
                    ))
                    .labelsHidden().controlSize(.mini).tint(Ara.accent)
                }
                if edit.wrappedValue.mono != [0, 0, 0] {
                    TriRow("Mono", edit.mono, 0...1)
                }
            }
        }
    }

    private var retouchPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Retouch") {
                SegPicker([("off", "Off"), ("heal", "Heal"), ("clone", "Clone"),
                           ("dodge", "Dodge"), ("burn", "Burn"), ("brush", "Brush")],
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
                // adjustment brush: paint strokes, layer holds the adjustment
                if retouchMode == "brush" || !edit.wrappedValue.brushes.isEmpty {
                    Divider().overlay(Ara.hairline)
                    HStack(spacing: 6) {
                        Text("Brush").font(.system(size: 10.5, weight: .semibold))
                            .foregroundStyle(Ara.text2)
                        Spacer()
                        ToolChip(label: "Erase", icon: "eraser", active: brushErase) {
                            brushErase.toggle()
                        }
                        .help("Erase strokes from the active layer")
                        ToolChip(label: "+Layer", icon: "plus", active: false) {
                            if edit.wrappedValue.brushes.count < 4 {
                                edit.wrappedValue.brushes.append(BrushLayer())
                                selBrush = edit.wrappedValue.brushes.count - 1
                            }
                        }
                        .help("New brush layer with its own adjustment (max 4)")
                    }
                    SliderRow("Size", $brushRadius, 0.005...0.25, reset: 0.04)
                    SliderRow("Feather", $brushSoft, 0...1, reset: 0.5)
                    SliderRow("Flow", $brushFlow, 0.05...1, reset: 1)
                    ForEach(Array(edit.wrappedValue.brushes.enumerated()), id: \.element.id) { li, layer in
                        BrushLayerRow(layer: layer, index: li, selected: li == selBrush,
                                      onSelect: { selBrush = li },
                                      onToggle: { edit.wrappedValue.brushes[li].enabled.toggle() },
                                      onDelete: {
                            edit.wrappedValue.brushes.remove(at: li)
                            selBrush = max(0, min(selBrush, edit.wrappedValue.brushes.count - 1))
                        })
                    }
                    if edit.wrappedValue.brushes.indices.contains(selBrush) {
                        let bl = Binding<BrushLayer>(
                            get: { edit.wrappedValue.brushes[selBrush] },
                            set: { edit.wrappedValue.brushes[selBrush] = $0 })
                        SliderRow("Exposure", bl.ev, -4...4)
                        SliderRow("Saturation", bl.sat, -1...1)
                        SliderRow("Temp", bl.temp, -1...1)
                        SliderRow("Opacity", bl.opacity, 0...1, reset: 1)
                        HStack(spacing: 6) {
                            Text("Link Q").font(.system(size: 10)).foregroundStyle(Ara.text2)
                            Spacer()
                            Toggle("", isOn: bl.linkQ)
                                .labelsHidden().controlSize(.mini).tint(Ara.accent)
                        }
                        .help("Gate this brush layer by the HSL qualifier matte")
                        HStack(spacing: 6) {
                            Text("Edge Aware").font(.system(size: 10)).foregroundStyle(Ara.text2)
                            Spacer()
                            Toggle("", isOn: bl.edgeAware)
                                .labelsHidden().controlSize(.mini).tint(Ara.accent)
                        }
                        .help("Strokes stick to the colour under their first dab — paint a sky without bleeding into buildings")
                        if bl.edgeAware.wrappedValue {
                            SliderRow("Tolerance", bl.edgeTol, 0.05...1.0, reset: 0.5)
                        }
                    }
                }
                if !recipe.spots.isEmpty || !edit.wrappedValue.lights.isEmpty || !recipe.clones.isEmpty {
                    ForEach(recipe.spots.indices, id: \.self) { i in
                        MarkRow("Spot \(i + 1)", icon: "bandage") { recipe.spots.remove(at: i) }
                    }
                    ForEach(recipe.clones.indices, id: \.self) { i in
                        MarkRow("Clone \(i + 1)", icon: "point.topleft.down.to.point.bottomright.curvepath") {
                            recipe.clones.remove(at: i)
                        }
                    }
                    ForEach(edit.wrappedValue.lights.indices, id: \.self) { i in
                        MarkRow("Light \(i + 1)  \(edit.wrappedValue.lights[i][3] >= 0 ? "+" : "")\(String(format: "%.1f", edit.wrappedValue.lights[i][3]))EV",
                                icon: "sun.max") {
                            edit.wrappedValue.lights.remove(at: i)
                        }
                    }
                }
            }
        }
    }

    private var detailPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Detail", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.detail) }
                    .help("Estimate noise in flat areas and fringing at edges")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Sharpen", $recipe.sharpen, 0...1)
                SliderRow("Noise", $recipe.noise_luma, 0...1)
                SliderRow("NR Chroma", $recipe.noise_chroma, 0...1)
                SliderRow("Dehaze", $recipe.dehaze, 0...1)
                SliderRow("Deband", $recipe.deband, 0...1)
                SliderRow("CA Fix", $recipe.ca_fix, 0...1)
                SliderRow("Beauty", $recipe.beauty, 0...1)
            }
            Panel("AI Denoise", trailing: {
                ToolChip(label: aidnReady ? "Redo" : "Prepare",
                         icon: "brain",
                         active: aidnBusy) { runAiDenoise() }
                    .help("Run the SCUNet denoise model on this photo (several minutes on CPU), then blend it in with Amount")
                    .opacity(aidnBusy ? 0.5 : 1)
            }) {
                HStack(spacing: 8) {
                    SliderRow("Amount", $recipe.ai_denoise, 0...1)
                        .opacity(aidnReady ? 1 : 0.4)
                }
                .disabled(!aidnReady)
                HStack(spacing: 6) {
                    if aidnBusy {
                        ProgressView().controlSize(.small)
                        Text("Preparing (several minutes on CPU)…")
                    } else if aidnReady {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(Ara.accent)
                        Text(aidnInfo.isEmpty ? "Denoised base ready" : aidnInfo)
                    } else {
                        Text("Runs the neural denoise once and caches the result next to the photo")
                    }
                }
                .font(.system(size: 10))
                .foregroundStyle(Ara.text3)
            }
        }
    }

    private var fxPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
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
                SliderRow("Fl Hue", $recipe.flare[3], 0...1, track: Ara.hueTrack)
            }
        }
    }

    private var xformPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Transform", trailing: {
                ToolChip(label: "Auto", icon: "wand.and.stars") { runAuto(.geometry) }
                    .help("Detect horizon tilt and converging verticals")
                    .opacity(autoBusy ? 0.5 : 1)
            }) {
                SliderRow("Straighten", $recipe.rotation_deg, -10...10, step: 0.1)
                SliderRow("Keystone V", $recipe.key_v, -0.4...0.4)
                SliderRow("Keystone H", $recipe.key_h, -0.4...0.4)
            }
            Panel("Lens") {
                HStack(spacing: 6) {
                    Image(systemName: lensProfile.isEmpty ? "camera.metering.unknown" : "checkmark.circle.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(lensProfile.isEmpty ? Ara.text3 : Ara.accent)
                    Text(lensProfile.isEmpty ? "No lens profile" : lensProfile)
                        .font(.system(size: 9.5))
                        .foregroundStyle(lensProfile.isEmpty ? Ara.text3 : Ara.text1)
                        .lineLimit(1).truncationMode(.middle)
                    Spacer()
                }
                .help(lensProfile.isEmpty
                      ? "This lens has no profile in the bundled lensfun DB"
                      : "lensfun profile: distortion + lateral CA + vignetting")
                SliderRow("Correction", $recipe.lens_corr, 0...1)
                    .disabled(lensProfile.isEmpty)
                    .opacity(lensProfile.isEmpty ? 0.45 : 1)
            }
            Panel("Crop") {
                HStack(spacing: 4) {
                    Text("Aspect")
                        .font(.system(size: 10.5, weight: .medium))
                        .foregroundStyle(Ara.text2)
                        .frame(width: 60, alignment: .leading)
                    ForEach([(name: "Free", k: 0.0), ("1:1", 1.0), ("4:3", 4.0 / 3.0),
                             ("3:2", 1.5), ("16:9", 16.0 / 9.0)], id: \.name) { a in
                        ToolChip(label: a.name) { applyAspect(a.k) }
                    }
                }
                SliderRow("Left", $recipe.crop[0], 0...0.45)
                SliderRow("Top", $recipe.crop[1], 0...0.45)
                SliderRow("Right", $recipe.crop[2], 0...0.45)
                SliderRow("Bottom", $recipe.crop[3], 0...0.45)
            }
        }
    }

    /// DaVinci grade versions / stills: named snapshots of the current recipe,
    /// persisted inside the sidecar file on Save.
    private var versionsPalette: some View {
        VStack(alignment: .leading, spacing: 10) {
            Panel("Versions", trailing: {
                ToolChip(label: "Save", icon: "plus") {
                    versionName = ""
                    showVersionName = true
                }
            }) {
                if versions.isEmpty {
                    Text("Save a named snapshot of the current grade, then click it " +
                         "to jump back. Versions persist inside the .araware.json sidecar.")
                        .font(.system(size: 10)).foregroundStyle(Ara.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(versions) { v in
                    VersionRow(v: v, active: v.recipe == recipe,
                               onApply: { recipe = v.recipe },
                               onDelete: { versions.removeAll { $0.id == v.id } })
                }
            }
            // DaVinci Gallery stills: app-wide grade library. Click applies
            // the still's recipe; a still can also act as the wipe reference.
            Panel("Grade Library", trailing: {
                ToolChip(label: "Save", icon: "plus") { saveStill() }
            }) {
                if refRecipe != nil {
                    HStack(spacing: 6) {
                        Image(systemName: "rectangle.split.2x1")
                            .font(.system(size: 10)).foregroundStyle(Ara.accent)
                        Text("Wipe ref: \(refName)")
                            .font(.system(size: 10)).foregroundStyle(Ara.text1)
                            .lineLimit(1)
                        Spacer()
                        Button("Clear") { clearReference() }
                            .font(.system(size: 10)).foregroundStyle(Ara.accent)
                            .buttonStyle(.plain)
                    }
                }
                if stills.isEmpty {
                    Text("Save the current grade as a reusable still — click one to apply it " +
                         "to this photo, or use it as the wipe reference.")
                        .font(.system(size: 10)).foregroundStyle(Ara.text3)
                        .fixedSize(horizontal: false, vertical: true)
                } else {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(stills) { st in
                                VStack(spacing: 3) {
                                    if let t = stillThumbs[st.id] {
                                        Image(nsImage: t)
                                            .resizable().aspectRatio(contentMode: .fill)
                                            .frame(width: 84, height: 56)
                                            .clipShape(RoundedRectangle(cornerRadius: 4))
                                            .overlay(RoundedRectangle(cornerRadius: 4)
                                                .stroke(Ara.border, lineWidth: 0.5))
                                    } else {
                                        RoundedRectangle(cornerRadius: 4)
                                            .fill(Ara.bg3)
                                            .frame(width: 84, height: 56)
                                            .overlay(ProgressView().controlSize(.mini))
                                    }
                                    Text(st.name)
                                        .font(.system(size: 8.5)).foregroundStyle(Ara.text2)
                                        .lineLimit(1)
                                        .frame(width: 84)
                                }
                                .contentShape(Rectangle())
                                .onTapGesture { applyStill(st) }
                                .contextMenu {
                                    Button("Apply") { applyStill(st) }
                                    Button("Use as wipe reference") { useAsReference(st) }
                                    Divider()
                                    Button("Rename…") { renameStill(st) }
                                    Button("Delete") { deleteStill(st) }
                                }
                                .help("Apply \"\(st.name)\" — right-click for more")
                            }
                        }
                        .padding(.vertical, 2)
                    }
                    .frame(height: 72)
                }
            }
            // darktable history stack: every undo step as a clickable row.
            Panel("Edit History") {
                if undoStack.isEmpty {
                    Text("Edits appear here as you make them — click a step to jump back.")
                        .font(.system(size: 10)).foregroundStyle(Ara.text3)
                        .fixedSize(horizontal: false, vertical: true)
                }
                ForEach(Array(undoStack.enumerated().reversed()), id: \.offset) { i, _ in
                    Button {
                        jumpToHistory(i)
                    } label: {
                        HStack(spacing: 7) {
                            Text(i == 0 ? "0" : "\(i)")
                                .font(.system(size: 9).monospacedDigit())
                                .foregroundStyle(Ara.text3)
                                .frame(width: 14, alignment: .trailing)
                            Text(i == 0 ? "Original" : "Edit \(i)")
                                .font(.system(size: 10.5))
                                .foregroundStyle(Ara.text2)
                            Spacer()
                        }
                        .padding(.horizontal, 8).padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                // current state marker — always the active row
                HStack(spacing: 7) {
                    Text("\(undoStack.count)")
                        .font(.system(size: 9).monospacedDigit())
                        .foregroundStyle(Ara.accent)
                        .frame(width: 14, alignment: .trailing)
                    Text("Current")
                        .font(.system(size: 10.5, weight: .semibold))
                        .foregroundStyle(Ara.text1)
                    Spacer()
                }
                .padding(.horizontal, 8).padding(.vertical, 4)
            }
        }
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
                    .help(status)
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

    /// .cube LUT file open panel; stores the path (parser rejects junk).
    private func pickLut() {
        let p = NSOpenPanel()
        p.canChooseFiles = true
        p.canChooseDirectories = false
        p.allowsMultipleSelection = false
        if let cube = UniformTypeIdentifiers.UTType(filenameExtension: "cube") {
            p.allowedContentTypes = [cube]
        }
        if p.runModal() == .OK, let u = p.url {
            edit.wrappedValue.lut_file = u.path
            if edit.wrappedValue.lut_amount == 0 { edit.wrappedValue.lut_amount = 1 }
        }
    }

    /// which suggestion subset a per-panel Auto chip applies
    private enum AutoScope { case all, tone, geometry, detail, zones, colour }

    /// engine-side analysis on a neutral preview, then apply the scope's
    /// suggestions (core/src/auto.rs). One analysis per click.
    private func runAuto(_ scope: AutoScope) {
        guard !autoBusy else { return }
        autoBusy = true
        status = "Analyzing…"
        let p = photo.path
        Task {
            let sug = await AraEngine.shared.work { $0.autoAnalyze(path: p) }
            await MainActor.run {
                autoBusy = false
                guard let s = sug else {
                    status = "Auto analysis failed: \(AraEngine.shared.lastError)"
                    return
                }
                switch scope {
                case .all:
                    recipe.wb_mode = .auto
                    edit.wrappedValue.auto_exposure = true
                    edit.wrappedValue.auto_contrast = true
                    recipe.rotation_deg = s.rotation_deg
                    recipe.key_v = s.key_v; recipe.key_h = s.key_h
                    recipe.noise_luma = s.noise_luma; recipe.noise_chroma = s.noise_chroma
                    recipe.ca_fix = s.ca_fix
                    recipe.dehaze = s.dehaze
                    edit.wrappedValue.vibrance = s.vibrance
                    edit.wrappedValue.zones_ev = s.zones_ev
                case .tone:
                    recipe.wb_mode = .auto
                    edit.wrappedValue.auto_exposure = true
                    edit.wrappedValue.auto_contrast = true
                case .geometry:
                    recipe.rotation_deg = s.rotation_deg
                    recipe.key_v = s.key_v; recipe.key_h = s.key_h
                case .detail:
                    recipe.noise_luma = s.noise_luma; recipe.noise_chroma = s.noise_chroma
                    recipe.ca_fix = s.ca_fix
                    recipe.dehaze = s.dehaze
                case .zones:
                    edit.wrappedValue.zones_ev = s.zones_ev
                case .colour:
                    edit.wrappedValue.vibrance = s.vibrance
                }
                status = autoSummary(s, scope)
            }
        }
    }

    /// status line describing what THIS chip actually applied (not the full
    /// suggestion, which may include fields other scopes own)
    private func autoSummary(_ s: AutoSuggestion, _ scope: AutoScope) -> String {
        var parts: [String] = []
        switch scope {
        case .all:
            return "Auto: \(s.summary)"
        case .tone:
            return "Auto: WB + exposure + contrast"
        case .geometry:
            if s.rotation_deg != 0 { parts.append(String(format: "straighten %+.1f°", s.rotation_deg)) }
            if s.key_v != 0 { parts.append(String(format: "keystone %+.2f", s.key_v)) }
        case .detail:
            if s.noise_luma > 0 { parts.append(String(format: "NR %.2f (σ=%.1f)", s.noise_luma, s.noise_sigma)) }
            if s.ca_fix > 0 { parts.append(String(format: "CA %.2f", s.ca_fix)) }
            if s.dehaze > 0 { parts.append(String(format: "dehaze %.2f", s.dehaze)) }
        case .zones:
            if s.zones_ev.contains(where: { $0 != 0 }) { parts.append("zone EQ set") }
        case .colour:
            if s.vibrance != 0 { parts.append(String(format: "vibrance %+.2f", s.vibrance)) }
        }
        return parts.isEmpty ? "Auto: nothing to correct" : "Auto: \(parts.joined(separator: " · "))"
    }

    private func applyAspect(_ k: Double) {
        if k == 0 { recipe.crop = [0, 0, 0, 0]; return }
        guard let img = image else { return }
        let remW = 1 - recipe.crop[0] - recipe.crop[2]
        let remH = 1 - recipe.crop[1] - recipe.crop[3]
        guard remW > 0.02, remH > 0.02 else { return }
        // frame aspect ≈ dst aspect corrected back for the current crop
        let fa = (Double(img.width) / Double(img.height)) * remH / remW
        let cx = recipe.crop[0] + remW / 2
        let cy = recipe.crop[1] + remH / 2
        var w = remW
        var h = w * fa / k
        if h > remH { h = remH; w = h * k / fa }
        w = min(w, 1); h = min(h, 1)
        let l = (cx - w / 2).clamped(to: 0...1)
        let t = (cy - h / 2).clamped(to: 0...1)
        recipe.crop = [l, t,
                       (1 - (l + w)).clamped(to: 0...1),
                       (1 - (t + h)).clamped(to: 0...1)]
    }

    private func curveBinding(_ ch: Int) -> Binding<[[Double]]> {
        switch ch {
        case 0: return edit.curve
        case 1: return edit.curve_r
        case 2: return edit.curve_g
        case 3: return edit.curve_b
        case 4: return edit.hue_hue
        case 5: return edit.hue_sat
        case 6: return edit.hue_lum
        case 7: return edit.lum_sat
        default: return edit.sat_sat
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
        let sc = AraEngine.shared.sidecar(path: photo.path, vslot: photo.vslot)
        baseline = sc.recipe
        baselineVersions = sc.versions
        versions = store.unsavedVersions[photo.id] ?? sc.versions
        recipe = store.unsavedEdits[photo.id] ?? sc.recipe
        selStage = 0
        selWindow = nil
        rating = sc.rating
        label = sc.label
        flag = sc.flag
        loadedRating = sc.rating
        loadedLabel = sc.label
        image = nil
        baselineImg = nil
        clearReference()
        undoStack.removeAll()
        redoStack.removeAll()
        cmp = .off
        zoom = 1
        pan = .zero
        lensProfile = ""
        liveFrame = []
        selBrush = 0
        aidnBusy = false
        aidnReady = false
        aidnInfo = ""
        subjBusy = false
        subjReady = false
        // .insp stills are always equirect — open straight into 360 mode
        panoMode = photo.name.lowercased().hasSuffix(".insp")
        if stills.isEmpty { loadGallery() }
        Task {
            let meta = await AraEngine.shared.work { $0.metadata(path: photo.path) }
            if let d = meta.data(using: .utf8),
               let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] {
                lensProfile = (j["lens_profile"] as? String) ?? ""
            }
            aidnReady = await AraEngine.shared.work { $0.aiDenoiseReady(path: photo.path) }
            subjReady = await AraEngine.shared.work { $0.aiSubjectReady(path: photo.path) }
        }
        rerender()
    }

    /// U-2-Net subject matte (engine src/ai.rs): prepares the mask PNG once,
    /// then adds a 'subject' power window carrying ev/sat/temp.
    private func runSubjectMask() {
        guard !subjBusy else { return }
        let p = photo.path
        // matte already cached: just add the window
        if subjReady {
            addSubjectWindow()
            return
        }
        subjBusy = true
        Task {
            let res = await AraEngine.shared.work { $0.aiSubjectPrepare(path: p) }
            subjBusy = false
            if res.ok {
                subjReady = true
                status = String(format: "Subject matte ready (%.1fs)", Double(res.ms) / 1000.0)
                addSubjectWindow()
            } else {
                status = "Subject detect failed: \(res.error)"
            }
        }
    }

    private func addSubjectWindow() {
        var w = PowerWindow()
        w.kind = "subject"
        w.p = [0, 0, 0, 0, 0, 0]
        w.ev = 0.6   // sensible starting lift so the mask is visible
        edit.wrappedValue.windows.append(w)
        selWindow = w.id
        status = "Subject window added — tweak EV/Sat/Temp"
    }

    /// SCUNet denoise pre-pass (engine src/ai.rs): bakes `<photo>.araware.aidn.jpg`
    /// from the current recipe's linear base, then the Amount slider blends it.
    private func runAiDenoise() {
        guard !aidnBusy else { return }
        aidnBusy = true
        let p = photo.path
        let r = recipe
        Task {
            let res = await AraEngine.shared.work { $0.aiDenoisePrepare(path: p, recipe: r) }
            aidnBusy = false
            if res.ok {
                aidnReady = true
                aidnInfo = String(format: "Denoised %dx%d in %.1f min", res.w, res.h, Double(res.ms) / 60000.0)
                status = "AI denoise ready"
                if recipe.ai_denoise == 0 { recipe.ai_denoise = 0.5 }
            } else {
                aidnInfo = ""
                status = "AI denoise failed: \(res.error)"
            }
            rerender()
        }
    }

    private func save() {
        // start from the on-disk sidecar so flag/keywords survive a save
        var sc = AraEngine.shared.sidecar(path: photo.path, vslot: photo.vslot)
        sc.rating = rating
        sc.label = label
        sc.recipe = recipe
        sc.versions = versions
        let stem = URL(fileURLWithPath: photo.path).deletingPathExtension().lastPathComponent
        let ok = AraEngine.shared.writeSidecar(path: photo.path, vslot: photo.vslot, sc)
        status = ok ? "Saved \(stem).araware.json"
                    : "Save failed: \(AraEngine.shared.lastError)"
        if ok {
            baseline = recipe
            baselineVersions = versions
            baselineImg = nil   // re-render compare base with the saved recipe
            clearReference()
            store.unsavedEdits.removeValue(forKey: photo.id)
            store.unsavedVersions.removeValue(forKey: photo.id)
            dirty = false
        }
    }

    // MARK: undo / redo

    /// Called from the recipe onChange with the pre-edit value. Edits within
    /// 0.8s of the previous change coalesce into one undo step (slider drags).
    private func recordUndo(_ old: Recipe) {
        if applyingHistory { return }
        let now = Date()
        if now.timeIntervalSince(lastEditTime) > 0.8 {
            undoStack.append(old)
            if undoStack.count > 80 { undoStack.removeFirst() }
        }
        lastEditTime = now
        redoStack.removeAll()
    }

    private func undo() {
        guard let prev = undoStack.popLast() else { status = "Nothing to undo"; return }
        applyingHistory = true   // cleared by the next recipe onChange
        redoStack.append(recipe)
        recipe = prev
        status = "Undo"
    }

    private func redo() {
        guard let next = redoStack.popLast() else { status = "Nothing to redo"; return }
        applyingHistory = true
        undoStack.append(recipe)
        recipe = next
        status = "Redo"
    }

    // MARK: compare baseline render

    /// Render the saved-state image once for wipe/before/diff modes.
    /// When a library still is chosen as wipe reference, render that instead.
    private func ensureBaseline() {
        if baselineImg != nil { return }
        let r = refRecipe ?? baseline
        Task.detached { [path = photo.path] in
            let img = await AraEngine.shared.work { $0.render(path: path, recipe: r, maxPx: 1400).0 }
            await MainActor.run { baselineImg = img }
        }
    }

    // MARK: grade library (~/.araware/gallery)

    private var galleryDir: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".araware/gallery", isDirectory: true)
    }
    private var galleryFile: URL { galleryDir.appendingPathComponent("stills.json") }

    private func loadGallery() {
        try? FileManager.default.createDirectory(at: galleryDir, withIntermediateDirectories: true)
        if let d = try? Data(contentsOf: galleryFile),
           let s = try? JSONDecoder().decode([GradeStill].self, from: d) {
            stills = s
        }
        // lazy thumb load — read <id>.jpg files that exist
        for st in stills where stillThumbs[st.id] == nil {
            if let img = NSImage(contentsOf: galleryDir.appendingPathComponent("\(st.id.uuidString).jpg")) {
                stillThumbs[st.id] = img
            }
        }
    }

    private func saveGallery() {
        if let d = try? JSONEncoder().encode(stills) {
            try? d.write(to: galleryFile, options: .atomic)
        }
    }

    private func saveStill() {
        var st = GradeStill(name: "", recipe: recipe, saved: Date().timeIntervalSince1970)
        st.name = "Still \(stills.count + 1)"
        stills.append(st)
        saveGallery()
        // render the thumbnail in the background
        let path = photo.path, r = recipe, id = st.id, dir = galleryDir
        Task.detached {
            let img = await AraEngine.shared.work { $0.render(path: path, recipe: r, maxPx: 280).0 }
            var thumb: NSImage?
            if let img {
                let rep = NSBitmapImageRep(cgImage: img)
                if let d = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]) {
                    try? d.write(to: dir.appendingPathComponent("\(id.uuidString).jpg"))
                    thumb = NSImage(data: d)
                }
            }
            await MainActor.run { if let thumb { stillThumbs[id] = thumb } }
        }
        status = "Saved \"\(st.name)\" to grade library"
    }

    private func applyStill(_ st: GradeStill) {
        recipe = st.recipe
        status = "Applied \"\(st.name)\""
    }

    private func useAsReference(_ st: GradeStill) {
        refRecipe = st.recipe
        refName = st.name
        baselineImg = nil
        if cmp == .off { cmp = .wipeV }
        ensureBaseline()
        status = "Wipe reference: \(st.name)"
    }

    private func clearReference() {
        refRecipe = nil; refName = ""; baselineImg = nil
    }

    private func renameStill(_ st: GradeStill) {
        let a = NSAlert()
        a.messageText = "Rename still"
        let tf = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 22))
        tf.stringValue = st.name
        a.accessoryView = tf
        a.addButton(withTitle: "Rename")
        a.addButton(withTitle: "Cancel")
        if a.runModal() == .alertFirstButtonReturn {
            let n = tf.stringValue.trimmingCharacters(in: .whitespaces)
            if !n.isEmpty, let i = stills.firstIndex(where: { $0.id == st.id }) {
                stills[i].name = n
                saveGallery()
            }
        }
    }

    private func deleteStill(_ st: GradeStill) {
        stills.removeAll { $0.id == st.id }
        stillThumbs.removeValue(forKey: st.id)
        try? FileManager.default.removeItem(
            at: galleryDir.appendingPathComponent("\(st.id.uuidString).jpg"))
        saveGallery()
        if refName == st.name { clearReference() }
    }

    // MARK: qualifier picking

    /// Read a 5×5 mean pixel from the displayed render at dst-normalized coords.
    private func samplePixel(nx: Double, ny: Double) -> (r: Double, g: Double, b: Double)? {
        guard let image, let provider = image.dataProvider,
              let cf = provider.data else { return nil }
        let data = cf as Data
        let w = image.width, h = image.height
        let px = min(max(Int(nx * Double(w)), 0), w - 1)
        let py = min(max(Int(ny * Double(h)), 0), h - 1)
        var r = 0.0, g = 0.0, b = 0.0, n = 0
        for dy in -2...2 {
            for dx in -2...2 {
                let x = px + dx, y = py + dy
                guard x >= 0, x < w, y >= 0, y < h else { continue }
                let o = (y * w + x) * 4
                if o + 2 < data.count {
                    r += Double(data[o]); g += Double(data[o + 1]); b += Double(data[o + 2]); n += 1
                }
            }
        }
        guard n > 0 else { return nil }
        return (r / Double(n) / 255, g / Double(n) / 255, b / Double(n) / 255)
    }

    private func rgbHsv(_ c: (Double, Double, Double)) -> (h: Double, s: Double, v: Double) {
        let (r, g, b) = c
        let mx = max(r, g, b), mn = min(r, g, b), d = mx - mn
        var h = 0.0
        if d > 1e-6 {
            if mx == r { h = ((g - b) / d).truncatingRemainder(dividingBy: 6) }
            else if mx == g { h = (b - r) / d + 2 }
            else { h = (r - g) / d + 4 }
            h /= 6
            if h < 0 { h += 1 }
        }
        return (h, mx > 1e-6 ? d / mx : 0, mx)
    }

    private func hueDist(_ a: Double, _ b: Double) -> Double {
        let d = abs(a - b)
        return min(d, 1 - d)
    }

    /// Apply an eyedropper sample to the qualifier ranges.
    private func qualifierPick(_ c: (Double, Double, Double), mode: String) {
        let (hh, ss, _) = rgbHsv(c)
        let l = 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2
        switch mode {
        case "qpick":
            edit.wrappedValue.qh = [hh, 0.08, 0.08]
            edit.wrappedValue.qs = [max(0, ss - 0.18), min(1, ss + 0.18), 0.12]
            edit.wrappedValue.ql = [max(0, l - 0.28), min(1, l + 0.28), 0.18]
            edit.wrappedValue.q_enabled = true
            status = String(format: "Keyed h=%.2f s=%.2f l=%.2f", hh, ss, l)
        case "qadd":
            edit.wrappedValue.qh[1] = max(edit.wrappedValue.qh[1], hueDist(hh, edit.wrappedValue.qh[0]) + 0.03)
            edit.wrappedValue.qs[0] = min(edit.wrappedValue.qs[0], ss)
            edit.wrappedValue.qs[1] = max(edit.wrappedValue.qs[1], ss)
            edit.wrappedValue.ql[0] = min(edit.wrappedValue.ql[0], l)
            edit.wrappedValue.ql[1] = max(edit.wrappedValue.ql[1], l)
        case "qsub":
            if hueDist(hh, edit.wrappedValue.qh[0]) < edit.wrappedValue.qh[1] {
                edit.wrappedValue.qh[1] = max(0.004, hueDist(hh, edit.wrappedValue.qh[0]) - 0.02)
            }
            if ss > edit.wrappedValue.qs[0] && ss < edit.wrappedValue.qs[1] {
                if ss - edit.wrappedValue.qs[0] < edit.wrappedValue.qs[1] - ss {
                    edit.wrappedValue.qs[0] = min(1, ss + 0.02)
                } else {
                    edit.wrappedValue.qs[1] = max(0, ss - 0.02)
                }
            }
            if l > edit.wrappedValue.ql[0] && l < edit.wrappedValue.ql[1] {
                if l - edit.wrappedValue.ql[0] < edit.wrappedValue.ql[1] - l {
                    edit.wrappedValue.ql[0] = min(1, l + 0.02)
                } else {
                    edit.wrappedValue.ql[1] = max(0, l - 0.02)
                }
            }
        default: break
        }
        edit.wrappedValue.q_enabled = true
    }

    // MARK: palette bookkeeping

    /// Amber-dot indicator: does this palette hold non-default values?
    private func paletteDirty(_ p: Palette) -> Bool {
        let d = Recipe()
        switch p {
        case .quick:
            return edit.wrappedValue.exposure != d.exposure || edit.wrappedValue.contrast != d.contrast
                || edit.wrappedValue.highlights != d.highlights || edit.wrappedValue.shadows != d.shadows
                || edit.wrappedValue.whites != d.whites || edit.wrappedValue.blacks != d.blacks
                || recipe.temperature != d.temperature || recipe.tint != d.tint
                || recipe.wb_mode != d.wb_mode || recipe.wb_pick != d.wb_pick
                || edit.wrappedValue.saturation != d.saturation || edit.wrappedValue.vibrance != d.vibrance
                || recipe.clarity != d.clarity || recipe.rotation_deg != d.rotation_deg
                || edit.wrappedValue.auto_exposure || edit.wrappedValue.auto_contrast
        case .light:
            return edit.wrappedValue.exposure != d.exposure || edit.wrappedValue.contrast != d.contrast
                || edit.wrappedValue.highlights != d.highlights || edit.wrappedValue.shadows != d.shadows
                || edit.wrappedValue.whites != d.whites || edit.wrappedValue.blacks != d.blacks
                || recipe.temperature != d.temperature || recipe.tint != d.tint
                || recipe.wb_mode != d.wb_mode || recipe.wb_pick != d.wb_pick
                || edit.wrappedValue.saturation != d.saturation || edit.wrappedValue.vibrance != d.vibrance
                || edit.wrappedValue.auto_exposure || edit.wrappedValue.auto_contrast
                || edit.wrappedValue.pivot != d.pivot
                || edit.wrappedValue.highlight_rolloff != d.highlight_rolloff
                || edit.wrappedValue.shadow_rolloff != d.shadow_rolloff
        case .wheels:
            return edit.wrappedValue.lift != d.lift || edit.wrappedValue.gamma != d.gamma
                || edit.wrappedValue.gain != d.gain || edit.wrappedValue.offset != d.offset
                || edit.wrappedValue.shadow_sat != d.shadow_sat || edit.wrappedValue.midtone_sat != d.midtone_sat
                || edit.wrappedValue.highlight_sat != d.highlight_sat
                || edit.wrappedValue.shadow_hue != d.shadow_hue || edit.wrappedValue.midtone_hue != d.midtone_hue
                || edit.wrappedValue.highlight_hue != d.highlight_hue || !edit.wrappedValue.look.isEmpty
        case .curves:
            return !edit.wrappedValue.curve.isEmpty || !edit.wrappedValue.curve_r.isEmpty
                || !edit.wrappedValue.curve_g.isEmpty || !edit.wrappedValue.curve_b.isEmpty
                || !edit.wrappedValue.hue_hue.isEmpty || !edit.wrappedValue.hue_sat.isEmpty
                || !edit.wrappedValue.hue_lum.isEmpty || !edit.wrappedValue.lum_sat.isEmpty
                || !edit.wrappedValue.sat_sat.isEmpty
        case .zones:
            return edit.wrappedValue.z_dark != d.z_dark || edit.wrappedValue.z_shadow != d.z_shadow
                || edit.wrappedValue.z_light != d.z_light || edit.wrappedValue.z_global != d.z_global
                || edit.wrappedValue.zones_ev != d.zones_ev
        case .slice:
            return edit.wrappedValue.color_slice != d.color_slice
        case .warp:
            return !edit.wrappedValue.warper.isEmpty
        case .qualifier:
            return edit.wrappedValue.q_enabled || edit.wrappedValue.q_show
        case .windows:
            return !edit.wrappedValue.windows.isEmpty
        case .mixer:
            return edit.wrappedValue.mixer != d.mixer || edit.wrappedValue.mono != d.mono
        case .retouch:
            return !recipe.spots.isEmpty || !edit.wrappedValue.lights.isEmpty || !recipe.clones.isEmpty
                || !edit.wrappedValue.brushes.isEmpty
        case .detail:
            return recipe.sharpen != 0 || recipe.noise_luma != 0 || recipe.noise_chroma != d.noise_chroma
                || recipe.dehaze != 0 || recipe.deband != 0 || recipe.ca_fix != 0 || recipe.beauty != 0
        case .fx:
            return recipe.clarity != 0 || recipe.vignette != 0 || recipe.grain != 0
                || recipe.glow != 0 || recipe.flare[2] != 0
        case .xform:
            return recipe.rotation_deg != 0 || recipe.crop != d.crop
                || recipe.key_v != 0 || recipe.key_h != 0
                || recipe.lens_corr != d.lens_corr
        case .meters, .versions:
            return false
        }
    }

    /// Reset every field owned by a palette (DaVinci palette reset).
    private func resetPalette(_ p: Palette) {
        let d = Recipe()
        switch p {
        case .quick:
            edit.wrappedValue.exposure = d.exposure; edit.wrappedValue.contrast = d.contrast
            edit.wrappedValue.highlights = d.highlights; edit.wrappedValue.shadows = d.shadows
            edit.wrappedValue.whites = d.whites; edit.wrappedValue.blacks = d.blacks
            recipe.temperature = d.temperature; recipe.tint = d.tint
            recipe.wb_mode = d.wb_mode; recipe.wb_pick = d.wb_pick
            edit.wrappedValue.saturation = d.saturation; edit.wrappedValue.vibrance = d.vibrance
            recipe.clarity = d.clarity; recipe.rotation_deg = d.rotation_deg
            edit.wrappedValue.auto_exposure = false; edit.wrappedValue.auto_contrast = false
        case .light:
            edit.wrappedValue.exposure = d.exposure; edit.wrappedValue.contrast = d.contrast
            edit.wrappedValue.highlights = d.highlights; edit.wrappedValue.shadows = d.shadows
            edit.wrappedValue.whites = d.whites; edit.wrappedValue.blacks = d.blacks
            recipe.temperature = d.temperature; recipe.tint = d.tint
            recipe.wb_mode = d.wb_mode; recipe.wb_pick = d.wb_pick
            edit.wrappedValue.saturation = d.saturation; edit.wrappedValue.vibrance = d.vibrance
            edit.wrappedValue.auto_exposure = false; edit.wrappedValue.auto_contrast = false
            edit.wrappedValue.pivot = d.pivot
            edit.wrappedValue.highlight_rolloff = d.highlight_rolloff
            edit.wrappedValue.shadow_rolloff = d.shadow_rolloff
        case .wheels:
            edit.wrappedValue.lift = d.lift; edit.wrappedValue.gamma = d.gamma; edit.wrappedValue.gain = d.gain
            edit.wrappedValue.offset = d.offset
            edit.wrappedValue.shadow_hue = d.shadow_hue; edit.wrappedValue.shadow_sat = d.shadow_sat
            edit.wrappedValue.midtone_hue = d.midtone_hue; edit.wrappedValue.midtone_sat = d.midtone_sat
            edit.wrappedValue.highlight_hue = d.highlight_hue; edit.wrappedValue.highlight_sat = d.highlight_sat
            edit.wrappedValue.look = ""
        case .curves:
            edit.wrappedValue.curve = []; edit.wrappedValue.curve_r = []; edit.wrappedValue.curve_g = []; edit.wrappedValue.curve_b = []
            edit.wrappedValue.hue_hue = []; edit.wrappedValue.hue_sat = []; edit.wrappedValue.hue_lum = []
            edit.wrappedValue.lum_sat = []; edit.wrappedValue.sat_sat = []
        case .zones:
            edit.wrappedValue.z_dark = d.z_dark; edit.wrappedValue.z_shadow = d.z_shadow
            edit.wrappedValue.z_light = d.z_light; edit.wrappedValue.z_global = d.z_global
            edit.wrappedValue.zones_ev = d.zones_ev
            zoneImgMode = false
        case .qualifier:
            edit.wrappedValue.qh = d.qh; edit.wrappedValue.qs = d.qs; edit.wrappedValue.ql = d.ql; edit.wrappedValue.qadj = d.qadj
            edit.wrappedValue.q_invert = false; edit.wrappedValue.q_clean = d.q_clean; edit.wrappedValue.q_blur = 0
            edit.wrappedValue.q_show = false; edit.wrappedValue.q_enabled = false
        case .windows:
            edit.wrappedValue.windows = []
            selWindow = nil
        case .mixer:
            edit.wrappedValue.mixer = d.mixer; edit.wrappedValue.mono = d.mono
        case .retouch:
            recipe.spots = []; edit.wrappedValue.lights = []; recipe.clones = []
            edit.wrappedValue.brushes = []
            selBrush = 0
            pendingClone = nil
        case .detail:
            recipe.sharpen = 0; recipe.noise_luma = 0; recipe.noise_chroma = d.noise_chroma
            recipe.dehaze = 0; recipe.deband = 0; recipe.ca_fix = 0; recipe.beauty = 0
        case .fx:
            recipe.clarity = 0; recipe.vignette = 0; recipe.grain = 0
            recipe.glow = 0; recipe.flare = d.flare
        case .xform:
            recipe.rotation_deg = 0; recipe.crop = d.crop
            recipe.key_v = 0; recipe.key_h = 0
            recipe.lens_corr = d.lens_corr
        case .slice:
            edit.wrappedValue.color_slice = d.color_slice
        case .warp:
            edit.wrappedValue.warper = []
        case .meters, .versions:
            break
        }
        status = "Reset \(p.title)"
    }

    // MARK: navigation / ratings / versions

    private func stepPhoto(_ dir: Int) {
        // match by path — rating/label edits mutate Photo values and would
        // break a Hashable-equality lookup
        guard let i = store.filtered.firstIndex(where: { $0.id == photo.id }) else { return }
        let j = i + dir
        guard store.filtered.indices.contains(j) else { return }
        store.select(store.filtered[j])
    }

    private func setRating(_ r: Int) {
        rating = (rating == r) ? 0 : r
        if rating != loadedRating {
            loadedRating = rating
            store.setRating(path: photo.path, vslot: photo.vslot, rating)
        }
        status = "Rating \(rating)"
    }

    /// DaVinci "Apply Grade from One Clip Prior" (Cmd+=): copy the previous
    /// photo's recipe (its unsaved edits win over its sidecar).
    private func applyPrevRecipe() {
        guard let i = store.filtered.firstIndex(where: { $0.id == photo.id }), i > 0 else {
            status = "No previous photo"
            return
        }
        let prev = store.filtered[i - 1]
        let r = store.unsavedEdits[prev.id] ?? AraEngine.shared.sidecar(path: prev.path, vslot: prev.vslot).recipe
        recipe = r
        status = "Applied grade from \(prev.name)"
    }

    private func commitVersion() {
        let name = versionName.trimmingCharacters(in: .whitespaces)
        versions.append(GradeVersion(
            name: name.isEmpty ? "Version \(versions.count + 1)" : name,
            recipe: recipe))
        versionName = ""
        dirty = true
    }

    /// apply a saved version — goes through undo history like any edit
    private func applyVersion(_ v: GradeVersion) {
        recipe = v.recipe
        status = "Applied \(v.name)"
    }

    /// darktable history jump: restore the recipe snapshot at undo index i.
    private func jumpToHistory(_ i: Int) {
        guard undoStack.indices.contains(i) else { return }
        applyingHistory = true
        let target = undoStack[i]
        undoStack.removeSubrange(i...)
        redoStack.removeAll()
        recipe = target
        status = i == 0 ? "Back to Original" : "Back to edit \(i)"
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

    /// Downsampled sRGB copy of the current render for the cursor probe
    /// and the zone-EQ hover readout (160px wide, ~55KB).
    private func makeProbe(_ img: CGImage) {
        let pw = 160
        let ph = max(1, Int(160.0 * Double(img.height) / Double(img.width)))
        var buf = [UInt8](repeating: 0, count: pw * ph * 4)
        buf.withUnsafeMutableBytes { ptr in
            guard let ctx = CGContext(data: ptr.baseAddress, width: pw, height: ph,
                                      bitsPerComponent: 8, bytesPerRow: pw * 4,
                                      space: CGColorSpaceCreateDeviceRGB(),
                                      bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return }
            ctx.interpolationQuality = .medium
            ctx.draw(img, in: CGRect(x: 0, y: 0, width: pw, height: ph))
        }
        probeBuf = buf
        probeW = pw
        probeH = ph
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
        renderGen += 1
        let gen = renderGen
        let r = recipe
        Task.detached { [path = photo.path] in
            let (img, bins, wv, vc, ce) = await AraEngine.shared.work {
                $0.renderScopes(path: path, recipe: r, maxPx: 1400)
            }
            await MainActor.run {
                // drop stale results — a newer render already started
                guard gen == renderGen else { return }
                if let img { image = img; makeProbe(img) }
                hist = (0..<4).map { ch in Array(bins[(ch * 256)..<(ch * 256 + 256)]) }
                wave = wv
                vec = vc
                cie = ce
                rendering = false
            }
        }
    }

    private func export() {
        showExportSheet = true
    }

    /// run the export after the settings sheet picked format/resize/sharpen
    private func runExport() {
        let uti: UTType = expFormat == "PNG" ? .png : expFormat == "TIFF" ? .tiff : .jpeg
        let ext = expFormat == "PNG" ? "png" : expFormat == "TIFF" ? "tiff" : "jpg"
        let panel = NSSavePanel()
        panel.allowedContentTypes = [uti]
        panel.nameFieldStringValue = photo.name.replacingOccurrences(
            of: "." + (photo.name.split(separator: ".").last.map(String.init) ?? ""),
            with: ".\(ext)")
        guard panel.runModal() == .OK, let url = panel.url else { return }
        rendering = true
        status = "Exporting…"
        let r = recipe
        let opts: [String: Any] = [
            "long_edge": expResize ? Int(expLongEdge) : 0,
            "sharpen": expSharpen,
        ]
        let qual = expQuality
        let space = expSpace
        Task.detached { [path = photo.path] in
            let img = await AraEngine.shared.work { $0.exportOpts(path: path, recipe: r, opts: opts) }
            await MainActor.run {
                rendering = false
                guard let img else {
                    status = "Export failed: \(AraEngine.shared.lastError)"
                    return
                }
                let tagged = convertColorSpace(img, space)
                // encode to memory first: ImageIO never embeds the ICC
                // profile itself, so JPEG APP2 / PNG iCCP get injected below
                let mdata = NSMutableData()
                guard let dest2 = CGImageDestinationCreateWithData(
                    mdata, uti.identifier as CFString, 1, nil) else {
                    status = "Export failed"
                    return
                }
                var props: [CFString: Any] = [:]
                if uti == .jpeg { props[kCGImageDestinationLossyCompressionQuality] = qual }
                CGImageDestinationAddImage(dest2, tagged, props as CFDictionary)
                guard CGImageDestinationFinalize(dest2) else {
                    status = "Export failed"
                    return
                }
                var out = mdata as Data
                if let icc = ExportICC.icc(of: tagged) {
                    if uti == .jpeg { out = ExportICC.jpeg(out, icc: icc) }
                    if uti == .png { out = ExportICC.png(out, icc: icc) }
                }
                let ok = (try? out.write(to: url)) != nil
                status = ok ? "Exported \(url.lastPathComponent) (\(space))" : "Export failed"
            }
        }
    }

    /// sRGB engine output → tagged target profile (ColorSync conversion).
    private func convertColorSpace(_ img: CGImage, _ space: String) -> CGImage {
        guard let src = CGColorSpace(name: CGColorSpace.sRGB) else { return img }
        // engine emits sRGB-encoded pixels; tag first so the conversion
        // below sees the right source space
        let base = (img.colorSpace == src) ? img : (img.copy(colorSpace: src) ?? img)
        let named: CGColorSpace? = switch space {
        case "Display P3": CGColorSpace(name: CGColorSpace.displayP3)
        case "Adobe RGB": CGColorSpace(name: CGColorSpace.adobeRGB1998)
        case "ProPhoto": CGColorSpace(name: CGColorSpace.rommrgb)
        default: src
        }
        guard let named, named != src else { return base }
        // draw into a context tagged with the destination profile so
        // ColorSync converts the pixel values and the image is tagged
        guard let ctx = CGContext(
            data: nil, width: base.width, height: base.height,
            bitsPerComponent: 8, bytesPerRow: 0, space: named,
            bitmapInfo: (CGImageAlphaInfo.noneSkipLast.rawValue
                | CGImageByteOrderInfo.orderDefault.rawValue))
        else { return base }
        ctx.draw(base, in: CGRect(x: 0, y: 0, width: base.width, height: base.height))
        return ctx.makeImage() ?? base
    }

    /// LR-style export settings: format, resize, output sharpening.
    private var exportSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Export").font(.system(size: 13, weight: .semibold)).foregroundStyle(Ara.text1)
            HStack(spacing: 8) {
                Text("Format").font(.system(size: 10.5)).foregroundStyle(Ara.text2).frame(width: 78, alignment: .leading)
                SegPicker([("JPEG", "JPEG"), ("PNG", "PNG"), ("TIFF", "TIFF")],
                          selection: $expFormat)
            }
            if expFormat == "JPEG" {
                SliderRow("Quality", $expQuality, 0.5...1, reset: 0.92)
            }
            HStack(spacing: 8) {
                Toggle("", isOn: $expResize).labelsHidden().controlSize(.mini).tint(Ara.accent)
                Text("Resize to long edge").font(.system(size: 10.5)).foregroundStyle(Ara.text2)
                Spacer()
                TextField("", value: $expLongEdge, format: .number)
                    .textFieldStyle(.roundedBorder).font(.system(size: 10.5))
                    .frame(width: 72).disabled(!expResize)
                Text("px").font(.system(size: 10)).foregroundStyle(Ara.text3)
            }
            SliderRow("Sharpen", $expSharpen, 0...1)
            HStack(spacing: 8) {
                Text("Color Space").font(.system(size: 10.5)).foregroundStyle(Ara.text2).frame(width: 78, alignment: .leading)
                SegPicker([("sRGB", "sRGB"), ("Display P3", "P3"), ("Adobe RGB", "Adobe"), ("ProPhoto", "ProPhoto")],
                          selection: $expSpace)
            }
            Text("ICC profile is embedded in the file.").font(.system(size: 9)).foregroundStyle(Ara.text3)
            Text("Output sharpening is applied after resize.").font(.system(size: 9)).foregroundStyle(Ara.text3)
            HStack {
                Spacer()
                Button("Cancel") { showExportSheet = false }.buttonStyle(AraSecondaryButton())
                Button("Export…") {
                    showExportSheet = false
                    runExport()
                }.buttonStyle(AraPrimaryButton())
            }
        }
        .padding(16)
        .frame(width: 320)
        .background(Ara.bg1)
    }
}

/// horizontal filmstrip thumb used at the bottom of the stage
struct FilmCell: View {
    let photo: Photo
    let selected: Bool
    var dirty: Bool = false
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
            if dirty {
                VStack {
                    Spacer()
                    HStack {
                        Spacer()
                        Circle()
                            .fill(Ara.accent)
                            .frame(width: 7, height: 7)
                            .overlay(Circle().stroke(.black.opacity(0.6), lineWidth: 0.75))
                            .padding(4)
                    }
                }
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

/// Look preset chooser with live thumbnails: the popover renders each look
/// over the current recipe at thumbnail size, cached per photo (DaVinci LUT
/// gallery / darktable preset style).
struct LookPicker: View {
    @Binding var look: String
    @Binding var lutFile: String
    let photo: Photo
    let recipe: Recipe

    @State private var open = false
    @State private var thumbs: [String: CGImage] = [:]
    @State private var lutThumbs: [String: CGImage] = [:]
    @State private var lutFiles: [String] = []
    @State private var thumbPath = ""
    @State private var loading = false

    /// DaVinci LUTs folder: ~/.araware/luts scanned for .cube files
    private static func lutDir() -> URL {
        let d = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".araware/luts", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    private let creative: [(String, String)] = [
        ("teal_orange", "Teal & Orange"), ("film_fade", "Film Fade"),
        ("bleach", "Bleach Bypass"), ("noir", "Noir"), ("matte", "Matte"),
    ]

    /// camera-matching profiles for the file's make (LR "Camera Matching")
    private var cameraLooks: [(String, String)] {
        let cam = photo.camera.lowercased()
        if cam.contains("fujifilm") {
            return [("fuji_provia", "Provia"), ("fuji_velvia", "Velvia"),
                    ("fuji_astia", "Astia"), ("fuji_chrome", "Classic Chrome"),
                    ("fuji_acros", "Acros")]
        }
        if cam.contains("canon") {
            return [("canon_faithful", "Faithful"), ("canon_landscape", "Landscape"),
                    ("canon_portrait", "Portrait"), ("canon_mono", "Monochrome")]
        }
        if cam.contains("nikon") {
            return [("nikon_neutral", "Neutral"), ("nikon_vivid", "Vivid"),
                    ("nikon_portrait", "Portrait"), ("nikon_mono", "Monochrome")]
        }
        if cam.contains("sony") {
            return [("sony_neutral", "Neutral"), ("sony_vivid", "Vivid"),
                    ("sony_portrait", "Portrait"), ("sony_bw", "B&W")]
        }
        if cam.contains("olympus") || cam.contains("om system") {
            return [("oly_vivid", "Vivid"), ("oly_muted", "Muted"),
                    ("oly_mono", "Monochrome")]
        }
        if cam.contains("panasonic") {
            return [("pana_natural", "Natural"), ("pana_vivid", "Vivid"),
                    ("pana_mono", "Monochrome")]
        }
        if cam.contains("leica") {
            return [("leica_natural", "Natural"), ("leica_vivid", "Vivid"),
                    ("leica_bw", "B&W")]
        }
        return []
    }

    private var options: [(String, String)] { [("", "None")] + cameraLooks + creative }

    var body: some View {
        Button {
            if thumbPath != photo.path { thumbs = [:]; thumbPath = photo.path }
            open.toggle()
            if open { loadThumbs() }
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
        .buttonStyle(.plain)
        .popover(isPresented: $open, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                Text("LOOKS")
                    .font(.system(size: 9, weight: .semibold)).tracking(1.2)
                    .foregroundStyle(Ara.text3)
                if !cameraLooks.isEmpty {
                    Text("CAMERA MATCHING")
                        .font(.system(size: 7.5, weight: .semibold)).tracking(1)
                        .foregroundStyle(Ara.text3.opacity(0.7))
                }
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 3),
                          spacing: 8) {
                    ForEach(options, id: \.0) { v, name in
                        VStack(spacing: 3) {
                            ZStack {
                                Ara.bg3
                                if let img = thumbs[v] {
                                    Image(img, scale: 1, label: Text(name))
                                        .resizable().scaledToFill()
                                } else {
                                    ProgressView().controlSize(.mini).tint(Ara.text3)
                                }
                            }
                            .aspectRatio(1.5, contentMode: .fit)
                            .clipped()
                            Text(name)
                                .font(.system(size: 8, weight: v == look ? .semibold : .regular))
                                .foregroundStyle(v == look ? Ara.accent : Ara.text3)
                                .lineLimit(1)
                        }
                        .padding(4)
                        .background(Ara.bg2)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .overlay(RoundedRectangle(cornerRadius: 6)
                            .stroke(v == look ? Ara.accent : Ara.hairline,
                                    lineWidth: v == look ? 1.5 : 0.5))
                        .contentShape(Rectangle())
                        .onTapGesture { look = v; open = false }
                    }
                }
                // .cube LUTs (DaVinci LUTs panel): files in ~/.araware/luts
                Text("LUTS")
                    .font(.system(size: 7.5, weight: .semibold)).tracking(1)
                    .foregroundStyle(Ara.text3.opacity(0.7))
                if lutFiles.isEmpty {
                    Text("drop .cube files into ~/.araware/luts")
                        .font(.system(size: 8.5)).foregroundStyle(Ara.text3)
                }
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 3),
                          spacing: 8) {
                    ForEach(lutFiles, id: \.self) { f in
                        let name = URL(fileURLWithPath: f).deletingPathExtension().lastPathComponent
                        VStack(spacing: 3) {
                            ZStack {
                                Ara.bg3
                                if let img = lutThumbs[f] {
                                    Image(img, scale: 1, label: Text(name))
                                        .resizable().scaledToFill()
                                } else {
                                    ProgressView().controlSize(.mini).tint(Ara.text3)
                                }
                            }
                            .aspectRatio(1.5, contentMode: .fit)
                            .clipped()
                            Text(name)
                                .font(.system(size: 8, weight: f == lutFile ? .semibold : .regular))
                                .foregroundStyle(f == lutFile ? Ara.accent : Ara.text3)
                                .lineLimit(1)
                        }
                        .padding(4)
                        .background(Ara.bg2)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .overlay(RoundedRectangle(cornerRadius: 6)
                            .stroke(f == lutFile ? Ara.accent : Ara.hairline,
                                    lineWidth: f == lutFile ? 1.5 : 0.5))
                        .contentShape(Rectangle())
                        .onTapGesture { lutFile = (lutFile == f) ? "" : f; open = false }
                    }
                }
            }
            .padding(10)
            .frame(width: 300)
            .background(Ara.bg1)
        }
    }

    /// One render per look at 220px; engine cache makes this fast enough.
    /// Same for every .cube in ~/.araware/luts (applied over the base look).
    private func loadThumbs() {
        if loading { return }
        loading = true
        let path = photo.path
        let base = recipe
        let files = ((try? FileManager.default.contentsOfDirectory(atPath: Self.lutDir().path)) ?? [])
            .filter { $0.lowercased().hasSuffix(".cube") }
            .map { Self.lutDir().appendingPathComponent($0).path }
            .sorted()
        Task.detached {
            var out: [String: CGImage] = [:]
            for (v, _) in options {
                var r = base
                r.look = v
                let (img, _) = await AraEngine.shared.work {
                    $0.render(path: path, recipe: r, maxPx: 220)
                }
                if let img { out[v] = img }
            }
            var lout: [String: CGImage] = [:]
            for f in files {
                var r = base
                r.lut_file = f
                r.lut_amount = 1
                let (img, _) = await AraEngine.shared.work {
                    $0.render(path: path, recipe: r, maxPx: 220)
                }
                if let img { lout[f] = img }
            }
            await MainActor.run {
                thumbs.merge(out) { _, n in n }
                lutThumbs.merge(lout) { _, n in n }
                lutFiles = files
                loading = false
            }
        }
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
            Text(String(format: "%.2f  %.2f  %.2f", v[0], v[1], v[2]))
                .font(.system(size: 7.5).monospacedDigit())
                .foregroundStyle(
                    v == [center, center, center] ? Ara.text3 : Ara.accent.opacity(0.8))
                .lineLimit(1).minimumScaleFactor(0.8)
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
                TrackSlider(value: $z[0], range: 0...1, height: 12, track: Ara.hueTrack)
                TrackSlider(value: $z[1], range: 0...1, height: 12)
                TrackSlider(value: $z[3], range: -1...1, height: 12)
            }
        }
    }
}

/// curve editor: click to add, drag to move, double-click a point to delete.
/// `hist` (luma histogram, 256 bins) is drawn as a faint backdrop like DaVinci.
struct CurveEditor: View {
    @Binding var points: [[Double]]
    var tint: Color = .white
    var hist: [UInt32] = []
    /// darktable colour equalizer: rainbow spectrum along the input axis for
    /// the hue-vs-* curves
    var spectrum: Bool = false

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width, h = geo.size.height
            ZStack {
                if spectrum {
                    LinearGradient(colors: [
                        .red, .orange, .yellow, .green, .cyan, .blue,
                        Color(hue: 0.83, saturation: 0.85, brightness: 0.9), .red,
                    ], startPoint: .leading, endPoint: .trailing)
                    .opacity(0.20)
                } else {
                    Rectangle().fill(Color(red: 0.05, green: 0.05, blue: 0.065))
                }
                Canvas { ctx, size in
                    // luma histogram backdrop
                    if hist.count == 256 {
                        let maxv = Double(hist.max() ?? 1)
                        var hp = Path()
                        hp.move(to: .init(x: 0, y: size.height))
                        for x in 0..<256 {
                            let v = min(log1p(Double(hist[x])) / log1p(maxv + 1), 1)
                            hp.addLine(to: .init(x: CGFloat(x) / 255 * size.width,
                                                 y: size.height - v * size.height))
                        }
                        hp.addLine(to: .init(x: size.width, y: size.height))
                        hp.closeSubpath()
                        ctx.fill(hp, with: .color(.white.opacity(0.07)))
                    }
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
        // padded hit area per dot — same fix as Stars (bare 11px dots
        // silently missed taps landing in the inter-dot gaps)
        HStack(spacing: 2) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 11, height: 11)
                    .overlay(Circle().stroke(.white.opacity(0.9), lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.4)
                    .frame(width: 15, height: 16)
                    .contentShape(Rectangle())
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
        for l in edit.wrappedValue.lights {
            let cx = rect.minX + l[0] * rect.width
            let cy = rect.minY + l[1] * rect.height
            let r = l[2] * rect.width
            let path = Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
            let col: Color = l[3] >= 0 ? .yellow : .purple
            ctx.stroke(path, with: .color(col.opacity(0.9)), lineWidth: 1.5)
        }
        for w in edit.wrappedValue.windows {
            // DaVinci overlay: white-ish outline; the selected window is amber
            // and thicker; a disabled window is dimmed to a hairline.
            let sel = w.id == selWindow
            let col: Color = sel ? Ara.accent : .cyan
            let alpha: Double = w.enabled ? (sel ? 1.0 : 0.85) : 0.25
            let lw: CGFloat = sel ? 2.5 : 1.5
            if w.kind == "lum" {
                // luminance-range window has no geometry to draw
            } else if w.kind == "gradient" {
                var ln = Path()
                ln.move(to: .init(x: rect.minX + w.p[0] * rect.width, y: rect.minY + w.p[1] * rect.height))
                ln.addLine(to: .init(x: rect.minX + w.p[2] * rect.width, y: rect.minY + w.p[3] * rect.height))
                ctx.stroke(ln, with: .color(col.opacity(alpha)), lineWidth: lw)
                // direction tick: short perpendicular at midpoint
                let mx = (w.p[0] + w.p[2]) / 2, my = (w.p[1] + w.p[3]) / 2
                let dx = w.p[2] - w.p[0], dy = w.p[3] - w.p[1]
                let len = max((dx * dx + dy * dy).squareRoot(), 1e-4)
                let nx2 = -dy / len, ny2 = dx / len
                var tick = Path()
                tick.move(to: .init(x: rect.minX + mx * rect.width, y: rect.minY + my * rect.height))
                tick.addLine(to: .init(x: rect.minX + (mx + nx2 * 0.04) * rect.width,
                                       y: rect.minY + (my + ny2 * 0.04) * rect.height))
                ctx.stroke(tick, with: .color(col.opacity(alpha)), lineWidth: lw)
            } else {
                let cx = fx2sx(w.p[0])
                let cy = fy2sy(w.p[1])
                let rx = w.p[2] * rect.width / max(sw, 0.01)
                let ry = w.p[3] * rect.height / max(sh, 0.01)
                ctx.stroke(Ellipse().path(in: CGRect(x: cx - rx, y: cy - ry, width: rx * 2, height: ry * 2)),
                           with: .color(col.opacity(alpha)), lineWidth: lw)
                if sel {
                    // centre cross for the selected window
                    var cr = Path()
                    cr.move(to: .init(x: cx - 6, y: cy)); cr.addLine(to: .init(x: cx + 6, y: cy))
                    cr.move(to: .init(x: cx, y: cy - 6)); cr.addLine(to: .init(x: cx, y: cy + 6))
                    ctx.stroke(cr, with: .color(Ara.accent.opacity(0.9)), lineWidth: 1)
                }
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
        // adjustment-brush strokes: centreline + feather circles at the ends
        for (li, layer) in edit.wrappedValue.brushes.enumerated() {
            let sel = li == selBrush
            for st in layer.strokes where st.pts.count >= 2 {
                let col: Color = st.erase ? .red : (sel ? Ara.accent : .white)
                let alpha: Double = layer.enabled ? (sel ? 0.95 : 0.55) : 0.2
                var ln = Path()
                ln.move(to: .init(x: fx2sx(st.pts[0][0]), y: fy2sy(st.pts[0][1])))
                for pt in st.pts.dropFirst() {
                    ln.addLine(to: .init(x: fx2sx(pt[0]), y: fy2sy(pt[1])))
                }
                ctx.stroke(ln, with: .color(col.opacity(alpha)),
                           style: StrokeStyle(lineWidth: sel ? 1.8 : 1.1, dash: st.erase ? [4, 3] : []))
                if sel {
                    // feather extent at both ends (radius is a frame-height fraction)
                    let r = st.radius * rect.height / max(sh, 0.01)
                    for pt in [st.pts.first!, st.pts.last!] {
                        let cx = fx2sx(pt[0]), cy = fy2sy(pt[1])
                        ctx.stroke(Circle().path(in: CGRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2)),
                                   with: .color(col.opacity(alpha * 0.5)), lineWidth: 0.8)
                    }
                }
            }
        }
        // live (in-progress) stroke: brighter preview while painting
        if liveFrame.count >= 2 {
            var ln = Path()
            ln.move(to: .init(x: fx2sx(liveFrame[0][0]), y: fy2sy(liveFrame[0][1])))
            for pt in liveFrame.dropFirst() {
                ln.addLine(to: .init(x: fx2sx(pt[0]), y: fy2sy(pt[1])))
            }
            ctx.stroke(ln, with: .color((brushErase ? Color.red : Ara.accent).opacity(0.95)), lineWidth: 2)
        }
        // brush cursor: feather circle tracks the mouse in brush mode
        if retouchMode == "brush", stageHover {
            let r = brushRadius * rect.height / max(sh, 0.01)
            let c = lastHover
            ctx.stroke(Circle().path(in: CGRect(x: c.x - r, y: c.y - r, width: r * 2, height: r * 2)),
                       with: .color((brushErase ? Color.red : Color.white).opacity(0.8)), lineWidth: 1.2)
            ctx.stroke(Circle().path(in: CGRect(x: c.x - r * (1 - brushSoft), y: c.y - r * (1 - brushSoft),
                                                width: r * 2 * (1 - brushSoft), height: r * 2 * (1 - brushSoft))),
                       with: .color((brushErase ? Color.red : Color.white).opacity(0.4)), lineWidth: 0.8)
        }
        }
    }
}

/// one brush layer row: select / visibility / stroke count / delete
struct BrushLayerRow: View {
    let layer: BrushLayer
    let index: Int
    let selected: Bool
    let onSelect: () -> Void
    let onToggle: () -> Void
    let onDelete: () -> Void
    var body: some View {
        HStack(spacing: 6) {
            Button(action: onSelect) {
                HStack(spacing: 6) {
                    Image(systemName: "paintbrush.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(selected ? Ara.accent : Ara.text3)
                    Text("Layer \(index + 1)")
                        .font(.system(size: 10.5, weight: selected ? .semibold : .regular))
                        .foregroundStyle(selected ? Ara.text1 : Ara.text2)
                    Text("×\(layer.strokes.count)")
                        .font(.system(size: 9).monospacedDigit())
                        .foregroundStyle(Ara.text3)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            Button(action: onToggle) {
                Image(systemName: layer.enabled ? "eye" : "eye.slash")
                    .font(.system(size: 10))
                    .foregroundStyle(layer.enabled ? Ara.text2 : Ara.text3)
            }
            .buttonStyle(.plain)
            .help(layer.enabled ? "Hide layer" : "Show layer")
            Button(action: onDelete) {
                Image(systemName: "trash")
                    .font(.system(size: 10))
                    .foregroundStyle(Ara.text3)
            }
            .buttonStyle(.plain)
            .help("Delete layer")
        }
        .padding(.vertical, 2)
        .padding(.horizontal, 5)
        .background(RoundedRectangle(cornerRadius: 4)
            .fill(selected ? Ara.accent.opacity(0.12) : Color.clear)
            .overlay(RoundedRectangle(cornerRadius: 4)
                .stroke(selected ? Ara.accent.opacity(0.4) : Color.clear, lineWidth: 0.5)))
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

/// per-window editor rows: visibility eye (DaVinci power-window on/off),
/// invert, per-window opacity, geometry sliders. Selecting a row arms it
/// for dragging on the stage.
struct WindowRow: View {
    @Binding var w: PowerWindow
    var selected = false
    var onSelect: () -> Void = {}
    let onDelete: () -> Void
    var body: some View {
        VStack(spacing: 5) {
            HStack {
                Image(systemName: w.kind == "gradient" ? "rectangle.lefthalf.filled"
                        : w.kind == "lum" ? "circle.lefthalf.filled"
                        : w.kind == "subject" ? "person.crop.square" : "circle")
                    .font(.system(size: 9))
                    .foregroundStyle(selected ? Ara.accent : .cyan)
                Text(w.kind == "gradient" ? "Gradient" : w.kind == "lum" ? "Lum Range"
                        : w.kind == "subject" ? "Subject" : "Circle")
                    .font(.system(size: 10.5, weight: .medium)).foregroundStyle(Ara.text1)
                Spacer()
                // on/off eye (DaVinci per-window visibility)
                Button { w.enabled.toggle() } label: {
                    Image(systemName: w.enabled ? "eye" : "eye.slash")
                        .font(.system(size: 9.5))
                        .foregroundStyle(w.enabled ? Ara.text2 : Ara.text3)
                }
                .buttonStyle(.plain)
                .help("Window on/off")
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
            SliderRow("Opacity", $w.opacity, 0...1, reset: 1)
            if w.kind == "lum" {
                SliderRow("Lo", $w.p[0], 0...1)
                SliderRow("Hi", $w.p[1], 0...1, reset: 0.6)
                SliderRow("Lo Feather", $w.p[2], 0.01...0.4, reset: 0.1)
                SliderRow("Hi Feather", $w.p[3], 0.01...0.4, reset: 0.1)
            } else if w.kind == "subject" {
                EmptyView()
            } else {
                if w.kind == "circle" {
                    SliderRow("Size", $w.p[2], 0.02...0.6, reset: 0.15)
                    SliderRow("Ratio", $w.p[3], 0.02...0.6, reset: 0.15)
                    SliderRow("Rot", $w.p[4], -90...90)
                }
                SliderRow("Soft", $w.p[5], 0.02...1, reset: 0.4)
            }
            HStack(spacing: 6) {
                Text("Link Q").font(.system(size: 9.5)).foregroundStyle(Ara.text2)
                Toggle("", isOn: $w.linkQ).labelsHidden().controlSize(.mini).tint(Ara.accent)
                Text("gate by Qualifier matte")
                    .font(.system(size: 9)).foregroundStyle(Ara.text3)
                Spacer()
            }
        }
        .padding(8)
        .background(selected ? Ara.accentSoft.opacity(0.5) : Ara.bg3)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6)
            .stroke(selected ? Ara.accent.opacity(0.6) : Ara.hairline, lineWidth: 1))
        .contentShape(Rectangle())
        .onTapGesture { onSelect() }
        .opacity(w.enabled ? 1 : 0.65)
    }
}

/// DaVinci "stills"/versions row: click to apply that snapshot.
struct VersionRow: View {
    let v: GradeVersion
    var active = false
    var onApply: () -> Void = {}
    var onDelete: () -> Void = {}
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "photo.stack")
                .font(.system(size: 9))
                .foregroundStyle(active ? Ara.accent : Ara.text3)
            Text(v.name.isEmpty ? "Version" : v.name)
                .font(.system(size: 10.5, weight: active ? .semibold : .regular))
                .foregroundStyle(active ? Ara.accent : Ara.text1)
                .lineLimit(1)
            Spacer()
            Button { onDelete() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 9, weight: .bold))
                    .foregroundStyle(Ara.text3)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 5)
            .fill(active ? Ara.accentSoft : Ara.bg3))
        .contentShape(Rectangle())
        .onTapGesture { onApply() }
    }
}

/// Installs a local NSEvent monitor for bare-key shortcuts (digits, arrows,
/// B, W, Esc). Handler returns true when the key was consumed.
final class KeyMonitor {
    private var monitor: Any?
    var handler: (NSEvent) -> Bool = { _ in false }
    func install() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] ev in
            guard let self else { return ev }
            return self.handler(ev) ? nil : ev
        }
    }
    func uninstall() {
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

/// Scroll-wheel zoom on the stage (hover-gated in the handler).
/// Handler returns true to let the event pass through untouched.
final class ScrollMonitor {
    private var monitor: Any?
    var handler: (NSEvent) -> Bool = { _ in true }
    func install() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] ev in
            guard let self else { return ev }
            return self.handler(ev) ? ev : nil
        }
    }
    func uninstall() {
        if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
    }
}

struct Stars: View {
    @Binding var rating: Int
    var body: some View {
        // generous hit area per star — the bare 12pt glyph is ~4px wide and
        // swallowed most taps, silently leaving `rating` unchanged
        HStack(spacing: 0) {
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .font(.system(size: 12))
                    .foregroundStyle(i <= rating ? Ara.gold : Ara.text3)
                    .frame(width: 15, height: 16)
                    .contentShape(Rectangle())
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
        }
    }
}
