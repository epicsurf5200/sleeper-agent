import SwiftUI

/// The desktop app's palette, taken from assets/logo-mark.svg so the two look
/// like the same product. Values mirror the `BRAND_*` constants in src/gui.rs.
enum Brand {
    static let bg = Color(red: 0x23 / 255, green: 0x25 / 255, blue: 0x32 / 255)
    static let bgLight = Color(red: 0x2b / 255, green: 0x2d / 255, blue: 0x3a / 255)
    static let stroke = Color(red: 0x4a / 255, green: 0x4d / 255, blue: 0x5a / 255)
    static let purple = Color(red: 0x91 / 255, green: 0x84 / 255, blue: 0xd9 / 255)
    static let text = Color(red: 0xe9 / 255, green: 0xe9 / 255, blue: 0xed / 255)

    static let good = Color(red: 0.56, green: 0.87, blue: 0.56)
    static let bad = Color(red: 0.94, green: 0.5, blue: 0.5)
    static let warn = Color(red: 0.86, green: 0.86, blue: 0.47)

    /// Colour for an injury/availability tag.
    static func status(_ s: String) -> Color {
        switch s {
        case "OUT", "IR", "SUSP": return bad
        case "D": return warn
        case "Q": return Color(red: 0.86, green: 0.86, blue: 0.47)
        default: return text
        }
    }

    /// Green in the top third of the league, red in the bottom third.
    static func rank(_ rank: Int, of teams: Int) -> Color {
        guard teams >= 3 else { return .gray }
        if rank * 3 <= teams { return good }
        if rank * 3 > teams * 2 { return bad }
        return warn
    }
}

/// Card container used throughout, matching the desktop app's grouped frames.
struct CardModifier: ViewModifier {
    func body(content: Content) -> some View {
        content
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Brand.bgLight)
            .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .stroke(Brand.stroke, lineWidth: 1)
            )
    }
}

extension View {
    func card() -> some View { modifier(CardModifier()) }

    /// Standard screen chrome: brand background edge to edge.
    func brandBackground() -> some View {
        background(Brand.bg.ignoresSafeArea())
    }
}

/// Player portrait with a neutral placeholder, since team defenses have none
/// and a real headshot takes a moment to arrive.
struct Headshot: View {
    let player: Player
    var size: CGFloat = 36

    var body: some View {
        Group {
            if let url = player.headshotURL {
                AsyncImage(url: url) { phase in
                    switch phase {
                    case .success(let image):
                        image.resizable().aspectRatio(contentMode: .fill)
                    default:
                        placeholder
                    }
                }
            } else {
                placeholder
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().stroke(Brand.stroke, lineWidth: 1))
    }

    private var placeholder: some View {
        ZStack {
            Brand.bgLight
            Text(initials)
                .font(.system(size: size * 0.36, weight: .semibold))
                .foregroundStyle(Brand.stroke)
        }
    }

    private var initials: String {
        let parts = player.name.split(separator: " ")
        return parts.prefix(2).compactMap { $0.first.map(String.init) }.joined()
    }
}

/// A tappable player row shared by every list, so a player behaves the same
/// way everywhere he appears.
struct PlayerRow: View {
    let player: Player
    var trailing: String?
    var onTap: (Player) -> Void

    var body: some View {
        Button {
            onTap(player)
        } label: {
            HStack(spacing: 10) {
                Headshot(player: player, size: 36)
                VStack(alignment: .leading, spacing: 2) {
                    Text(player.name)
                        .font(.body)
                        .foregroundStyle(Brand.status(player.status))
                    Text("\(player.position) · \(player.team)\(player.status == "OK" ? "" : " · \(player.status)")")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if let trailing {
                    Text(trailing)
                        .font(.callout.monospacedDigit())
                        .foregroundStyle(Brand.text)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// Consistent inline spinner + label for the AI actions.
struct BusyLabel: View {
    let text: String
    var body: some View {
        HStack(spacing: 8) {
            ProgressView().controlSize(.small)
            Text(text).font(.callout).foregroundStyle(.secondary)
        }
    }
}

/// Error banner. AI and network failures are common enough that they deserve
/// a consistent, non-modal presentation.
struct ErrorBanner: View {
    let message: String
    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: "exclamationmark.triangle.fill")
            Text(message).font(.callout)
        }
        .foregroundStyle(Brand.bad)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Brand.bad.opacity(0.12))
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}
