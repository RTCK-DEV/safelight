import SwiftUI
import AppKit
import ImageIO
import UniformTypeIdentifiers

struct LibraryView: View {
    @EnvironmentObject var store: LibraryStore
    @State private var showNewCollection = false
    @State private var showSmartSheet = false
    @FocusState private var gridFocused: Bool

    var body: some View {
        NavigationSplitView {
            sidebar
        } content: {
            VStack(spacing: 0) {
                filterStrip
                Group {
                    if store.scanning || store.merging {
                        ProgressView(store.merging ? "Merging…" : "Scanning…")
                            .tint(Theme.accent)
                            .foregroundStyle(Theme.text2)
                    } else if store.filtered.isEmpty {
                        emptyState
                    } else if store.surveying {
                        SurveyView()
                    } else {
                        grid
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .background(Theme.bg1)
            .navigationTitle(navTitle)
            .toolbar {
                ToolbarItemGroup {
                    Button { store.pickFolder() } label: {
                        Image(systemName: "folder.badge.plus")
                    }
                    .help("Open folder (⌘O)")
                    Button { store.refresh() } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                    .help("Rescan")
                    Button {
                        store.surveying.toggle()
                    } label: {
                        Image(systemName: "rectangle.split.2x1")
                            .foregroundStyle(store.surveying ? Theme.accent : Theme.text2)
                    }
                    .help("Survey/compare selected (2–4)")
                    .disabled(store.selection.count < 2 || store.selection.count > 4)
                }
            }
        } detail: {
            if let photo = store.active {
                EditorView(photo: photo)
                    .id(photo.id)
            } else {
                ZStack {
                    Theme.bg0.ignoresSafeArea()
                    VStack(spacing: 10) {
                        Image(systemName: "camera.aperture")
                            .font(.system(size: 34))
                            .foregroundStyle(Theme.text3)
                        Text("Select a photo")
                            .font(.system(size: 12, weight: .medium))
                            .foregroundStyle(Theme.text3)
                    }
                }
            }
        }
        .preferredColorScheme(.dark)
        .tint(Theme.accent)
        .sheet(isPresented: $showNewCollection) {
            CollectionNameSheet(title: "New Collection") { name in
                store.addCollection(name: name, smart: false, rules: "")
            }
        }
        .sheet(isPresented: $showSmartSheet) {
            SmartCollectionSheet()
        }
        .alert("Merge failed", isPresented: Binding(
            get: { !store.mergeError.isEmpty },
            set: { if !$0 { store.mergeError = "" } })) {
            Button("OK") { store.mergeError = "" }
        } message: {
            Text(store.mergeError).font(.system(size: 11))
        }
    }

    private var navTitle: String {
        if scopeIsCollection, let c = store.collections.first(where: { String($0.id) == store.scope }) {
            return c.name
        }
        switch store.scope {
        case "all": return "All Photos"
        case "folder": return store.folder?.lastPathComponent ?? "safelight"
        default: return store.scope
        }
    }

    private var scopeIsCollection: Bool {
        Int64(store.scope) != nil
    }

    // MARK: - sidebar

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            // hand-rolled rows — SwiftUI List(selection:) rows stop being
            // clickable once context menus/badges attach, so scope +
            // highlight are managed directly (same look, no List quirks).
            ScrollView {
                VStack(alignment: .leading, spacing: 1) {
                    sectionHeader("Library")
                    sideRow("folder", "Current Folder", "folder")
                    sideRow("photo.stack", "All Photos", "all")
                    ForEach(store.knownFolders, id: \.self) { f in
                        sideRow("externaldrive",
                                URL(fileURLWithPath: f).lastPathComponent,
                                "folder:" + f, dim: true)
                    }

                    sectionHeader("Collections")
                    ForEach(store.collections) { c in
                        sideRow(c.smart == 1 ? "sparkle.magnifyingglass" : "tray.full",
                                c.name, String(c.id), badge: c.count)
                            .contextMenu {
                                Button("Rename…") { renameCollection(c) }
                                if c.smart == 1 {
                                    Button("Edit Rules…") { editSmart(c) }
                                }
                                Divider()
                                Button("Delete", role: .destructive) {
                                    store.deleteCollection(c)
                                }
                            }
                    }
                    HStack(spacing: 8) {
                        Button { showNewCollection = true } label: {
                            Label("Collection", systemImage: "plus")
                        }
                        Button { showSmartSheet = true } label: {
                            Label("Smart", systemImage: "sparkles")
                        }
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Theme.accent)
                    .font(.system(size: 11))
                    .padding(.top, 4)
                }
                .padding(.horizontal, 6)
                .padding(.top, 4)
            }

            if let p = store.active {
                Divider().background(Theme.hairline)
                InfoCard(photo: p)
            }
        }
    }

    private func sectionHeader(_ t: String) -> some View {
        Text(t.uppercased())
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(Theme.text3)
            .padding(.horizontal, 8)
            .padding(.top, 10)
            .padding(.bottom, 3)
    }

    private func sideRow(_ icon: String, _ title: String, _ tag: String,
                         badge: Int = 0, dim: Bool = false) -> some View {
        let active = store.scope == tag
        return HStack(spacing: 7) {
            Image(systemName: icon)
                .font(.system(size: 10))
                .frame(width: 15)
                .foregroundStyle(active ? Theme.accent : (dim ? Theme.text3 : Theme.text2))
            Text(title)
                .font(.system(size: 11, weight: active ? .semibold : .regular))
                .foregroundStyle(active ? Theme.text1 : (dim ? Theme.text3 : Theme.text2))
                .lineLimit(1).truncationMode(.middle)
            Spacer()
            if badge > 0 {
                Text("\(badge)")
                    .font(.system(size: 9, weight: .semibold).monospacedDigit())
                    .foregroundStyle(.white)
                    .padding(.horizontal, 6).padding(.vertical, 1)
                    .background(active ? Theme.accent : Theme.bg4)
                    .clipShape(Capsule())
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(active ? Theme.accent.opacity(0.14) : Color.clear)
        .clipShape(RoundedRectangle(cornerRadius: 5))
        .contentShape(Rectangle())
        .onTapGesture {
            store.scope = tag
            if tag.hasPrefix("folder:") {
                store.open(URL(fileURLWithPath: String(tag.dropFirst(7))))
            }
        }
    }

    private func renameCollection(_ c: CollectionInfo) {
        let a = NSAlert()
        a.messageText = "Rename collection"
        let f = NSTextField(frame: NSRect(x: 0, y: 0, width: 220, height: 24))
        f.stringValue = c.name
        a.accessoryView = f
        a.addButton(withTitle: "Rename")
        a.addButton(withTitle: "Cancel")
        if a.runModal() == .alertFirstButtonReturn, !f.stringValue.isEmpty {
            store.renameCollection(c, name: f.stringValue)
        }
    }

    @State private var editingSmart: CollectionInfo?
    private func editSmart(_ c: CollectionInfo) {
        editingSmart = c
        showSmartSheet = true
    }

    // MARK: - filter strip

    private var filterStrip: some View {
        HStack(spacing: 10) {
            RatingFilter(rating: $store.minRating)
            FlagFilter(sel: $store.flagFilter)
            LabelFilter(label: $store.labelFilter)
            if !store.cameras.isEmpty {
                FilterMenu(title: "Camera", options: store.cameras, sel: $store.cameraFilter)
            }
            if !store.lenses.isEmpty {
                FilterMenu(title: "Lens", options: store.lenses, sel: $store.lensFilter)
            }
            if !store.keywords.isEmpty {
                FilterMenu(title: "Keyword", options: store.keywords, sel: $store.keywordFilter)
            }
            HStack(spacing: 4) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 9))
                    .foregroundStyle(Theme.text3)
                TextField("Search", text: $store.searchText)
                    .textFieldStyle(.plain)
                    .font(.system(size: 10))
                    .frame(width: 80)
            }
            .padding(.horizontal, 7).padding(.vertical, 3)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
            if store.selection.count > 1 {
                Text("\(store.selection.count) selected")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Theme.accent)
            }
            Spacer()
            SortMenu(sel: $store.sortKey)
            Text("\(store.filtered.count)/\(store.photos.count)")
                .font(.system(size: 10).monospacedDigit())
                .foregroundStyle(Theme.text3)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(Theme.bg1)
        .overlay(alignment: .bottom) { Theme.hairline.frame(height: 1) }
    }

