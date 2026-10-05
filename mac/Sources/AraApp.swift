import SwiftUI

@main
struct AraApp: App {
    @StateObject private var store = LibraryStore()

    var body: some Scene {
        WindowGroup("araware") {
            LibraryView()
                .environmentObject(store)
                .frame(minWidth: 960, minHeight: 620)
        }
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
