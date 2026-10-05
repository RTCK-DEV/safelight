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

    func render(path: String, recipe: Recipe, maxPx: UInt32) -> CGImage? {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        return cgImage(js.withCString { r in path.withCString { araware_render(handle, $0, r, maxPx) } })
    }

    func export(path: String, recipe: Recipe) -> CGImage? {
        let js = (try? JSONEncoder().encode(recipe)).flatMap { String(data: $0, encoding: .utf8) } ?? ""
        return cgImage(js.withCString { r in path.withCString { araware_export(handle, $0, r) } })
    }

    func metadata(path: String) -> String {
        takeString(path.withCString { araware_metadata(handle, $0) }) ?? ""
    }

    func sidecar(path: String) -> Sidecar {
        guard let js = takeString(path.withCString { araware_sidecar_read($0) }),
              let data = js.data(using: .utf8),
              let sc = try? JSONDecoder().decode(Sidecar.self, from: data)
        else { return Sidecar() }
        return sc
    }

    @discardableResult
    func writeSidecar(path: String, _ sc: Sidecar) -> Bool {
        guard let data = try? JSONEncoder().encode(sc),
              let js = String(data: data, encoding: .utf8) else { return false }
        return js.withCString { j in path.withCString { araware_sidecar_write($0, j) } } == 0
    }

    @discardableResult
    func setRating(path: String, _ rating: Int) -> Bool {
        path.withCString { araware_set_rating(handle, $0, Int32(rating)) } == 0
    }
}
