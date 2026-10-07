import SwiftUI

@main
struct AraApp: App {
    @StateObject private var store = LibraryStore()

    var body: some Scene {
        WindowGroup("araware") {
            LibraryView()
                .environmentObject(store)
                .frame(minWidth: 1080, minHeight: 660)
        }
        .windowToolbarStyle(.unifiedCompact)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Open Folder…") { store.pickFolder() }
                    .keyboardShortcut("o", modifiers: .command)
            }
        }
    }
}

final class LibraryStore: ObservableObject {
    @Published var photos: [Photo] = []
    @Published var folder: URL?
    /// multi-selection by Photo.id; `active` is the editor/focus photo
    @Published var selection: Set<String> = []
    @Published var activeId: String?
    @Published var scanning = false
    @Published var minRating = 0
    @Published var labelFilter = ""
    @Published var cameraFilter = ""
    @Published var lensFilter = ""
    /// "": all, "pick": picked only, "none": unflagged, "reject": rejected
    @Published var flagFilter = ""
    @Published var keywordFilter = ""
    @Published var searchText = ""
    /// sidebar scope: nil = current folder; "all" = every catalog folder;
    /// otherwise a collection id (manual or smart)
    @Published var scope: String = "folder"
    @Published var collections: [CollectionInfo] = []
    @Published var knownFolders: [String] = []
    /// stacks the user expanded in the grid (collapsed show cover only)
    @Published var expandedStacks: Set<Int64> = []
    /// compare/survey mode shows the selected photos side by side
    @Published var surveying = false
    /// darktable lighttable sort key: name | date | capture | rating | size
    @Published var sortKey = "name"

    /// Recipes edited but not yet saved, keyed by Photo.id (variants get
    /// their own slot) — keeps unsaved work alive when switching photos.
    var unsavedEdits: [String: Recipe] = [:]

    /// Same idea for grade-version lists (they live in the sidecar file).
    var unsavedVersions: [String: [GradeVersion]] = [:]

    init() {
        // collections + known folders exist independent of any open folder —
        // load them at launch so the sidebar is populated immediately
        reloadCollections()
    }

    /// distinct camera/lens names seen in the current folder (filter menus)
    var cameras: [String] { Array(Set(photos.map(\.camera).filter { !$0.isEmpty })).sorted() }
    var lenses: [String] { Array(Set(photos.map(\.lens).filter { !$0.isEmpty })).sorted() }
    var keywords: [String] { Array(Set(photos.flatMap(\.keywords))).sorted() }

    var active: Photo? {
        if let a = activeId, let p = photos.first(where: { $0.id == a }) { return p }
        return photos.first(where: { selection.contains($0.id) })
    }

    /// selected photos as currently in scope — resolves against `filtered`
    /// so All-Photos/collection selections of other folders' assets work
    var selectedPhotos: [Photo] {
        filtered.filter { selection.contains($0.id) }
    }

    /// decode a selection/photo ref into (path, vslot); refs are `path`
    /// or `path#vN` (N = virtual copy slot)
    func splitRef(_ ref: String) -> (path: String, vslot: Int) {
        if let i = ref.lastIndex(of: "#") {
            let suffix = ref[ref.index(after: i)...]
            if suffix.hasPrefix("v"), let n = Int(suffix.dropFirst()) {
                return (String(ref[..<i]), n)
            }
        }
        return (ref, 0)
    }

    private func scopePhotos() -> [Photo] {
        if scope == "folder" { return photos }
        if scope == "all" {
            // masters from every folder the catalog has seen
            var all = photos
            let have = Set(photos.map(\.path))
            for f in knownFolders where f != (folder?.path ?? "") {
                let more = eng.assetsSnapshot(folder: f).filter { !have.contains($0.path) }
                all.append(contentsOf: more)
            }
            return all
        }
        // collection scope: manual items or smart rules evaluated on the index
        guard let cid = Int64(scope),
              let c = collections.first(where: { $0.id == cid }) else { return photos }
        if c.smart == 1 {
            return eng.smartEval(rules: (try? JSONSerialization.jsonObject(
                with: Data(c.rules.utf8))) as? [String: Any] ?? [:])
        }
        // manual collection: items may live in any known folder — resolve
        // each ref against its own folder's catalog snapshot (variants
        // appear there with path#vN refs of their own)
        let refs = Set(eng.collectionItems(id: cid))
        var byFolder: [String: Set<String>] = [:]
        for r in refs {
            let base = String(r.split(separator: "#").first.map(String.init) ?? r)
            byFolder[URL(fileURLWithPath: base).deletingLastPathComponent().path,
                     default: []].insert(r)
        }
        var out: [Photo] = []
        for (dir, rs) in byFolder {
            let snap = dir == (folder?.path ?? "") ? photos
                : eng.assetsSnapshot(folder: dir)
            out.append(contentsOf: snap.filter {
                rs.contains($0.ref) || rs.contains($0.path)
            })
        }
        return out
    }

