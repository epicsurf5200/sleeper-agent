import SwiftUI

struct MatchupView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        Group {
            if let m = state.snapshot.myMatchup, let me = state.snapshot.roster {
                content(m, me)
            } else {
                ContentUnavailableView(
                    "No matchup", systemImage: "sportscourt",
                    description: Text("Nothing scheduled for week \(state.snapshot.week) yet.")
                )
            }
        }
        .brandBackground()
        .navigationTitle("Matchup")
        .refreshable { await state.refresh() }
    }

    @ViewBuilder
    private func content(_ m: Matchup, _ me: Roster) -> some View {
        let iAmHome = m.homeTeam == me.teamName
        let myProj = iAmHome ? m.homeProjected : m.awayProjected
        let oppProj = iAmHome ? m.awayProjected : m.homeProjected
        let myScore = iAmHome ? m.homeScore : m.awayScore
        let oppScore = iAmHome ? m.awayScore : m.homeScore
        let oppName = iAmHome ? m.awayTeam : m.homeTeam
        let opp = state.snapshot.allRosters.first { $0.teamName == oppName }
        let diff = myProj - oppProj
        let winPct = MatchupView.winProbability(margin: diff)

        List {
            Section {
                HStack(alignment: .top) {
                    side(me.teamName, myProj, myScore, highlight: true)
                    Spacer()
                    Text("vs").foregroundStyle(.secondary).padding(.top, 18)
                    Spacer()
                    side(oppName, oppProj, oppScore, highlight: false)
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text(diff >= 0
                         ? String(format: "Favoured by %.1f — %.0f%% to win", diff, winPct)
                         : String(format: "Underdog by %.1f — %.0f%% to win", -diff, winPct))
                        .font(.callout.bold())
                        .foregroundStyle(diff >= 0 ? Brand.good : Brand.bad)
                    ProgressView(value: min(max(winPct / 100, 0), 1))
                        .tint(diff >= 0 ? Brand.good : Brand.bad)
                    // State the assumption rather than presenting a bare number
                    // as if it were precise.
                    Text("Assumes a ~26 pt standard deviation on the weekly margin.")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }

            Section(me.teamName) {
                ForEach(me.players.filter(\.isStarter)) { p in
                    PlayerRow(player: p,
                              trailing: String(format: "%.1f", p.projectedPoints)) {
                        selected = $0
                    }
                }
            }

            Section(oppName) {
                if let opp {
                    ForEach(opp.players.filter(\.isStarter)) { p in
                        PlayerRow(player: p,
                                  trailing: String(format: "%.1f", p.projectedPoints)) {
                            selected = $0
                        }
                    }
                } else {
                    Text("Opponent roster not loaded yet.").foregroundStyle(.secondary)
                }
            }
        }
        .scrollContentBackground(.hidden)
    }

    private func side(_ name: String, _ proj: Double, _ score: Double,
                      highlight: Bool) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(name).font(.caption.bold()).lineLimit(2)
            Text(String(format: "%.1f", proj))
                .font(.system(size: 30, weight: .semibold))
                .foregroundStyle(highlight ? Brand.purple : Brand.text)
            Text(String(format: "live %.1f", score))
                .font(.caption2).foregroundStyle(.secondary)
        }
        .frame(maxWidth: 130, alignment: .leading)
    }

    /// Chance of winning given a projected margin, matching `win_probability`
    /// in the desktop app so the two never disagree.
    static func winProbability(margin: Double) -> Double {
        let sigma = 26.0
        return 50.0 * (1.0 + erf(margin / (sigma * 2.0.squareRoot())))
    }
}
