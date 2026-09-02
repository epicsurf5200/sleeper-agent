import SwiftUI

struct LeagueView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            if let err = state.errors["ranks"] {
                ErrorBanner(message: err).listRowBackground(Color.clear)
            }

            if state.ranks.isEmpty {
                if state.isBusy("ranks") {
                    BusyLabel(text: "Comparing rosters…")
                } else {
                    ContentUnavailableView(
                        "Nothing to compare yet", systemImage: "chart.pie",
                        description: Text("Load your league first, then pull to refresh.")
                    )
                }
            } else {
                Section {
                    RadarChart(ranks: state.ranks)
                        .frame(height: 280)
                        .frame(maxWidth: .infinity)
                    Text("Outer edge is best in the league at that position, centre is last.")
                        .font(.caption2).foregroundStyle(.secondary)
                }

                let weak = state.ranks.filter(\.isWeakness)
                let strong = state.ranks.filter(\.isStrength)

                Section("Where to improve") {
                    if weak.isEmpty {
                        Text("No position sits in the bottom third.")
                            .foregroundStyle(.secondary).font(.callout)
                    } else {
                        ForEach(weak) { r in rankLine(r, Brand.bad) }
                    }
                }

                Section("Surplus to trade from") {
                    if strong.isEmpty {
                        Text("No position sits in the top third.")
                            .foregroundStyle(.secondary).font(.callout)
                    } else {
                        ForEach(strong) { r in rankLine(r, Brand.good) }
                    }
                }

                Section("All positions") {
                    ForEach(state.ranks) { r in
                        HStack {
                            Text(r.position).font(.callout.bold()).frame(width: 44, alignment: .leading)
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Starters \(r.starters, specifier: "%.1f") · \(r.rankOrdinal) of \(r.teams)")
                                    .font(.caption)
                                    .foregroundStyle(Brand.rank(r.rankStarters, of: r.teams))
                                Text("Bench \(r.bench, specifier: "%.1f") · \(PosRank.ordinal(r.rankBench))")
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Text(String(format: "%+.1f", r.vsAverage))
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(r.vsAverage >= 0 ? Brand.good : Brand.bad)
                        }
                    }
                }

                if let me = state.snapshot.roster {
                    Section("Your starters by position") {
                        ForEach(state.ranks) { r in
                            let players = me.players
                                .filter { $0.position == r.position && $0.isStarter }
                                .sorted { $0.projectedPoints > $1.projectedPoints }
                            if !players.isEmpty {
                                ForEach(players) { p in
                                    PlayerRow(player: p,
                                              trailing: String(format: "%.1f", p.projectedPoints)) {
                                        selected = $0
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("League")
        .task { if state.ranks.isEmpty { await state.loadRanks() } }
        .refreshable {
            await state.refresh()
            await state.loadRanks()
        }
    }

    private func rankLine(_ r: PosRank, _ colour: Color) -> some View {
        HStack {
            Text(r.position).font(.callout.bold()).frame(width: 44, alignment: .leading)
            Text("\(r.rankOrdinal) of \(r.teams)")
            Spacer()
            Text(String(format: "%+.1f vs avg", r.vsAverage)).font(.caption.monospacedDigit())
        }
        .foregroundStyle(colour)
        .font(.callout)
    }
}

/// Radar of starter strength per position.
///
/// Plots league *rank*, not raw points: positions score on wildly different
/// scales, so a points-based radar would mostly show the scoring system rather
/// than the team. Mirrors `radar_chart` in the desktop app.
struct RadarChart: View {
    let ranks: [PosRank]

    var body: some View {
        GeometryReader { geo in
            let side = min(geo.size.width, geo.size.height)
            let centre = CGPoint(x: geo.size.width / 2, y: geo.size.height / 2)
            // Leave room for the labels around the outside.
            let radius = side / 2 - 28
            let n = ranks.count

            if n >= 3 {
                ZStack {
                    // Grid rings.
                    ForEach(1...4, id: \.self) { step in
                        polygon(centre: centre, radius: radius * CGFloat(step) / 4, n: n)
                            .stroke(Brand.stroke, lineWidth: 1)
                    }
                    // Spokes.
                    Path { p in
                        for i in 0..<n {
                            p.move(to: centre)
                            p.addLine(to: point(centre, radius, i, n))
                        }
                    }
                    .stroke(Brand.stroke, lineWidth: 1)

                    // The team's shape.
                    shapePath(centre: centre, radius: radius, n: n)
                        .fill(Brand.purple.opacity(0.35))
                    shapePath(centre: centre, radius: radius, n: n)
                        .stroke(Brand.purple, lineWidth: 2)

                    // Vertex dots and axis labels.
                    ForEach(Array(ranks.enumerated()), id: \.offset) { i, r in
                        let v = point(centre, radius * CGFloat(max(0, min(1, r.starterScore))), i, n)
                        Circle().fill(Brand.purple)
                            .frame(width: 6, height: 6)
                            .position(v)
                        Text(r.position)
                            .font(.caption2.bold())
                            .foregroundStyle(Brand.text)
                            .position(point(centre, radius + 16, i, n))
                    }
                }
            }
        }
    }

    /// Angle for axis `i`, starting at the top and going clockwise.
    private func point(_ centre: CGPoint, _ r: CGFloat, _ i: Int, _ n: Int) -> CGPoint {
        let a = 2 * Double.pi * Double(i) / Double(n) - Double.pi / 2
        return CGPoint(x: centre.x + r * cos(a), y: centre.y + r * sin(a))
    }

    private func polygon(centre: CGPoint, radius: CGFloat, n: Int) -> Path {
        Path { p in
            for i in 0..<n {
                let v = point(centre, radius, i, n)
                if i == 0 { p.move(to: v) } else { p.addLine(to: v) }
            }
            p.closeSubpath()
        }
    }

    private func shapePath(centre: CGPoint, radius: CGFloat, n: Int) -> Path {
        Path { p in
            for (i, r) in ranks.enumerated() {
                let v = point(centre, radius * CGFloat(max(0, min(1, r.starterScore))), i, n)
                if i == 0 { p.move(to: v) } else { p.addLine(to: v) }
            }
            p.closeSubpath()
        }
    }
}