    var filtered: [Photo] {
        var f = scopePhotos().filter {
            $0.rating >= minRating
                && (labelFilter.isEmpty || $0.label == labelFilter)
                && (cameraFilter.isEmpty || $0.camera == cameraFilter)
                && (lensFilter.isEmpty || $0.lens == lensFilter)
                && (flagFilter.isEmpty
                    || (flagFilter == "pick" && $0.flag == 1)
                    || (flagFilter == "reject" && $0.flag == -1)
                    || (flagFilter == "none" && $0.flag == 0))
                && (keywordFilter.isEmpty || $0.keywords.contains(keywordFilter))
                && (searchText.isEmpty || $0.name.localizedCaseInsensitiveContains(searchText)
                    || $0.camera.localizedCaseInsensitiveContains(searchText)
                    || $0.lens.localizedCaseInsensitiveContains(searchText))
        }
        // collapsed stacks show only their cover (min stack_seq) member
        f = f.filter { p in
            guard p.stack != 0, !expandedStacks.contains(p.stack) else { return true }
            let members = f.filter { $0.stack == p.stack }
            return members.min(by: { $0.stack_seq < $1.stack_seq })?.id == p.id
        }
        switch sortKey {
        case "date":    return f.sorted { $0.mtime < $1.mtime }
        case "capture": return f.sorted { ($0.ctime, $0.name) < ($1.ctime, $1.name) }
        case "rating":  return f.sorted { ($0.rating, $0.name) > ($1.rating, $1.name) }
        case "size":    return f.sorted { $0.size > $1.size }
        default:        return f.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        }
    }

    /// Single rating write path for the whole app — thumbnail hover-stars,
    /// the editor's star row and digit keys all go through here so the
    /// sidecar, the grid and the open editor can never disagree.
    func setRating(path: String, vslot: Int = 0, _ r: Int) {
        let nr = min(max(r, 0), 5)
        if vslot == 0 {
            _ = eng.setRating(path: path, nr)
        } else {
            var sc = eng.sidecar(path: path, vslot: vslot)
            sc.rating = nr
            _ = eng.writeSidecar(path: path, vslot: vslot, sc)
        }
        let id = vslot == 0 ? path : "\(path)#v\(vslot)"
        if let i = photos.firstIndex(where: { $0.id == id }) {
            photos[i].rating = nr
        }
    }

    func setFlag(path: String, vslot: Int = 0, _ flag: Int) {
        _ = eng.setFlag(path: path, vslot: vslot, flag)
        let id = vslot == 0 ? path : "\(path)#v\(vslot)"
        if let i = photos.firstIndex(where: { $0.id == id }) {
            photos[i].flag = flag
        }
    }

    func setLabel(path: String, vslot: Int = 0, _ label: String) {
        if vslot == 0 {
            _ = eng.setLabel(path: path, label)
        } else {
            var sc = eng.sidecar(path: path, vslot: vslot)
            sc.label = label
            _ = eng.writeSidecar(path: path, vslot: vslot, sc)
        }
        let id = vslot == 0 ? path : "\(path)#v\(vslot)"
        if let i = photos.firstIndex(where: { $0.id == id }) {
            photos[i].label = label
        }
    }

    func setKeywords(_ p: Photo, _ kw: [String]) {
        if p.vslot == 0 {
            _ = eng.setKeywords(path: p.path, kw)
        } else {
            var sc = eng.sidecar(path: p.path, vslot: p.vslot)
            sc.keywords = kw
            _ = eng.writeSidecar(path: p.path, vslot: p.vslot, sc)
        }
        if let i = photos.firstIndex(where: { $0.id == p.id }) {
            photos[i].keywords = kw
        }
    }

    // ---- batch / selection ops -----------------------------------------

    /// apply a mutator to every selected ref — selection ids carry
    /// (path, vslot) so batch ops work even on assets outside the open
    /// folder, without needing a Photo row
    func forEachSelected(_ f: (_ path: String, _ vslot: Int) -> Void) {
        for ref in selection {
            let r = splitRef(ref)
            f(r.path, r.vslot)
        }
    }