    private var emptyState: some View {
        VStack(spacing: 14) {
            ZStack {
                Circle().fill(Theme.bg3).frame(width: 72, height: 72)
                Image(systemName: scopeIsCollection ? "tray" : "photo.on.rectangle.angled")
                    .font(.system(size: 28))
                    .foregroundStyle(Theme.accent)
            }
            Text(scopeIsCollection ? "No photos match"
                 : (store.folder == nil ? "Open a folder of RAW files" : "No photos match the filters"))
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.text2)
            if !scopeIsCollection {
                Button("Open Folder…") { store.pickFolder() }
                    .buttonStyle(SlPrimaryButton())
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: - grid

    private var grid: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 148), spacing: 10)], spacing: 10) {
                    ForEach(store.filtered) { photo in
                        cell(for: photo)
                    }
                }
                .padding(10)
            }
            .scrollIndicators(.visible)
            .onChange(of: store.activeId) { _, id in
                if let id { proxy.scrollTo(id, anchor: .center) }
            }
        }
        .focusable()
        .focused($gridFocused)
        .onKeyPress { key in gridKey(key) }
    }

    private func cell(for photo: Photo) -> some View {
        ThumbCell(photo: photo,
                  selected: store.selection.contains(photo.id),
                  active: store.activeId == photo.id,
                  stacked: photo.stack != 0 && !store.expandedStacks.contains(photo.stack),
                  stackCount: photo.stack != 0
                      ? store.filtered.filter { $0.stack == photo.stack }.count + hiddenMembers(photo)
                      : 0,
                  onRate: { store.setRating(path: photo.path, vslot: photo.vslot,
                                            (photo.rating == $0) ? 0 : $0) })
            .onTapGesture {
                // read live modifiers: plain=select, ⌘=toggle, ⇧=range
                let m = NSEvent.modifierFlags.intersection(.deviceIndependentFlagsMask)
                if m.contains(.command) { store.select(photo, additive: true) }
                else if m.contains(.shift) { store.select(photo, range: true) }
                else { store.select(photo) }
            }
            .contextMenu { contextMenu(for: photo) }
            .id(photo.id)
    }

    private func hiddenMembers(_ p: Photo) -> Int {
        // members hidden by a collapsed stack (grid shows only the cover)
        guard p.stack != 0, !store.expandedStacks.contains(p.stack) else { return 0 }
        return store.photos.filter { $0.stack == p.stack && $0.id != p.id }.count
    }

    @ViewBuilder
    private func contextMenu(for p: Photo) -> some View {
        // rating
        Menu("Rating") {
            ForEach(0...5, id: \.self) { r in
                Button(r == 0 ? "No stars" : String(repeating: "★", count: r)) {
                    store.forEachSelected { store.setRating(path: $0, vslot: $1, r) }
                }
            }
        }
        // flags
        Menu("Flag") {
            Button("Pick (P)") { store.forEachSelected { store.setFlag(path: $0, vslot: $1, 1) } }
            Button("Unflag (U)") { store.forEachSelected { store.setFlag(path: $0, vslot: $1, 0) } }
            Button("Reject (X)") { store.forEachSelected { store.setFlag(path: $0, vslot: $1, -1) } }
        }
        // labels
        Menu("Label") {
            Button("None") { store.forEachSelected { store.setLabel(path: $0, vslot: $1, "") } }
            ForEach(labelColors, id: \.name) { l in
                Button { store.forEachSelected { store.setLabel(path: $0, vslot: $1, l.name) } } label: {
                    Label(l.name, systemImage: "circle.fill")
                }
            }
        }
        Divider()
        // stacks
        if store.selection.count > 1 {
            Button("Group into Stack (G)") { store.stackSelection() }
        }
        if p.stack != 0 {
            Button("Expand Stack") { store.expandedStacks.insert(p.stack) }
            Button("Collapse Stack") { store.expandedStacks.remove(p.stack) }
            Button("Ungroup Stack") { store.unstackSelection() }
            Button("Set as Cover") { _ = store.eng.stackCover(p.path); store.refresh() }
        }
        Divider()
        // virtual copies
        Button("New Virtual Copy") { store.createVirtualCopy(of: p) }
        if p.isVariant {
            Button("Promote to Master") { store.promoteVirtualCopy(p) }
            Button("Delete Virtual Copy", role: .destructive) { store.deleteVirtualCopy(p) }
        }
        Divider()
        // collections
        let manual = store.collections.filter { $0.smart == 0 }
        if !manual.isEmpty {
            Menu("Add to Collection") {
                ForEach(manual) { c in
                    Button(c.name) { store.addSelectedToCollection(c) }
                }
            }
        }
        if scopeIsCollection, let cid = Int64(store.scope),
           let c = store.collections.first(where: { $0.id == cid }), c.smart == 0 {
            Button("Remove from “\(c.name)”") { store.removeSelectedFromCollection(c) }
        }
        Divider()
        Button("Show in Finder") {
            NSWorkspace.shared.selectFile(p.path, inFileViewerRootedAtPath: "")
        }
        if store.selection.count > 1 {
            Menu("Merge Selected") {
                Button("HDR (Exposure Fusion)") { mergeSelected("hdr") }
                Button("Focus Stack") { mergeSelected("focus") }
            }
        }
        Button("Export Selected…") { exportSelected() }
    }

    /// merge the selected photos at full res (each with its own sidecar),
    /// write a JPEG next to the first source, rescan to show it
    private func mergeSelected(_ mode: String) {
        let targets = store.selectedPhotos
        guard targets.count > 1 else { return }
        let first = targets[0]
        let paths = targets.map(\.path)
        Task.detached {
            await MainActor.run { store.merging = true }
            let img = await SafelightEngine.shared.work { $0.merge(paths: paths, mode: mode) }
            await MainActor.run {
                store.merging = false
                guard let img else {
                    store.mergeError = SafelightEngine.shared.lastError
                    return
                }
                let dir = URL(fileURLWithPath: first.path).deletingLastPathComponent()
                let stem = URL(fileURLWithPath: first.path).deletingPathExtension().lastPathComponent
                let url = dir.appendingPathComponent("\(stem)_\(mode).jpg")
                if let srgb = CGColorSpace(name: CGColorSpace.sRGB),
                   let tagged = img.copy(colorSpace: srgb) {
                    let mdata = NSMutableData()
                    if let dest = CGImageDestinationCreateWithData(
                        mdata, UTType.jpeg.identifier as CFString, 1, nil) {
                        CGImageDestinationAddImage(dest, tagged, [
                            kCGImageDestinationLossyCompressionQuality: 0.95,
                        ] as CFDictionary)
                        if CGImageDestinationFinalize(dest) {
                            var out = mdata as Data
                            if let icc = ExportICC.icc(of: tagged) {
                                out = ExportICC.jpeg(out, icc: icc)
                            }
                            try? out.write(to: url)
                        }
                    }
                }
                store.refresh()
            }
        }
    }

    private func exportSelected() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Export Here"
        guard panel.runModal() == .OK, let dir = panel.url else { return }
        let targets = store.selectedPhotos
        Task.detached {
            for p in targets {
                let sc = await SafelightEngine.shared.work { $0.sidecar(path: p.path, vslot: p.vslot) }
                let img = await SafelightEngine.shared.work { $0.export(path: p.path, recipe: sc.recipe) }
                if let img {
                    let stem = URL(fileURLWithPath: p.path).deletingPathExtension().lastPathComponent
                    let name = p.vslot > 0 ? "\(stem)_v\(p.vslot).jpg" : "\(stem).jpg"
                    let url = dir.appendingPathComponent(name)
                    await MainActor.run {
                        // encode via ImageIO then inject the sRGB ICC APP2 —
                        // macOS never writes the profile itself
                        if let srgb = CGColorSpace(name: CGColorSpace.sRGB),
                           let srgbImg = img.copy(colorSpace: srgb) {
                            let mdata = NSMutableData()
                            if let dest = CGImageDestinationCreateWithData(
                                mdata, UTType.jpeg.identifier as CFString, 1, nil) {
                                CGImageDestinationAddImage(dest, srgbImg, [
                                    kCGImageDestinationLossyCompressionQuality: 0.9,
                                ] as CFDictionary)
                                if CGImageDestinationFinalize(dest) {
                                    var out = mdata as Data
                                    if let icc = ExportICC.icc(of: srgbImg) {
                                        out = ExportICC.jpeg(out, icc: icc)
                                    }
                                    try? out.write(to: url)
                                }
                            }
                        } else {
                            let rep = NSBitmapImageRep(cgImage: img)
                            let data = rep.representation(using: .jpeg, properties: [.compressionFactor: 0.9])
                            try? data?.write(to: url)
                        }
                    }
                }
            }
        }
    }

    // MARK: - keys

    private func gridKey(_ key: KeyPress) -> KeyPress.Result {
        if let c = key.characters.first {
            switch c {
            case "p": store.forEachSelected { store.setFlag(path: $0, vslot: $1, 1) }; return .handled
            case "u": store.forEachSelected { store.setFlag(path: $0, vslot: $1, 0) }; return .handled
            case "x": store.forEachSelected { store.setFlag(path: $0, vslot: $1, -1) }; return .handled
            case "g": if store.selection.count > 1 { store.stackSelection() }; return .handled
            case "0","1","2","3","4","5":
                let r = Int(String(c)) ?? 0
                store.forEachSelected { store.setRating(path: $0, vslot: $1, r) }
                return .handled
            default: break
            }
        }
        switch key.key {
        case .leftArrow: store.moveSelection(-1); return .handled
        case .rightArrow: store.moveSelection(1); return .handled
        case .upArrow: store.moveSelection(-4); return .handled
        case .downArrow: store.moveSelection(4); return .handled
        case .escape:
            if store.surveying { store.surveying = false; return .handled }
            if store.selection.count > 1, let a = store.active {
                store.select(a) // collapse multi-selection to the active item
                return .handled
            }
            return .ignored
        default: return .ignored
        }
    }
}

