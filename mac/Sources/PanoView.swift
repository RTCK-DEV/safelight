import SceneKit
import SwiftUI
import simd

/// Interactive 360° viewer: the developed image wrapped on a sphere with
/// the camera at its center. Drag to look around, pinch to zoom FOV.
/// Shown on the stage when the photo is an equirectangular pano
/// (Insta360 .insp / DJI Osmo 360 / GPano 2:1 images).
struct PanoView: View {
    let image: CGImage
    @State private var yaw: CGFloat = 0
    @State private var pitch: CGFloat = 0
    @State private var fov: CGFloat = 70

    var body: some View {
        PanoSceneView(image: image, yaw: yaw, pitch: pitch, fov: fov)
            .gesture(
                DragGesture(minimumDistance: 1)
                    .onChanged { g in
                        // ~fov degrees across the visible height feels natural
                        yaw -= g.translation.width * 0.004
                        pitch = (pitch + g.translation.height * 0.004)
                            .clamped(to: -1.48...1.48)
                    }
            )
            .gesture(
                MagnifyGesture()
                    .onChanged { v in
                        fov = (fov / v.magnification).clamped(to: 25...100)
                    }
            )
            .onTapGesture(count: 2) { _ in
                withAnimation(.easeOut(duration: 0.25)) {
                    yaw = 0; pitch = 0; fov = 70
                }
            }
    }
}

private struct PanoSceneView: NSViewRepresentable {
    let image: CGImage
    let yaw: CGFloat
    let pitch: CGFloat
    let fov: CGFloat

    func makeNSView(context: Context) -> SCNView {
        let v = SCNView()
        v.backgroundColor = .black
        v.antialiasingMode = .multisampling4X
        let scene = SCNScene()

        let sphere = SCNSphere(radius: 10)
        // parametric (non-geodesic): only this unwrap has lat-long UVs that
        // match an equirectangular texture — geodesic UVs aren't lat-long
        sphere.segmentCount = 96
        let mat = SCNMaterial()
        mat.diffuse.contents = NSImage(cgImage: image, size: .zero)
        mat.isDoubleSided = true
        mat.diffuse.magnificationFilter = .linear
        mat.diffuse.minificationFilter = .linear
        mat.diffuse.mipFilter = .linear
        // viewed from inside: flip U so the texture isn't mirrored.
        // flipped U goes negative — must repeat, else clamp samples column 0
        mat.diffuse.wrapS = .repeat
        mat.diffuse.contentsTransform = SCNMatrix4MakeScale(-1, 1, 1)
        sphere.materials = [mat]
        scene.rootNode.addChildNode(SCNNode(geometry: sphere))

        let cam = SCNCamera()
        cam.zFar = 100
        let node = SCNNode()
        node.camera = cam
        scene.rootNode.addChildNode(node)
        v.scene = scene
        return v
    }

    func updateNSView(_ v: SCNView, context: Context) {
        guard let camNode = v.scene?.rootNode.childNodes
            .first(where: { $0.camera != nil }),
            let cam = camNode.camera else { return }
        // recipe re-render hands us a new CGImage — rebind the texture
        if context.coordinator.lastImage !== image {
            context.coordinator.lastImage = image
            if let sphere = v.scene?.rootNode.childNodes
                .first(where: { $0.geometry != nil }) {
                sphere.geometry?.materials.first?.diffuse.contents =
                    NSImage(cgImage: image, size: .zero)
            }
        }
        cam.fieldOfView = fov
        let qYaw = simd_quatf(angle: Float(yaw), axis: [0, 1, 0])
        let qPitch = simd_quatf(angle: Float(pitch), axis: [1, 0, 0])
        camNode.simdOrientation = simd_mul(qYaw, qPitch)
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    final class Coordinator {
        var lastImage: CGImage?
    }
}
