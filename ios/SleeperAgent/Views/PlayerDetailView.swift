import SwiftUI

/// Everything known about one player. The desktop shows this in a side panel;
/// on a phone it is a sheet, but the content is the same.
struct PlayerDetailView: View {
    @EnvironmentObject var state: AppState
    @Environment(\.dismiss) private var dismiss
    let playerId: String

    @State private var detail: PlayerDetail?
    @State private var error: String?

    var body: some View {
        ScrollView {
            if let d = detail {
                content(d)
            } else if let error {
                ErrorBanner(message: error).padding()
            } else {
                BusyLabel(text: "Loading…").padding(40)
            }
        }
        .brandBackground()
        .navigationTitle(detail?.player.name ?? "Player")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button("Done") { dismiss() }
            }
        }
        .task {
            do { detail = try await state.playerDetail(playerId) }
            catch { self.error = error.localizedDescription }
        }
    }

    @ViewBuilder
    private func content(_ d: PlayerDetail) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            VStack(spacing: 8) {
                Headshot(player: d.player, size: 128)
                Text(d.player.name).font(.title2.bold())
                Text("\(d.player.position) · \(d.player.team) · \(d.player.status)")
                    .font(.subheadline).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)

            VStack(alignment: .leading, spacing: 6) {
                row("Projected",
                    String(format: "%.1f pts", d.player.projectedPoints),
                    color: Brand.purple)
                if d.player.avgPoints > 0 {
                    row("Season avg", String(format: "%.1f pts/game", d.player.avgPoints))
                }
                row("Roster slot", d.player.rosterSlot)
                if let o = d.player.opponent { row("This week", o) }
                if let b = d.player.byeWeek { row("Bye week", "\(b)") }
                row("Rostered by", d.owner ?? "Free agent")
                if let a = d.trendingAdd {
                    row("Trending", "+\(a) adds (24h)", color: Brand.good)
                }
                if let dr = d.trendingDrop {
                    row("Trending", "−\(dr) drops (24h)", color: Brand.bad)
                }
            }
            .card()

            if !d.upcoming.isEmpty {
                section("Upcoming games") {
                    ForEach(d.upcoming) { g in
                        HStack {
                            Text("Wk \(g.week)").foregroundStyle(.secondary)
                            Text(g.label)
                            Spacer()
                            if let date = g.date {
                                Text(date).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .font(.callout)
                    }
                }
            }

            section("Against projection") {
                if let p = d.perf, p.games > 0 {
                    // Name the season explicitly: in the preseason this is
                    // last year's record, not this year's.
                    Text("\(d.perfSeason) season · \(p.games) games")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Beat projection \(Int(p.beatPct))% of games (\(p.beat)/\(p.games))")
                        .font(.callout.bold())
                        .foregroundStyle(p.beatPct >= 50 ? Brand.good : Brand.bad)
                    ProgressView(value: min(max(p.beatPct / 100, 0), 1))
                        .tint(p.beatPct >= 50 ? Brand.good : Brand.bad)
                    Text(String(format: "Average %+.1f vs projection (%.1f actual, %.1f projected)",
                                p.avgDiff, p.avgActual, p.avgProj))
                        .font(.caption)
                    Text(String(format: "Best %+.1f · worst %+.1f", p.bestDiff, p.worstDiff))
                        .font(.caption).foregroundStyle(.secondary)

                    let lines = d.statLines
                    if !lines.isEmpty {
                        Divider().overlay(Brand.stroke)
                        Text("\(d.perfSeason) totals").font(.caption.bold())
                        ForEach(lines, id: \.0) { label, value in
                            HStack {
                                Text(label)
                                Spacer()
                                Text(String(format: "%.0f", value))
                            }
                            .font(.caption)
                        }
                    }
                } else {
                    Text("No completed games on record yet.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            }

            section("News") {
                if d.news.isEmpty {
                    Text("No recent headlines mention this player.")
                        .font(.callout).foregroundStyle(.secondary)
                } else {
                    ForEach(d.news.prefix(8), id: \.self) { h in
                        Text("• \(h)").font(.callout)
                    }
                }
            }
        }
        .padding()
    }

    private func row(_ label: String, _ value: String, color: Color = Brand.text) -> some View {
        HStack {
            Text(label).foregroundStyle(.secondary)
            Spacer()
            Text(value).foregroundStyle(color)
        }
        .font(.callout)
    }

    private func section<C: View>(_ title: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.headline)
            content()
        }
        .card()
    }
}