// MARK: - survey / compare

/// side-by-side compare of 2–4 selected photos (LR Survey / C1 compare)
struct SurveyView: View {
    @EnvironmentObject var store: LibraryStore

    var body: some View {
        let sel = store.selectedPhotos.prefix(4)
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 340), spacing: 8)], spacing: 8) {
                ForEach(Array(sel)) { p in
                    SurveyTile(photo: p)
                        .onTapGesture {
                            store.setFlag(path: p.path, vslot: p.vslot, 1)
                        }
                }
            }
            .padding(8)
        }
        .background(Theme.bg0)
        .overlay(alignment: .top) {
            HStack {
                Image(systemName: "rectangle.split.2x1")
                    .foregroundStyle(Theme.accent)
                Text("Survey — click a tile to flag it Picked · Esc to exit")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.text2)
                Spacer()
                Button("Done") { store.surveying = false }
                    .buttonStyle(SlPrimaryButton())
            }
            .padding(.horizontal, 12).padding(.vertical, 7)
            .background(Theme.bg1.opacity(0.94))
            .overlay(alignment: .bottom) { Theme.hairline.frame(height: 1) }
        }
    }
}

struct SurveyTile: View {
    let photo: Photo
    @State private var image: CGImage?

    var body: some View {
        ZStack(alignment: .bottomLeading) {
            ZStack {
                Theme.bg3
                if let image {
                    Image(image, scale: 1, label: Text(photo.name))
                        .resizable()
                        .scaledToFit()
                } else {
                    ProgressView().tint(Theme.text3)
                }
            }
            .aspectRatio(1.5, contentMode: .fit)
            HStack(spacing: 5) {
                if photo.flag == 1 {
                    Image(systemName: "flag.fill")
                        .font(.system(size: 9))
                        .foregroundStyle(Theme.accent)
                }
                Text(photo.name)
                    .font(.system(size: 9))
                    .foregroundStyle(Theme.text2)
            }
            .padding(.horizontal, 6).padding(.vertical, 3)
            .background(.black.opacity(0.6))
            .clipShape(Capsule())
            .padding(6)
        }
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6)
            .stroke(photo.flag == 1 ? Theme.accent : Theme.hairline,
                    lineWidth: photo.flag == 1 ? 2 : 1))
        .task {
            let sc = await SafelightEngine.shared.work { $0.sidecar(path: photo.path, vslot: photo.vslot) }
            let img = await SafelightEngine.shared.work { $0.render(path: photo.path, recipe: sc.recipe, maxPx: 1200) }
            await MainActor.run { image = img.0 }
        }
    }
}

