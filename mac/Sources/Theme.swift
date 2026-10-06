import SwiftUI

/// araware "darkroom" theme — near-black surfaces, warm amber accent.
enum Ara {
    static let bg0 = Color(red: 0.035, green: 0.035, blue: 0.042)   // stage / window
    static let bg1 = Color(red: 0.075, green: 0.077, blue: 0.086)   // sidebar / inspector
    static let bg2 = Color(red: 0.115, green: 0.118, blue: 0.130)   // cards / panels
    static let bg3 = Color(red: 0.165, green: 0.170, blue: 0.185)   // inset tracks / chips
    static let bg4 = Color(red: 0.22, green: 0.225, blue: 0.245)    // hover / selected chip
    static let hairline = Color.white.opacity(0.07)
    static let border = Color.white.opacity(0.10)
    static let text1 = Color.white.opacity(0.92)
    static let text2 = Color.white.opacity(0.55)
    static let text3 = Color.white.opacity(0.32)
    static let accent = Color(red: 0.96, green: 0.66, blue: 0.24)   // warm amber
    static let accentSoft = accent.opacity(0.16)
    static let gold = Color(red: 1.0, green: 0.80, blue: 0.30)
    static let track = Color.white.opacity(0.13)
}

/// Custom slider track: thin rail, amber fill drawn from the default (reset)
/// position toward the knob, white knob ringed with accent. Drag or click to
/// set; double-click resets. `reset` defaults to 0 when the range contains it.
struct TrackSlider: View {
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.01
    var reset: Double? = nil
    var height: CGFloat = 16

    @State private var dragActive = false
    @State private var lastStart = Date.distantPast

    private var def: Double {
        reset ?? (range.contains(0) ? 0 : range.lowerBound)
    }

    var body: some View {
        GeometryReader { geo in
            let w = geo.size.width
            let span = range.upperBound - range.lowerBound
            let frac = { (v: Double) -> CGFloat in
                CGFloat((v - range.lowerBound) / span).clampedTo01() * w
            }
            let kx = frac(value)
            let dx = frac(def)
            ZStack {
                Capsule().fill(Ara.track)
                    .frame(width: w, height: 4)
                Capsule().fill(Ara.accent.opacity(0.85))
                    .frame(width: max(abs(kx - dx), 2), height: 4)
                    .offset(x: min(kx, dx))
                Circle()
                    .fill(Color.white)
                    .frame(width: 11, height: 11)
                    .overlay(Circle().stroke(Ara.accent.opacity(0.95), lineWidth: 2))
                    .shadow(color: .black.opacity(0.5), radius: 1, y: 0.5)
                    .position(x: kx.clamped(to: 5.5...(w - 5.5)), y: geo.size.height / 2)
            }
            .frame(maxHeight: .infinity)
            .contentShape(Rectangle())
            // DragGesture(minimumDistance:0) swallows onTapGesture, so the
            // double-click reset is detected manually: two gesture starts
            // within 0.3s restore the default instead of jumping position.
            .gesture(DragGesture(minimumDistance: 0)
                .onChanged { g in
                    if !dragActive {
                        dragActive = true
                        if g.time.timeIntervalSince(lastStart) < 0.3 {
                            value = def
                            lastStart = .distantPast
                            return
                        }
                        lastStart = g.time
                    }
                    let f = (Double(g.location.x) / Double(w)).clamped(to: 0...1)
                    var v = range.lowerBound + f * span
                    v = (v / step).rounded() * step
                    value = v.clamped(to: range)
                }
                .onEnded { _ in dragActive = false })
        }
        .frame(height: height)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("slider")
        .accessibilityValue(String(format: "%.2f", value))
    }
}

/// Label + TrackSlider + monospaced value readout. Same call signature as the
/// old SliderRow so existing call sites keep working.
struct SliderRow: View {
    let title: String
    @Binding var value: Double
    let range: ClosedRange<Double>
    var step: Double = 0.01
    var reset: Double? = nil

    init(_ title: String, _ value: Binding<Double>, _ range: ClosedRange<Double>,
         step: Double = 0.01, reset: Double? = nil) {
        self.title = title
        self._value = value
        self.range = range
        self.step = step
        self.reset = reset
    }

    private var def: Double {
        reset ?? (range.contains(0) ? 0 : range.lowerBound)
    }

    var body: some View {
        HStack(spacing: 7) {
            Text(title)
                .font(.system(size: 10.5, weight: .medium))
                .foregroundStyle(Ara.text2)
                .frame(width: 60, alignment: .leading)
                .lineLimit(1)
            TrackSlider(value: $value, range: range, step: step, reset: reset)
            Text(String(format: "%.2f", value))
                .font(.system(size: 10.5).monospacedDigit())
                .foregroundStyle(abs(value - def) < 1e-9 ? Ara.text3 : Ara.accent)
                .frame(width: 38, alignment: .trailing)
        }
        .frame(height: 20)
    }
}

