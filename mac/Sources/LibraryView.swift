import SwiftUI

struct LibraryView: View {
    @EnvironmentObject var store: LibraryStore

    var body: some View {
        NavigationSplitView {
            Group {
                if store.scanning {
                    ProgressView("Scanning…")
                } else if store.photos.isEmpty {
                    VStack(spacing: 12) {
                        Image(systemName: "photo.on.rectangle.angled")
                            .font(.system(size: 44))
                            .foregroundStyle(.secondary)
                        Text("Open a folder of RAW files")
                            .foregroundStyle(.secondary)
                        Button("Open Folder…") { store.pickFolder() }
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ScrollView {
                        LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 8)], spacing: 8) {
                            ForEach(store.photos) { photo in
                                ThumbCell(photo: photo)
                                    .onTapGesture { store.selection = photo }
                            }
                        }
                        .padding(8)
                    }
                }
            }
            .navigationTitle(store.folder?.lastPathComponent ?? "Library")
            .toolbar {
                ToolbarItemGroup {
                    Button { store.pickFolder() } label: { Label("Open", systemImage: "folder") }
                    Button { store.refresh() } label: { Label("Refresh", systemImage: "arrow.clockwise") }
                }
            }
        } detail: {
            if let photo = store.selection {
                EditorView(photo: photo)
                    .id(photo.id)
            } else {
                Text("Select a photo")
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
    }
}

struct ThumbCell: View {
    let photo: Photo
    @State private var image: CGImage?

    var body: some View {
        VStack(spacing: 4) {
            ZStack {
                Rectangle().fill(.quaternary)
                    .aspectRatio(1.4, contentMode: .fit)
                if let image {
                    Image(image, scale: 1, label: Text(photo.name))
                        .resizable()
                        .scaledToFit()
                } else {
                    ProgressView()
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 6))
            HStack(spacing: 4) {
                Text(photo.name)
                    .font(.caption2)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if photo.rating > 0 {
                    Text(String(repeating: "★", count: photo.rating))
                        .font(.caption2)
                        .foregroundStyle(.yellow)
                }
            }
        }
        .task {
            let img = await AraEngine.shared.work { $0.thumbnail(path: photo.path, maxPx: 400) }
            await MainActor.run { image = img }
        }
    }
}