// MARK: - sidebar info card

/// capture metadata + flag/keyword editor for the active photo
struct InfoCard: View {
    @EnvironmentObject var store: LibraryStore
    let photo: Photo
    @State private var kwText = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(photo.name)
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(Theme.text1)
                .lineLimit(1).truncationMode(.middle)
            if !photo.camera.isEmpty {
                Text(photo.camera).font(.system(size: 9)).foregroundStyle(Theme.text2)
            }
            if !photo.lens.isEmpty {
                Text(photo.lens).font(.system(size: 9)).foregroundStyle(Theme.text3)
                    .lineLimit(1).truncationMode(.middle)
            }
            HStack(spacing: 8) {
                if photo.iso > 0 { Text("ISO \(Int(photo.iso))") }
                if photo.aperture > 0 { Text(String(format: "f/%.1f", photo.aperture)) }
                if photo.shutter > 0 {
                    Text(photo.shutter < 1 ? "1/\(Int(1.0 / photo.shutter))s" : String(format: "%.1fs", photo.shutter))
                }
                if photo.focal > 0 { Text("\(Int(photo.focal))mm") }
            }
            .font(.system(size: 8.5).monospacedDigit())
            .foregroundStyle(Theme.text3)
            HStack(spacing: 4) {
                flagButton(1, "flag.fill", Theme.accent)
                flagButton(0, "flag", Theme.text3)
                flagButton(-1, "xmark", Theme.red)
                Spacer()
                if photo.stack != 0 {
                    Text("stack \(photo.stack)").font(.system(size: 8)).foregroundStyle(Theme.text3)
                }
                if photo.isVariant {
                    Text("v\(photo.vslot)").font(.system(size: 8, weight: .bold)).foregroundStyle(Theme.accent)
                }
            }
            TextField("keywords (comma separated)", text: $kwText)
                .textFieldStyle(.plain)
                .font(.system(size: 9))
                .padding(.horizontal, 6).padding(.vertical, 4)
                .background(Theme.bg3)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .onSubmit {
                    let kw = kwText.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty }
                    store.setKeywords(photo, kw)
                }
                .onAppear { kwText = photo.keywords.joined(separator: ", ") }
                .onChange(of: photo.id) { kwText = photo.keywords.joined(separator: ", ") }
        }
        .padding(10)
        .background(Theme.bg1)
    }

    private func flagButton(_ v: Int, _ icon: String, _ tint: Color) -> some View {
        Button { store.setFlag(path: photo.path, vslot: photo.vslot, photo.flag == v ? 0 : v) } label: {
            Image(systemName: icon)
                .font(.system(size: 9))
                .foregroundStyle(photo.flag == v ? tint : Theme.text3)
                .frame(width: 18, height: 16)
                .background(photo.flag == v ? tint.opacity(0.15) : Color.clear)
                .clipShape(RoundedRectangle(cornerRadius: 3))
        }
        .buttonStyle(.plain)
    }
}