/// Card panel replacing GroupBox: small-caps hairline-separated header,
/// optional collapse chevron when `expanded` is bound.
struct Panel<Content: View, Trailing: View>: View {
    let title: String
    var expanded: Binding<Bool>?
    let content: Content
    let trailing: Trailing

    init(_ title: String,
         expanded: Binding<Bool>? = nil,
         @ViewBuilder trailing: () -> Trailing = { EmptyView() },
         @ViewBuilder content: () -> Content) {
        self.title = title
        self.expanded = expanded
        self.trailing = trailing()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                if expanded != nil {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(Ara.text3)
                        .rotationEffect(.degrees(expanded?.wrappedValue == true ? 90 : 0))
                }
                Text(title.uppercased())
                    .font(.system(size: 9.5, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(Ara.text2)
                Spacer()
                trailing
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .contentShape(Rectangle())
            .onTapGesture {
                guard expanded != nil else { return }
                withAnimation(.easeInOut(duration: 0.15)) {
                    expanded?.wrappedValue.toggle()
                }
            }

            if expanded?.wrappedValue != false {
                VStack(spacing: 8) { content }
                    .padding(.horizontal, 10)
                    .padding(.bottom, 10)
                    .padding(.top, 2)
            }
        }
        .background(Ara.bg2)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Ara.hairline, lineWidth: 1))
    }
}

extension Panel where Trailing == EmptyView {
    init(_ title: String, expanded: Binding<Bool>? = nil,
         @ViewBuilder content: () -> Content) {
        self.init(title, expanded: expanded, trailing: { EmptyView() }, content: content)
    }
}

/// Capsule segmented control (replaces .pickerStyle(.segmented)).
struct SegPicker<T: Hashable>: View {
    let options: [(value: T, label: String)]
    @Binding var selection: T

    init(_ options: [(T, String)], selection: Binding<T>) {
        self.options = options
        self._selection = selection
    }

    var body: some View {
        HStack(spacing: 0) {
            ForEach(options, id: \.value) { opt in
                Button { selection = opt.value } label: {
                    Text(opt.label)
                        .font(.system(size: 10, weight: selection == opt.value ? .semibold : .regular))
                        .foregroundStyle(selection == opt.value ? .white : Ara.text2)
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 5)
                        .background(
                            Capsule().fill(selection == opt.value ? Ara.bg4 : .clear)
                                .overlay(Capsule().stroke(
                                    selection == opt.value ? Ara.border : .clear, lineWidth: 0.5))
                        )
                        .padding(2)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(1)
        .background(Ara.bg3)
        .clipShape(Capsule())
    }
}

struct AraPrimaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Color.black.opacity(0.85))
            .padding(.horizontal, 12).padding(.vertical, 5)
            .background(Capsule().fill(Ara.accent))
            .opacity(configuration.isPressed ? 0.75 : 1)
    }
}

struct AraSecondaryButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11, weight: .medium))
            .foregroundStyle(Ara.text1)
            .padding(.horizontal, 11).padding(.vertical, 5)
            .background(Capsule().fill(Ara.bg3)
                .overlay(Capsule().stroke(Ara.border, lineWidth: 0.5)))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

/// Small icon button for toolbars / action strips.
struct IconAction: View {
    let icon: String
    var label: String? = nil
    var active = false
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: icon).font(.system(size: 11, weight: .medium))
                if let label { Text(label).font(.system(size: 10.5, weight: .medium)) }
            }
            .foregroundStyle(active ? Ara.accent : Ara.text2)
            .padding(.horizontal, label != nil ? 8 : 6)
            .padding(.vertical, 5)
            .background(RoundedRectangle(cornerRadius: 6).fill(active ? Ara.accentSoft : Ara.bg3)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(active ? Ara.accent.opacity(0.4) : Ara.hairline, lineWidth: 0.5)))
        }
        .buttonStyle(.plain)
    }
}

/// Small tool button used inside panels ("Pick WB on image" etc).
struct ToolChip: View {
    let label: String
    var icon: String? = nil
    var active = false
    var action: () -> Void
    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                if let icon { Image(systemName: icon).font(.system(size: 9, weight: .bold)) }
                Text(label).font(.system(size: 10, weight: .medium))
            }
            .foregroundStyle(active ? Ara.accent : Ara.text1)
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(Capsule().fill(active ? Ara.accentSoft : Ara.bg3)
                .overlay(Capsule().stroke(active ? Ara.accent.opacity(0.5) : Ara.hairline, lineWidth: 0.5)))
        }
        .buttonStyle(.plain)
    }
}

extension Double {
    func clamped(to r: ClosedRange<Double>) -> Double { Swift.min(Swift.max(self, r.lowerBound), r.upperBound) }
}

extension CGFloat {
    func clampedTo01() -> CGFloat { Swift.min(Swift.max(self, 0), 1) }
    func clamped(to r: ClosedRange<CGFloat>) -> CGFloat { Swift.min(Swift.max(self, r.lowerBound), r.upperBound) }
}