    func stackSelection() {
        let ps = selectedPhotos.filter { $0.vslot == 0 }
        guard ps.count > 1 else { return }
        _ = eng.stackGroup(ps.map(\.path))
        refresh()
    }

    func unstackSelection() {
        let stacks = Set(selectedPhotos.map(\.stack).filter { $0 != 0 })
        for s in stacks { _ = eng.stackUngroup(s) }
        refresh()
    }

    func createVirtualCopy(of p: Photo) {
        guard p.vslot >= 0 else { return }
        let src = p.vslot
        _ = eng.variantCreate(path: p.path, vslot: src)
        refresh()
    }

    func deleteVirtualCopy(_ p: Photo) {
        guard p.isVariant else { return }
        _ = eng.variantDelete(path: p.path, vslot: p.vslot)
        refresh()
    }

    func promoteVirtualCopy(_ p: Photo) {
        guard p.isVariant else { return }
        _ = eng.variantPromote(path: p.path, vslot: p.vslot)
        refresh()
    }

    // ---- collections ----------------------------------------------------

    func reloadCollections() {
        collections = eng.collections
        knownFolders = eng.knownFolders()
    }

    func addCollection(name: String, smart: Bool, rules: String) {
        _ = eng.collectionAdd(name: name, smart: smart, rules: rules)
        reloadCollections()
    }

    func renameCollection(_ c: CollectionInfo, name: String) {
        _ = eng.collectionRename(id: c.id, name: name)
        reloadCollections()
    }

    func deleteCollection(_ c: CollectionInfo) {
        _ = eng.collectionDelete(id: c.id)
        if scope == String(c.id) { scope = "folder" }
        reloadCollections()
    }

    func addSelectedToCollection(_ c: CollectionInfo) {
        eng.collectionAddItems(id: c.id, refs: Array(selection))
        reloadCollections()
    }

    func removeSelectedFromCollection(_ c: CollectionInfo) {
        eng.collectionRemoveItems(id: c.id, refs: Array(selection))
        reloadCollections()
    }

    // ---- selection helpers ----------------------------------------------

    func select(_ p: Photo, additive: Bool = false, range: Bool = false) {
        if range, let a = activeId,
           let ai = filtered.firstIndex(where: { $0.id == a }),
           let bi = filtered.firstIndex(where: { $0.id == p.id }) {
            let lo = min(ai, bi), hi = max(ai, bi)
            selection.formUnion(filtered[lo...hi].map(\.id))
        } else if additive {
            if selection.contains(p.id) { selection.remove(p.id) } else { selection.insert(p.id) }
        } else {
            selection = [p.id]
        }
        activeId = p.id
    }

    func moveSelection(_ dir: Int) {
        let list = filtered
        guard !list.isEmpty else { return }
        let cur = activeId.flatMap { a in list.firstIndex(where: { $0.id == a }) } ?? 0
        let nxt = (cur + dir).clamped(to: 0...(list.count - 1))
        selection = [list[nxt].id]
        activeId = list[nxt].id
    }

    let eng = AraEngine.shared

    func pickFolder() {
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = true
        if p.runModal() == .OK, let url = p.urls.first {
            open(url)
        }
    }

    func open(_ url: URL) {
        let switching = folder != url
        folder = url
        scope = "folder"
        if switching {
            // a different folder: drafts and selection belong to the old list
            unsavedEdits.removeAll()
            unsavedVersions.removeAll()
            selection.removeAll()
            activeId = nil
        }
        // a rescan of the same folder keeps unsaved edits + selection alive
        scanning = true
        reloadCollections()
        Task.detached { [weak self] in
            let photos = await AraEngine.shared.work { $0.scan(folder: url.path) }
            await MainActor.run {
                // a stale scan finishing after the user opened another
                // folder must not overwrite the newer folder's list
                guard self?.folder == url else { return }
                self?.photos = photos
                self?.scanning = false
                self?.reloadCollections()
                if let sel = self?.activeId,
                   photos.contains(where: { $0.id == sel }) {
                    return // keep the selection on rescan
                } else {
                    self?.activeId = photos.first?.id
                    self?.selection = photos.first.map { [$0.id] } ?? []
                }
            }
        }
    }

    func refresh() {
        guard let url = folder else { return }
        open(url)
    }
}

private extension Int {
    func clamped(to r: ClosedRange<Int>) -> Int { Swift.min(Swift.max(self, r.lowerBound), r.upperBound) }
}