// MARK: - collection sheets

struct CollectionNameSheet: View {
    @Environment(\.dismiss) var dismiss
    let title: String
    var onCreate: (String) -> Void
    @State private var name = ""

    var body: some View {
        VStack(spacing: 14) {
            Text(title).font(.system(size: 13, weight: .semibold))
            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .frame(width: 240)
            HStack {
                Button("Cancel") { dismiss() }
                Button("Create") {
                    if !name.isEmpty { onCreate(name); dismiss() }
                }
                .buttonStyle(SlPrimaryButton())
                .disabled(name.isEmpty)
            }
        }
        .padding(20)
        .frame(width: 300)
    }
}

/// smart-collection rules editor — the same JSON shape core's smart_eval reads
struct SmartCollectionSheet: View {
    @Environment(\.dismiss) var dismiss
    @EnvironmentObject var store: LibraryStore
    @State private var name = ""
    @State private var ratingMin = 0
    @State private var flag = -2   // -2 = any
    @State private var label = ""
    @State private var camera = ""
    @State private var lens = ""
    @State private var keyword = ""
    @State private var nameLike = ""
    @State private var edited = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("New Smart Collection").font(.system(size: 13, weight: .semibold))
            TextField("Name", text: $name).textFieldStyle(.roundedBorder)
            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 8) {
                GridRow {
                    Text("Rating ≥").frame(width: 80, alignment: .trailing)
                    Picker("", selection: $ratingMin) {
                        ForEach(0...5, id: \.self) { Text("\($0)").tag($0) }
                    }
                    .pickerStyle(.segmented).frame(width: 160)
                }
                GridRow {
                    Text("Flag").frame(width: 80, alignment: .trailing)
                    Picker("", selection: $flag) {
                        Text("Any").tag(-2); Text("Picked").tag(1)
                        Text("Unflagged").tag(0); Text("Rejected").tag(-1)
                    }
                    .pickerStyle(.segmented).frame(width: 260)
                }
                GridRow {
                    Text("Label").frame(width: 80, alignment: .trailing)
                    Picker("", selection: $label) {
                        Text("Any").tag("")
                        ForEach(labelColors, id: \.name) { Text($0.name).tag($0.name) }
                    }
                    .frame(width: 130)
                }
                GridRow {
                    Text("Camera").frame(width: 80, alignment: .trailing)
                    TextField("contains", text: $camera).textFieldStyle(.roundedBorder)
                }
                GridRow {
                    Text("Lens").frame(width: 80, alignment: .trailing)
                    TextField("contains", text: $lens).textFieldStyle(.roundedBorder)
                }
                GridRow {
                    Text("Keyword").frame(width: 80, alignment: .trailing)
                    TextField("contains", text: $keyword).textFieldStyle(.roundedBorder)
                }
                GridRow {
                    Text("Filename").frame(width: 80, alignment: .trailing)
                    TextField("contains", text: $nameLike).textFieldStyle(.roundedBorder)
                }
                GridRow {
                    Text("").frame(width: 80)
                    Toggle("Edited only (rating/flag/label/keywords set)", isOn: $edited)
                        .font(.system(size: 10))
                }
            }
            .font(.system(size: 10))
            HStack {
                Button("Cancel") { dismiss() }
                Spacer()
                Button("Create") {
                    var rules: [String: Any] = [:]
                    if ratingMin > 0 { rules["rating_min"] = ratingMin }
                    if flag != -2 { rules["flag"] = flag }
                    if !label.isEmpty { rules["label"] = label }
                    if !camera.isEmpty { rules["camera_contains"] = camera }
                    if !lens.isEmpty { rules["lens_contains"] = lens }
                    if !keyword.isEmpty { rules["keyword"] = keyword }
                    if !nameLike.isEmpty { rules["name_contains"] = nameLike }
                    if edited { rules["edited"] = true }
                    let rulesJs = String(data: (try? JSONSerialization.data(withJSONObject: rules)) ?? Data(), encoding: .utf8) ?? "{}"
                    store.addCollection(name: name.isEmpty ? "Smart" : name, smart: true, rules: rulesJs)
                    dismiss()
                }
                .buttonStyle(SlPrimaryButton())
            }
        }
        .padding(18)
        .frame(width: 380)
    }
}

