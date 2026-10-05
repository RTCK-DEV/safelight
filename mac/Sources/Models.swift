import Foundation

struct Photo: Identifiable, Codable, Hashable {
    var id: String { path }
    let path: String
    let name: String
    let kind: String
    let size: Int64
    let mtime: Int64
    var rating: Int
    var label: String
    let pair: String?
    let has_sidecar: Bool
}

enum WbMode: String, Codable, CaseIterable {
    case asShot = "as_shot"
    case auto = "auto"
    case manual = "manual"
}

/// Mirrors araware_core::recipe::Recipe (serde snake_case).
struct Recipe: Codable, Equatable {
    var exposure: Double = 0
    var contrast: Double = 0
    var highlights: Double = 0
    var shadows: Double = 0
    var whites: Double = 0
    var blacks: Double = 0
    var saturation: Double = 0
    var vibrance: Double = 0
    var temperature: Double = 0
    var tint: Double = 0
    var wb_mode: WbMode = .asShot
    var curve: [[Double]] = []
    var sharpen: Double = 0
    var noise_luma: Double = 0
}

/// Mirrors araware_core::recipe::Sidecar.
struct Sidecar: Codable {
    var version: Int = 1
    var rating: Int = 0
    var label: String = ""
    var recipe: Recipe = Recipe()
}
