import Foundation
import AppKit
import CoreGraphics

final class AraEngine: @unchecked Sendable {
    private let handle: UnsafeMutableRawPointer
    private let queue = DispatchQueue(label: "ara.engine", qos: .userInitiated)

    static let shared: AraEngine = {
        guard let e = AraEngine() else { fatalError("araware_init failed") }
        return e
    }()

    private init?() {
        guard let h = araware_init() else { return nil }
        handle = h
    }

    deinit { araware_free_engine(handle) }

    var lastError: String {
        guard let p = araware_last_error() else { return "" }
        defer { araware_free_string(p) }
        return String(cString: p)
    }

    private func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p else { return nil }
        defer { araware_free_string(p) }
        return String(cString: p)
    }

    private func cgImage(_ img: AraImage) -> CGImage? {
        guard img.data != nil, img.width > 0, img.height > 0 else { return nil }
        let ctx = UnsafeMutablePointer<AraImage>.allocate(capacity: 1)
        ctx.initialize(to: img)
        let w = Int(img.width), h = Int(img.height), len = img.len
        guard let provider = CGDataProvider(
            dataInfo: ctx,
            data: img.data,
            size: Int(len),
            releaseData: { info, _, _ in
                guard let info else { return }
                let i = info.assumingMemoryBound(to: AraImage.self)
                araware_free_image(i.move())
                i.deallocate()
            }
        ) else {
            araware_free_image(ctx.move())
            ctx.deallocate()
            return nil
        }
        return CGImage(
            width: w, height: h,
            bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue),
            provider: provider, decode: nil,
            shouldInterpolate: true, intent: .defaultIntent
        )
    }

    /// Run blocking engine calls off the main thread.
    func work<T>(_ f: @escaping (AraEngine) -> T) async -> T {
        await withCheckedContinuation { c in
            queue.async { c.resume(returning: f(self)) }
        }
    }

    func scan(folder: String) -> [Photo] {
        guard let js = takeString(folder.withCString { araware_scan_folder(handle, $0) }),
              let data = js.data(using: .utf8),
              let photos = try? JSONDecoder().decode([Photo].self, from: data)
        else { return [] }
        return photos
    }

    func thumbnail(path: String, maxPx: UInt32 = 512) -> CGImage? {
        cgImage(path.withCString { araware_thumbnail(handle, $0, maxPx) })
    }

    /// Render plus the output-image histogram (R,G,B,luma x 256).
    func render(path: String, recipe: Recipe, maxPx: UInt32) -> (CGImage?, [[UInt32]]) {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var bins = [UInt32](repeating: 0, count: 1024)
        let img = bins.withUnsafeMutableBufferPointer { buf in
            buf.baseAddress!.withMemoryRebound(to: AraHistogram.self, capacity: 1) { hist in
                js.withCString { r in
                    path.withCString { araware_render_h(handle, $0, r, maxPx, hist) }
                }
            }
        }
        let rows = (0..<4).map { ch in Array(bins[(ch * 256)..<(ch * 256 + 256)]) }
        return (cgImage(img), rows)
    }

    /// Render plus scope buffers (histogram, waveform 3ch, vectorscope, CIE xy).
    func renderScopes(path: String, recipe: Recipe, maxPx: UInt32)
        -> (CGImage?, [UInt32], [UInt32], [UInt32], [UInt32])
    {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var wave = [UInt32](repeating: 0, count: 196608)
        var vec = [UInt32](repeating: 0, count: 65536)
        var cie = [UInt32](repeating: 0, count: 65536)
        var bins = [UInt32](repeating: 0, count: 1024)
        let img = wave.withUnsafeMutableBufferPointer { wv in
            vec.withUnsafeMutableBufferPointer { vc in
                cie.withUnsafeMutableBufferPointer { ce in
                    bins.withUnsafeMutableBufferPointer { hs in
                        js.withCString { r in
                            path.withCString {
                                araware_scopes(handle, $0, r, maxPx,
                                               wv.baseAddress, vc.baseAddress,
                                               ce.baseAddress, hs.baseAddress)
                            }
                        }
                    }
                }
            }
        }
        return (cgImage(img), bins, wave, vec, cie)
    }

    func export(path: String, recipe: Recipe) -> CGImage? {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        return cgImage(js.withCString { r in path.withCString { araware_export(handle, $0, r) } })
    }

    func metadata(path: String) -> String {
        takeString(path.withCString { araware_metadata(handle, $0) }) ?? ""
    }

    /// Auto-correction analysis on a neutral preview — returns suggested
    /// recipe values plus confidence fields (see core/src/auto.rs).
    func autoAnalyze(path: String) -> AutoSuggestion? {
        guard let js = takeString(path.withCString { araware_auto_analyze(handle, $0) }),
              let data = js.data(using: .utf8)
        else { return nil }
        return try? JSONDecoder().decode(AutoSuggestion.self, from: data)
    }

    func sidecar(path: String, vslot: Int = 0) -> Sidecar {
        let js = takeString(path.withCString { araware_sidecar_read_v($0, Int32(vslot)) })
            ?? takeString(path.withCString { araware_sidecar_read($0) })
        guard let js,
              let data = js.data(using: .utf8),
              let sc = try? JSONDecoder().decode(Sidecar.self, from: data)
        else { return Sidecar() }
        return sc
    }

    @discardableResult
    func writeSidecar(path: String, vslot: Int = 0, _ sc: Sidecar) -> Bool {
        guard let data = try? JSONEncoder().encode(sc),
              let js = String(data: data, encoding: .utf8) else { return false }
        return js.withCString { j in
            path.withCString { araware_sidecar_write_v($0, Int32(vslot), j) }
        } == 0
    }

    @discardableResult
    func setRating(path: String, _ rating: Int) -> Bool {
        path.withCString { araware_set_rating(handle, $0, Int32(rating)) } == 0
    }

    @discardableResult
    func setLabel(path: String, _ label: String) -> Bool {
        label.withCString { l in path.withCString { araware_set_label(handle, $0, l) } } == 0
    }

    // MARK: - library organization (flags / keywords / stacks / variants / collections)

    /// raw JSON dispatch into Engine::library — see core/src/engine.rs for ops
    @discardableResult
    func libraryCmd(_ cmd: [String: Any]) -> [String: Any]? {
        guard let data = try? JSONSerialization.data(withJSONObject: cmd),
              let s = String(data: data, encoding: .utf8),
              let js = takeString(s.withCString { araware_library(handle, $0) }),
              let out = js.data(using: .utf8),
              let v = try? JSONSerialization.jsonObject(with: out) as? [String: Any]
        else { return nil }
        return v
    }

    private func libraryList<T: Decodable>(_ cmd: [String: Any]) -> [T] {
        guard let data = try? JSONSerialization.data(withJSONObject: cmd),
              let s = String(data: data, encoding: .utf8),
              let js = takeString(s.withCString { araware_library(handle, $0) }),
              let out = js.data(using: .utf8),
              let v = try? JSONDecoder().decode([T].self, from: out)
        else { return [] }
        return v
    }

    @discardableResult
    func setFlag(path: String, vslot: Int = 0, _ flag: Int) -> Bool {
        libraryCmd(["op": "set_flag", "path": path, "vslot": vslot, "flag": flag]) != nil
    }

    @discardableResult
    func setKeywords(path: String, _ keywords: [String]) -> Bool {
        libraryCmd(["op": "set_keywords", "path": path, "keywords": keywords]) != nil
    }

    @discardableResult
    func stackGroup(_ paths: [String]) -> Int64 {
        libraryCmd(["op": "stack_group", "paths": paths])
            .flatMap { ($0["stack"] as? NSNumber)?.int64Value } ?? 0
    }

    @discardableResult
    func stackUngroup(_ stack: Int64) -> Bool {
        libraryCmd(["op": "stack_ungroup", "stack": stack]) != nil
    }

    @discardableResult
    func stackCover(_ path: String) -> Bool {
        libraryCmd(["op": "stack_cover", "path": path]) != nil
    }

    /// create a virtual copy of `path` seeded from `vslot`'s sidecar; returns new slot
    @discardableResult
    func variantCreate(path: String, vslot: Int = 0) -> Int {
        Int(libraryCmd(["op": "variant_create", "path": path, "vslot": vslot])
            .flatMap { ($0["vslot"] as? NSNumber)?.intValue } ?? 0)
    }

    @discardableResult
    func variantDelete(path: String, vslot: Int) -> Bool {
        libraryCmd(["op": "variant_delete", "path": path, "vslot": vslot]) != nil
    }

    @discardableResult
    func variantPromote(path: String, vslot: Int) -> Bool {
        libraryCmd(["op": "variant_promote", "path": path, "vslot": vslot]) != nil
    }

    var collections: [CollectionInfo] { libraryList(["op": "coll_list"]) }

    @discardableResult
    func collectionAdd(name: String, smart: Bool = false, rules: String = "") -> Int64 {
        libraryCmd(["op": "coll_add", "name": name, "smart": smart, "rules": rules])
            .flatMap { ($0["id"] as? NSNumber)?.int64Value } ?? 0
    }

    @discardableResult
    func collectionRename(id: Int64, name: String) -> Bool {
        libraryCmd(["op": "coll_rename", "id": id, "name": name]) != nil
    }

    @discardableResult
    func collectionSetRules(id: Int64, rules: String) -> Bool {
        libraryCmd(["op": "coll_rules", "id": id, "rules": rules]) != nil
    }

    @discardableResult
    func collectionDelete(id: Int64) -> Bool {
        libraryCmd(["op": "coll_delete", "id": id]) != nil
    }

    func collectionItems(id: Int64) -> [String] {
        libraryList(["op": "coll_items", "id": id])
    }

    @discardableResult
    func collectionAddItems(id: Int64, refs: [String]) -> Bool {
        libraryCmd(["op": "coll_add_items", "id": id, "paths": refs]) != nil
    }

    @discardableResult
    func collectionRemoveItems(id: Int64, refs: [String]) -> Bool {
        libraryCmd(["op": "coll_remove_items", "id": id, "paths": refs]) != nil
    }

    /// evaluate smart rules against the catalog index — returns matching
    /// Photo rows (masters only, across every scanned folder)
    func smartEval(rules: [String: Any]) -> [Photo] {
        let rulesJs = (try? JSONSerialization.data(withJSONObject: rules))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        return libraryList(["op": "smart_eval", "rules": rulesJs])
    }

    /// folders the catalog knows about (for the sidebar)
    func knownFolders() -> [String] {
        libraryList(["op": "folders"])
    }

    /// DB snapshot of a folder without rescanning
    func assetsSnapshot(folder: String) -> [Photo] {
        libraryList(["op": "assets", "path": folder])
    }
}
