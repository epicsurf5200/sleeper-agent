import SwiftUI

struct TrendingView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            if state.snapshot.trendingAdd.isEmpty && state.snapshot.trendingDrop.isEmpty {
                ContentUnavailableView(
                    "Nothing trending", systemImage: "chart.line.uptrend.xyaxis",
                    description: Text("Pull to refresh.")
                )
            }

            if !state.snapshot.trendingAdd.isEmpty {
                Section("Most added (24h)") {
                    ForEach(state.snapshot.trendingAdd) { t in
                        row(t, direction: "add")
                    }
                }
            }

            if !state.snapshot.trendingDrop.isEmpty {
                Section("Most dropped (24h)") {
                    ForEach(state.snapshot.trendingDrop) { t in
                        row(t, direction: "drop")
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Trending")
        .refreshable { await state.refresh() }
    }

    @ViewBuilder
    private func row(_ t: TrendingPlayer, direction: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                PlayerRow(player: t.player, trailing: "\(t.count)") { selected = $0 }
                // Each player gets its own busy key, so several explanations
                // can run at once.
                if state.isBusy("why-\(t.player.id)") {
                    ProgressView().controlSize(.small)
                } else {
                    Button {
                        Task { await state.explainTrending(t.player, direction: direction) }
                    } label: {
                        Image(systemName: state.trendWhy[t.player.id] == nil
                              ? "questionmark.circle" : "arrow.clockwise")
                    }
                    .buttonStyle(.borderless)
                }
            }

            if let err = state.errors["why-\(t.player.id)"] {
                Text(err).font(.caption).foregroundStyle(Brand.bad)
            }

            if let why = state.trendWhy[t.player.id] {
                VStack(alignment: .leading, spacing: 6) {
                    Text(why).font(.caption)
                    Button("Dismiss") { state.trendWhy[t.player.id] = nil }
                        .font(.caption)
                        .buttonStyle(.borderless)
                }
                .padding(8)
                .background(Brand.bgLight)
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            }
        }
    }
}