// MARK: - filters

/// segmented flag filter: All / Picked / Unflagged / Rejected
struct FlagFilter: View {
    @Binding var sel: String
    var body: some View {
        HStack(spacing: 0) {
            ForEach([("", "All", "flag"), ("pick", "P", "flag.fill"),
                     ("none", "U", "flag"), ("reject", "R", "xmark")], id: \.0) { v, t, icon in
                Button { sel = v } label: {
                    Image(systemName: icon)
                        .font(.system(size: 8.5))
                        .foregroundStyle(sel == v ? .white : Theme.text3)
                        .padding(.horizontal, 6).padding(.vertical, 3)
                        .background(sel == v ? Theme.accent : Color.clear)
                }
                .buttonStyle(.plain)
                .help(t)
            }
        }
        .clipShape(Capsule())
        .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
    }
}

/// Clickable 0–5 rating filter: tap a star to require that many stars.
struct RatingFilter: View {
    @Binding var rating: Int
    var body: some View {
        HStack(spacing: 2) {
            Image(systemName: "star.fill")
                .font(.system(size: 9))
                .foregroundStyle(Theme.text3)
            ForEach(1...5, id: \.self) { i in
                Image(systemName: i <= rating ? "star.fill" : "star")
                    .font(.system(size: 10))
                    .foregroundStyle(i <= rating ? Theme.gold : Theme.text3)
                    .onTapGesture { rating = (rating == i) ? 0 : i }
            }
            if rating > 0 { Text("+").font(.system(size: 9)).foregroundStyle(Theme.text3) }
        }
    }
}

