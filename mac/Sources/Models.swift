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
    var rotation_deg: Double = 0
    var clarity: Double = 0
    var vignette: Double = 0
    var grain: Double = 0
    // grading
    var lift: [Double] = [0, 0, 0]
    var gamma: [Double] = [1, 1, 1]
    var gain: [Double] = [1, 1, 1]
    var shadow_hue: Double = 0.55
    var shadow_sat: Double = 0
    var highlight_hue: Double = 0.08
    var highlight_sat: Double = 0
    var look: String = ""
    // auto correction
    var auto_exposure: Bool = false
    var auto_contrast: Bool = false
    // retouch
    var crop: [Double] = [0, 0, 0, 0]
    var spots: [[Double]] = []
    var lights: [[Double]] = []
}

/// Mirrors araware_core::recipe::Sidecar.
struct Sidecar: Codable {
    var version: Int = 1
    var rating: Int = 0
    var label: String = ""
    var recipe: Recipe = Recipe()
}
