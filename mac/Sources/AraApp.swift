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
    @Published var selection: Photo?
    @Published var scanning = false
    @Published var minRating = 0
    @Published var labelFilter = ""

    /// Recipes edited but not yet saved, keyed by photo path — keeps unsaved
    /// work alive when switching photos. Cleared when the folder reloads.
    var unsavedEdits: [String: Recipe] = [:]

    /// Same idea for grade-version lists (they live in the sidecar file).
    var unsavedVersions: [String: [GradeVersion]] = [:]

    var filtered: [Photo] {
        photos.filter {
            $0.rating >= minRating && (labelFilter.isEmpty || $0.label == labelFilter)
        }
    }

    let eng = AraEngine.shared

    func pickFolder() {
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = false
        if p.runModal() == .OK, let url = p.url {
            open(url)
        }
    }

    func open(_ url: URL) {
        folder = url
        unsavedEdits.removeAll()
        unsavedVersions.removeAll()
        scanning = true
        Task.detached { [weak self] in
            let photos = await AraEngine.shared.work { $0.scan(folder: url.path) }
            await MainActor.run {
                self?.photos = photos
                self?.scanning = false
            }
        }
    }

    func refresh() {
        guard let url = folder else { return }
        open(url)
    }
}
