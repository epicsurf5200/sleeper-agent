import SwiftUI

struct LineupView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            Section {
                Button {
                    Task { await state.generateLineup() }
                } label: {
                    if state.isBusy("lineup") {
                        BusyLabel(text: "Asking Claude…")
                    } else {
                        Label("Generate AI lineup", systemImage: "wand.and.stars")
                    }
                }
                .disabled(state.isBusy("lineup"))
            }

            if let err = state.errors["lineup"] {
                ErrorBanner(message: err).listRowBackground(Color.clear)
            }

            if let l = state.lineup {
                Section("Week \(l.week) · projected \(l.projectedTotal, specifier: "%.1f")") {
                    ForEach(Array(l.starters.enumerated()), id: \.offset) { _, slot in
                        HStack(spacing: 10) {
                            Text(slot.slot)
                                .font(.caption.bold())
                                .frame(width: 46, alignment: .leading)
                                .foregroundStyle(.secondary)
                            if let p = slot.player {
                                PlayerRow(player: p,
                                          trailing: String(format: "%.1f", p.projectedPoints)) {
                                    selected = $0
                                }
                            } else {
                                Text("(empty)").foregroundStyle(.secondary)
                            }
                        }
                    }
                }

                Section("Reasoning") {
                    Text(l.reasoning).font(.callout)
                }
            } else if !state.isBusy("lineup") {
                ContentUnavailableView(
                    "No lineup yet", systemImage: "wand.and.stars",
                    description: Text("Generate one to see Claude's picks and reasoning.")
                )
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Lineup")
    }
}