/// Dropdown text filter for camera/lens/keyword metadata.
struct FilterMenu: View {
    let title: String
    let options: [String]
    @Binding var sel: String

    var body: some View {
        Menu {
            Button("All") { sel = "" }
            Divider()
            ForEach(options, id: \.self) { o in
                Button(o) { sel = (sel == o) ? "" : o }
            }
        } label: {
            HStack(spacing: 3) {
                Text(sel.isEmpty ? title : sel)
                    .font(.system(size: 10, weight: sel.isEmpty ? .regular : .semibold))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Image(systemName: "chevron.down")
                    .font(.system(size: 7, weight: .bold))
            }
            .foregroundStyle(sel.isEmpty ? Theme.text3 : Theme.accent)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(sel.isEmpty ? Color.clear : Theme.accent.opacity(0.12))
            .clipShape(Capsule())
            .overlay(Capsule().stroke(sel.isEmpty ? Theme.border : Theme.accent.opacity(0.6), lineWidth: 1))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
    }
}

/// darktable sort dropdown for the grid order.
struct SortMenu: View {
    @Binding var sel: String
    var body: some View {
        Menu {
            ForEach([("name", "Name"), ("capture", "Capture Time"), ("date", "Modified"),
                     ("rating", "Rating"), ("size", "File Size")], id: \.0) { k, name in
                Button { sel = k } label: {
                    if sel == k { Label(name, systemImage: "checkmark") } else { Text(name) }
                }
            }
        } label: {
            HStack(spacing: 3) {
                Image(systemName: "arrow.up.arrow.down")
                    .font(.system(size: 9, weight: .semibold))
                Text(["date": "Modified", "capture": "Captured", "rating": "Rating", "size": "Size"][sel] ?? "Name")
                    .font(.system(size: 10))
            }
            .foregroundStyle(Theme.text3)
            .padding(.horizontal, 7).padding(.vertical, 3)
            .clipShape(Capsule())
            .overlay(Capsule().stroke(Theme.border, lineWidth: 1))
        }
        .menuStyle(.borderlessButton)
        .fixedSize()
        .help("Sort order")
    }
}

/// Label-colour dots used as the library filter.
struct LabelFilter: View {
    @Binding var label: String
    var body: some View {
        HStack(spacing: 5) {
            ForEach(labelColors, id: \.name) { l in
                Circle()
                    .fill(l.color)
                    .frame(width: 10, height: 10)
                    .overlay(Circle().stroke(.white.opacity(0.85), lineWidth: label == l.name ? 1.5 : 0))
                    .opacity(label.isEmpty || label == l.name ? 1 : 0.35)
                    .onTapGesture { label = (label == l.name) ? "" : l.name }
            }
            if !label.isEmpty {
                Button { label = "" } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 10))
                        .foregroundStyle(Theme.text3)
                }
                .buttonStyle(.plain)
            }
        }
    }
}

// MARK: - thumbnail cell

