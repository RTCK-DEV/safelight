import Foundation
import AppKit
import CoreGraphics

/// macOS ImageIO never embeds ICC profiles into JPEG/PNG output — the
/// segments must be injected after encoding. JPEG gets ICC_PROFILE APP2
/// (64KB-chunked per spec); PNG gets an iCCP chunk (zlib-compressed).
enum ExportICC {
    /// ICC bytes of the image's tagged colorspace; nil → keep data unchanged.
    static func icc(of img: CGImage) -> Data? {
        img.colorSpace?.copyICCData() as Data?
    }

    /// insert ICC_PROFILE APP2 segments right after SOI
    static func jpeg(_ jpeg: Data, icc: Data) -> Data {
        guard jpeg.count >= 4, jpeg[0] == 0xFF, jpeg[1] == 0xD8 else { return jpeg }
        let chunkMax = 65519
        let nSeg = (icc.count + chunkMax - 1) / chunkMax
        var out = jpeg.subdata(in: 0..<2)
        for i in 0..<nSeg {
            let lo = i * chunkMax
            let hi = min(icc.count, lo + chunkMax)
            var seg = Data([0xFF, 0xE2])
            let plen = UInt16(2 + 14 + (hi - lo))
            seg.append(UInt8(plen >> 8)); seg.append(UInt8(plen & 0xFF))
            seg.append(contentsOf: "ICC_PROFILE".utf8); seg.append(0)
            seg.append(UInt8(i + 1)); seg.append(UInt8(nSeg))
            seg.append(icc.subdata(in: lo..<hi))
            out.append(seg)
        }
        out.append(jpeg.subdata(in: 2..<jpeg.count))
        return out
    }

    /// PNG iCCP chunk placed before the first IDAT
    static func png(_ png: Data, icc: Data) -> Data {
        guard png.count > 8,
              png[0..<8].elementsEqual([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]),
              let comp = try? (icc as NSData).compressed(using: .zlib) as Data
        else { return png }
        var payload = Data("ICC Profile".utf8)
        payload.append(0)      // null separator
        payload.append(0)      // compression method: zlib
        payload.append(comp)
        var chunk = Data()
        var len = UInt32(payload.count).bigEndian
        chunk.append(Data(bytes: &len, count: 4))
        var typeAndData = Data("iCCP".utf8)
        typeAndData.append(payload)
        chunk.append(typeAndData)
        var crc = crc32(typeAndData).bigEndian
        chunk.append(Data(bytes: &crc, count: 4))
        // locate first IDAT
        var pos = 8
        while pos + 8 <= png.count {
            let clen = Int(png[pos]) << 24 | Int(png[pos + 1]) << 16
                | Int(png[pos + 2]) << 8 | Int(png[pos + 3])
            let ctype = png[pos + 4..<pos + 8]
            if ctype.elementsEqual([0x49, 0x44, 0x41, 0x54]) { // "IDAT"
                var out = png.subdata(in: 0..<pos)
                out.append(chunk)
                out.append(png.subdata(in: pos..<png.count))
                return out
            }
            pos += 8 + clen + 4
        }
        return png
    }

    private static func crc32(_ d: Data) -> UInt32 {
        var crc: UInt32 = 0xFFFFFFFF
        for b in d {
            crc ^= UInt32(b)
            for _ in 0..<8 {
                crc = (crc >> 1) ^ ((crc & 1) != 0 ? 0xEDB88320 : 0)
            }
        }
        return ~crc
    }
}

final class SafelightEngine: @unchecked Sendable {
    private let handle: UnsafeMutableRawPointer
    private let queue = DispatchQueue(label: "ara.engine", qos: .userInitiated)

    static let shared: SafelightEngine = {
        guard let e = SafelightEngine() else { fatalError("safelight_init failed") }
        return e
    }()

    private init?() {
        guard let h = safelight_init() else { return nil }
        handle = h
    }

    deinit { safelight_free_engine(handle) }

    var lastError: String {
        guard let p = safelight_last_error() else { return "" }
        defer { safelight_free_string(p) }
        return String(cString: p)
    }

