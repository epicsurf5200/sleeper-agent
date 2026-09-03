import SwiftUI

struct RosterView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            if let err = state.errors["connect"] ?? state.errors["core"] {
                ErrorBanner(message: err).listRowBackground(Color.clear)
            }

            if let r = state.snapshot.roster {
                Section {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(r.teamName).font(.headline)
                            Text("\(r.record) · PF \(r.pointsFor, specifier: "%.1f")")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        VStack(alignment: .trailing, spacing: 2) {
                            Text("Week \(state.snapshot.week)").font(.caption)
                            Text(state.snapshot.season).font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Section("Starters") {
                    ForEach(r.players.filter(\.isStarter)) { p in
                        PlayerRow(player: p,
                                  trailing: String(format: "%.1f", p.projectedPoints)) {
                            selected = $0
                        }
                    }
                }

                let bench = r.players.filter { !$0.isStarter }
                if !bench.isEmpty {
                    Section("Bench") {
                        ForEach(bench) { p in
                            PlayerRow(player: p,
                                      trailing: String(format: "%.1f", p.projectedPoints)) {
                                selected = $0
                            }
                        }
                    }
                }
            } else if state.isBusy("connect") {
                BusyLabel(text: "Connecting to your league…")
            } else if state.config.username.isEmpty {
                // First launch: point at the one thing that must happen next.
                ContentUnavailableView {
                    Label("Not set up yet", systemImage: "person.crop.circle.badge.plus")
                } description: {
                    Text("Add your Sleeper username in Settings to get started.")
                }
            } else {
                ContentUnavailableView(
                    "No roster", systemImage: "tray",
                    description: Text("Pull to refresh once your league has drafted.")
                )
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Roster")
        .refreshable { await state.refresh() }
        .toolbar {
            if state.isBusy("refresh") {
                ToolbarItem(placement: .topBarTrailing) {
                    ProgressView().controlSize(.small)
                }
            }
        }
    }
}