struct ThumbCell: View {
    let photo: Photo
    let selected: Bool
    var active: Bool = false
    var stacked: Bool = false
    var stackCount: Int = 0
    var onRate: ((Int) -> Void)? = nil
    @State private var image: CGImage?
    @State private var hovering = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ZStack(alignment: .bottom) {
                ZStack {
                    Theme.bg3
                    if let image {
                        Image(image, scale: 1, label: Text(photo.name))
                            .resizable()
                            .scaledToFill()
                    } else {
                        ProgressView().controlSize(.small).tint(Theme.text3)
                    }
                }
                .aspectRatio(1.4, contentMode: .fit)
                .clipped()

                // stack depth behind the cover — LR-style offset outlines
                if stacked {
                    RoundedRectangle(cornerRadius: 7)
                        .stroke(Theme.text3.opacity(0.5), lineWidth: 1)
                        .offset(x: 3, y: -3)
                }

                // bottom gradient info bar
                LinearGradient(colors: [.clear, .black.opacity(0.75)],
                               startPoint: .top, endPoint: .bottom)
                    .frame(height: 30)
                HStack(alignment: .lastTextBaseline) {
                    if photo.rating > 0 {
                        Text(String(repeating: "★", count: photo.rating))
                            .font(.system(size: 9))
                            .foregroundStyle(Theme.gold)
                    }
                    if photo.flag == 1 {
                        Image(systemName: "flag.fill")
                            .font(.system(size: 8))
                            .foregroundStyle(Theme.accent)
                    } else if photo.flag == -1 {
                        Image(systemName: "xmark")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(Theme.red)
                    }
                    Spacer()
                    if photo.isVariant {
                        Text("v\(photo.vslot)")
                            .font(.system(size: 8, weight: .bold))
                            .foregroundStyle(Theme.accent)
                    }
                    if photo.has_sidecar {
                        Image(systemName: "checkmark.circle.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(Theme.accent)
                    }
                }
                .padding(.horizontal, 6)
                .padding(.bottom, 5)

                // stack badge (top-right)
                if photo.stack != 0 && stackCount > 1 {
                    VStack {
                        HStack {
                            Spacer()
                            HStack(spacing: 3) {
                                Image(systemName: "square.stack.3d.up.fill")
                                    .font(.system(size: 7))
                                Text("\(stackCount)")
                                    .font(.system(size: 8, weight: .bold).monospacedDigit())
                            }
                            .foregroundStyle(.white)
                            .padding(.horizontal, 5).padding(.vertical, 2)
                            .background(.black.opacity(0.6))
                            .clipShape(Capsule())
                            .padding(5)
                        }
                        Spacer()
                    }
                }

                // label dot
                if let c = labelColors.first(where: { $0.name == photo.label })?.color {
                    VStack {
                        HStack {
                            Circle()
                                .fill(c)
                                .frame(width: 8, height: 8)
                                .overlay(Circle().stroke(.black.opacity(0.5), lineWidth: 0.5))
                                .padding(6)
                            Spacer()
                        }
                        Spacer()
                    }
                }

                // darktable lighttable: inline star rating on hover
                if hovering, let onRate {
                    VStack {
                        HStack {
                            Spacer()
                            HStack(spacing: 2) {
                                ForEach(1...5, id: \.self) { i in
                                    Image(systemName: i <= photo.rating ? "star.fill" : "star")
                                        .font(.system(size: 8))
                                        .foregroundStyle(i <= photo.rating ? Theme.gold : .white.opacity(0.75))
                                        .frame(width: 13, height: 16)
                                        .contentShape(Rectangle())
                                        .onTapGesture { onRate(i) }
                                }
                            }
                            .padding(.horizontal, 4).padding(.vertical, 2)
                            .background(.black.opacity(0.55))
                            .clipShape(Capsule())
                            .padding(5)
                        }
                        Spacer()
                    }
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .overlay(
                RoundedRectangle(cornerRadius: 7)
                    .stroke(active ? Theme.accent
                            : (selected ? Theme.accent.opacity(0.55)
                               : (hovering ? Theme.border : Theme.hairline)),
                            lineWidth: active ? 2 : (selected ? 1.5 : 1))
            )
            .opacity(photo.flag == -1 ? 0.45 : 1)
            .shadow(color: .black.opacity(active ? 0.45 : 0.0), radius: 6, y: 2)
            .scaleEffect(hovering ? 1.02 : 1)
            .animation(.easeOut(duration: 0.12), value: hovering)
            .onHover { hovering = $0 }

            Text(photo.name)
                .font(.system(size: 10, weight: selected ? .semibold : .regular))
                .foregroundStyle(selected ? Theme.text1 : Theme.text2)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .task {
            let img = await SafelightEngine.shared.work { $0.thumbnail(path: photo.path, maxPx: 400) }
            await MainActor.run { image = img }
        }
        .id("thumb-\(photo.id)")
    }
}