    private func takeString(_ p: UnsafeMutablePointer<CChar>?) -> String? {
        guard let p else { return nil }
        defer { safelight_free_string(p) }
        return String(cString: p)
    }

    private func cgImage(_ img: SlImage) -> CGImage? {
        guard img.data != nil, img.width > 0, img.height > 0 else { return nil }
        let ctx = UnsafeMutablePointer<SlImage>.allocate(capacity: 1)
        ctx.initialize(to: img)
        let w = Int(img.width), h = Int(img.height), len = img.len
        guard let provider = CGDataProvider(
            dataInfo: ctx,
            data: img.data,
            size: Int(len),
            releaseData: { info, _, _ in
                guard let info else { return }
                let i = info.assumingMemoryBound(to: SlImage.self)
                safelight_free_image(i.move())
                i.deallocate()
            }
        ) else {
            safelight_free_image(ctx.move())
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
    func work<T>(_ f: @escaping (SafelightEngine) -> T) async -> T {
        await withCheckedContinuation { c in
            queue.async { c.resume(returning: f(self)) }
        }
    }

    func scan(folder: String) -> [Photo] {
        guard let js = takeString(folder.withCString { safelight_scan_folder(handle, $0) }),
              let data = js.data(using: .utf8),
              let photos = try? JSONDecoder().decode([Photo].self, from: data)
        else { return [] }
        return photos
    }

    func thumbnail(path: String, maxPx: UInt32 = 512) -> CGImage? {
        cgImage(path.withCString { safelight_thumbnail(handle, $0, maxPx) })
    }

    /// Render plus the output-image histogram (R,G,B,luma x 256).
    func render(path: String, recipe: Recipe, maxPx: UInt32) -> (CGImage?, [[UInt32]]) {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var bins = [UInt32](repeating: 0, count: 1024)
        let img = bins.withUnsafeMutableBufferPointer { buf in
            buf.baseAddress!.withMemoryRebound(to: SlHistogram.self, capacity: 1) { hist in
                js.withCString { r in
                    path.withCString { safelight_render_h(handle, $0, r, maxPx, hist) }
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
                                safelight_scopes(handle, $0, r, maxPx,
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
        return cgImage(js.withCString { r in path.withCString { safelight_export(handle, $0, r) } })
    }

    /// export with finishing opts (long_edge px, output sharpen 0..1)
    func exportOpts(path: String, recipe: Recipe, opts: [String: Any]) -> CGImage? {
        guard let js = (try? JSONEncoder().encode(recipe)).flatMap({ String(data: $0, encoding: .utf8) }),
              let oj = try? JSONSerialization.data(withJSONObject: opts),
              let os = String(data: oj, encoding: .utf8) else { return nil }
        return cgImage(js.withCString { r in
            path.withCString { p in
                os.withCString { o in safelight_export_opts(handle, p, r, o) }
            }
        })
    }

    /// merge several photos at full res ("hdr" | "focus"); each renders
    /// with its own sidecar recipe, aligned onto the first.
    func merge(paths: [String], mode: String) -> CGImage? {
        guard let pj = try? JSONSerialization.data(withJSONObject: paths),
              let ps = String(data: pj, encoding: .utf8) else { return nil }
        return cgImage(mode.withCString { m in
            ps.withCString { p in safelight_merge(handle, p, m) }
        })
    }

    func metadata(path: String) -> String {
        takeString(path.withCString { safelight_metadata(handle, $0) }) ?? ""
    }

    /// Auto-correction analysis on a neutral preview — returns suggested
    /// recipe values plus confidence fields (see core/src/auto.rs).
    func autoAnalyze(path: String) -> AutoSuggestion? {
        guard let js = takeString(path.withCString { safelight_auto_analyze(handle, $0) }),
              let data = js.data(using: .utf8)
        else { return nil }
        return try? JSONDecoder().decode(AutoSuggestion.self, from: data)
    }

    /// Bake the SCUNet-denoised linear base to `<photo>.safelight.aidn.jpg`.
    /// Minutes on CPU — always call via `work`.
    func aiDenoisePrepare(path: String, recipe: Recipe)
        -> (ok: Bool, w: Int, h: Int, ms: Int, error: String)
    {
        struct R: Decodable { var ok: Bool; var w: Int?; var h: Int?; var ms: Int?; var error: String? }
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        guard let out = takeString(path.withCString { p in
            js.withCString { safelight_ai_denoise_prepare(handle, p, $0) }
        }), let data = out.data(using: .utf8),
              let r = try? JSONDecoder().decode(R.self, from: data)
        else { return (false, 0, 0, 0, "call failed") }
        return (r.ok, r.w ?? 0, r.h ?? 0, r.ms ?? 0, r.error ?? "")
    }

    /// Whether a fresh denoise cache exists for `path`.
    func aiDenoiseReady(path: String) -> Bool {
        struct R: Decodable { var ready: Bool }
        guard let out = takeString(path.withCString { safelight_ai_denoise_ready(handle, $0) }),
              let data = out.data(using: .utf8),
              let r = try? JSONDecoder().decode(R.self, from: data)
        else { return false }
        return r.ready
    }

    /// Run U-2-Net salient-subject detection and cache the matte next to the
    /// photo (a few seconds — much lighter than the denoise pass).
    func aiSubjectPrepare(path: String) -> (ok: Bool, w: Int, h: Int, ms: Int, error: String) {
        struct R: Decodable { var ok: Bool; var w: Int?; var h: Int?; var ms: Int?; var error: String? }
        guard let out = takeString(path.withCString { safelight_ai_subject_prepare(handle, $0) }),
              let data = out.data(using: .utf8),
              let r = try? JSONDecoder().decode(R.self, from: data)
        else { return (false, 0, 0, 0, "call failed") }
        return (r.ok, r.w ?? 0, r.h ?? 0, r.ms ?? 0, r.error ?? "")
    }

    /// Whether a fresh subject-matte cache exists for `path`.
    func aiSubjectReady(path: String) -> Bool {
        struct R: Decodable { var ready: Bool }
        guard let out = takeString(path.withCString { safelight_ai_subject_ready(handle, $0) }),
              let data = out.data(using: .utf8),
              let r = try? JSONDecoder().decode(R.self, from: data)
        else { return false }
        return r.ready
    }

    func sidecar(path: String, vslot: Int = 0) -> Sidecar {
        let js = takeString(path.withCString { safelight_sidecar_read_v($0, Int32(vslot)) })
            ?? takeString(path.withCString { safelight_sidecar_read($0) })
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
            path.withCString { safelight_sidecar_write_v($0, Int32(vslot), j) }
        } == 0
    }

    @discardableResult
    func setRating(path: String, _ rating: Int) -> Bool {
        path.withCString { safelight_set_rating(handle, $0, Int32(rating)) } == 0
    }

    @discardableResult
    func setLabel(path: String, _ label: String) -> Bool {
        label.withCString { l in path.withCString { safelight_set_label(handle, $0, l) } } == 0
    }

    // MARK: - library organization (flags / keywords / stacks / variants / collections)

    /// raw JSON dispatch into Engine::library — see core/src/engine.rs for ops
    @discardableResult
    func libraryCmd(_ cmd: [String: Any]) -> [String: Any]? {
        guard let data = try? JSONSerialization.data(withJSONObject: cmd),
              let s = String(data: data, encoding: .utf8),
              let js = takeString(s.withCString { safelight_library(handle, $0) }),
              let out = js.data(using: .utf8),
              let v = try? JSONSerialization.jsonObject(with: out) as? [String: Any]
        else { return nil }
        return v
    }

    private func libraryList<T: Decodable>(_ cmd: [String: Any]) -> [T] {
        guard let data = try? JSONSerialization.data(withJSONObject: cmd),
              let s = String(data: data, encoding: .utf8),
              let js = takeString(s.withCString { safelight_library(handle, $0) }),
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
