import SwiftUI

struct WaiverView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            Section {
                Button {
                    Task { await state.runWaiver() }
                } label: {
                    if state.isBusy("waiver") {
                        BusyLabel(text: "Scanning the wire…")
                    } else {
                        Label("Suggest waiver pickups", systemImage: "sparkle.magnifyingglass")
                    }
                }
                .disabled(state.isBusy("waiver"))
            }

            if let err = state.errors["waiver"] {
                ErrorBanner(message: err).listRowBackground(Color.clear)
            }

            if let r = state.waiver {
                Section("Candidates") {
                    ForEach(r.candidates) { c in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack(spacing: 8) {
                                Text("\(c.priority)")
                                    .font(.caption.bold())
                                    .frame(width: 20)
                                    .foregroundStyle(Brand.purple)
                                PlayerRow(
                                    player: c.player,
                                    trailing: String(format: "%.1f", c.metrics.adjustedNextWeek)
                                ) { selected = $0 }
                            }
                            if let d = c.dropCandidate {
                                Text("Drop \(d.player.name) (Δ\(d.netRosDelta, specifier: "%+.0f"))")
                                    .font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                }

                if !r.raw.isEmpty {
                    Section("Claude's notes") {
                        Text(r.raw).font(.callout)
                    }
                }
            } else if !state.isBusy("waiver") {
                ContentUnavailableView(
                    "No waiver report", systemImage: "figure.stand.line.dotted.figure.stand",
                    description: Text("Run a scan to see pickups worth a claim.")
                )
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Waiver")
    }
}

struct NewsView: View {
    @EnvironmentObject var state: AppState

    var body: some View {
        List {
            if state.snapshot.news.isEmpty {
                ContentUnavailableView(
                    "No headlines", systemImage: "newspaper",
                    description: Text("Pull to refresh.")
                )
            }
            ForEach(state.snapshot.news) { item in
                VStack(alignment: .leading, spacing: 4) {
                    Text(item.title).font(.callout)
                    Text(item.source).font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("News")
        .refreshable { await state.refresh() }
    }
}
