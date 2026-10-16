import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

/// Headless smoke test driven by the `SAFELIGHT_SELFTEST=<outdir>` env var.
/// Exercises the exact engine calls the UI makes (scan → thumbnail → render
/// → scopes → sidecar write/read → rating/label/flag → export + ICC JPEG)
/// against the folder opened via launch arg, writes `selftest.json` and
/// `export.jpg` into <outdir>, then exits — GUI verification without
/// needing an accessibility grant for synthetic input.
enum SelfTest {

    static func run(store: LibraryStore) {
        guard let dir = ProcessInfo.processInfo.environment["SAFELIGHT_SELFTEST"],
              !dir.isEmpty else { return }
        Task { await SelfTest.go(store: store, outDir: dir) }
    }

    private struct Report: Encodable {
        var folder: String
        var photo_count: Int
        var checks: [Check]
        var ok: Bool { checks.allSatisfy { $0.pass } }
    }
    private struct Check: Encodable {
        var name: String
        var pass: Bool
        var detail: String
    }

    @MainActor
    private static func go(store: LibraryStore, outDir: String) async {
        var checks: [Check] = []
        func ck(_ name: String, _ pass: Bool, _ detail: String = "") {
            checks.append(Check(name: name, pass: pass, detail: detail))
            NSLog("[selftest] %@ %@ %@", pass ? "PASS" : "FAIL", name, detail)
        }

        let out = URL(fileURLWithPath: outDir, isDirectory: true)
        try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

        // wait for the folder scan launched via the launch-arg path
        let deadline = Date().addingTimeInterval(60)
        while (store.scanning || store.photos.isEmpty), Date() < deadline {
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
        guard let photo = store.photos.first else {
            let rep = Report(folder: store.folder?.path ?? "", photo_count: 0,
                             checks: [Check(name: "scan", pass: false, detail: "no photos")])
            write(rep, to: out)
            exit(1)
        }
        ck("scan", true, "\(store.photos.count) photos")

        let eng = SafelightEngine.shared
        let path = photo.path

        // thumbnail (grid path)
        let thumb = await eng.work { $0.thumbnail(path: path, maxPx: 400) }
        ck("thumbnail", thumb != nil,
           thumb.map { "\($0.width)x\($0.height)" } ?? "nil")

        // render + histogram (editor preview path)
        var recipe = Recipe()
        recipe.exposure = 0.7
        let (img, hist) = await eng.work { $0.render(path: path, recipe: recipe, maxPx: 900) }
        let histSum = hist.flatMap { $0 }.reduce(0 as UInt32, +)
        ck("render", img != nil && histSum > 0,
           img.map { "\($0.width)x\($0.height) hist=\(histSum)" } ?? "nil")

        // scopes buffers (histogram/waveform/vectorscope/CIE)
        let (_, bins, wave, vec, cie) = await eng.work {
            $0.renderScopes(path: path, recipe: Recipe(), maxPx: 400)
        }
        ck("scopes", bins.reduce(0,+) > 0 && wave.reduce(0,+) > 0
           && vec.reduce(0,+) > 0 && cie.reduce(0,+) > 0,
           "bins=\(bins.reduce(0,+)) wave=\(wave.reduce(0,+)) "
           + "vec=\(vec.reduce(0,+)) cie=\(cie.reduce(0,+))")

        // auto analyze (Auto chip path)
        let sug = await eng.work { $0.autoAnalyze(path: path) }
        ck("auto_analyze", sug != nil,
           sug.map { "zones=\($0.zones_ev.count) conf_rot=\($0.straighten_conf)" } ?? "nil")

        // sidecar round-trip via the same calls save() uses — restore the
        // original afterwards so the test never clobbers real edits
        let orig = await eng.work { $0.sidecar(path: path, vslot: photo.vslot) }
        var sc = orig
        sc.rating = 3; sc.label = "selftest"; sc.recipe = recipe
        let wrote = await eng.work { $0.writeSidecar(path: path, vslot: photo.vslot, sc) }
        let back = await eng.work { $0.sidecar(path: path, vslot: photo.vslot) }
        ck("sidecar", wrote && back.rating == 3 && back.label == "selftest"
           && abs(back.recipe.exposure - 0.7) < 1e-6,
           "wrote=\(wrote) rating=\(back.rating) label=\(back.label) exp=\(back.recipe.exposure)")
        _ = await eng.work { $0.writeSidecar(path: path, vslot: photo.vslot, orig) }

        // flag/keywords setters (quick-key paths)
        let flagOk = await eng.work { $0.setFlag(path: path, vslot: photo.vslot, 1) }
        let kwOk = await eng.work { $0.setKeywords(path: path, ["selftest"]) }
        let sc2 = await eng.work { $0.sidecar(path: path, vslot: photo.vslot) }
        ck("flag_keywords", flagOk && kwOk && sc2.flag == 1
           && sc2.keywords.contains("selftest"),
           "flag=\(sc2.flag) kw=\(sc2.keywords)")
        _ = await eng.work { $0.setFlag(path: path, vslot: photo.vslot, orig.flag) }
        _ = await eng.work { $0.setKeywords(path: path, orig.keywords) }

        // export path: engine export → JPEG encode → ICC APP2 injection,
        // identical to runExport() minus the save panel
        if let ex = await eng.work({
            $0.exportOpts(path: path, recipe: recipe,
                          opts: ["long_edge": 1200, "sharpen": 0.3])
        }) {
            let mdata = NSMutableData()
            var ok = false
            var detail = "encode failed"
            if let dest = CGImageDestinationCreateWithData(
                mdata, UTType.jpeg.identifier as CFString, 1, nil) {
                CGImageDestinationAddImage(dest, ex, [
                    kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary)
                if CGImageDestinationFinalize(dest) {
                    var data = mdata as Data
                    if let icc = ExportICC.icc(of: ex) {
                        data = ExportICC.jpeg(data, icc: icc)
                    }
                    let f = out.appendingPathComponent("export.jpg")
                    ok = (try? data.write(to: f)) != nil
                    let hasICC = data.range(
                        of: "ICC_PROFILE".data(using: .utf8)!,
                        in: 0..<min(data.count, 200_000)) != nil
                    detail = "\(ex.width)x\(ex.height) bytes=\(data.count) icc=\(hasICC)"
                    ok = ok && hasICC
                }
            }
            ck("export_jpeg", ok, detail)
        } else {
            ck("export_jpeg", false, SafelightEngine.shared.lastError)
        }

        let rep = Report(folder: store.folder?.path ?? "",
                         photo_count: store.photos.count, checks: checks)
        write(rep, to: out)
        exit(rep.ok ? 0 : 1)
    }

    private static func write(_ rep: Report, to out: URL) {
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        if let d = try? enc.encode(rep) {
            try? d.write(to: out.appendingPathComponent("selftest.json"))
            NSLog("[selftest] report -> %@", out.appendingPathComponent("selftest.json").path)
        }
    }
}
