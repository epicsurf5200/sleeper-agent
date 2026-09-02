import SwiftUI

struct TradesView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    @State private var count = 3
    @State private var multiTier = false
    @State private var horizon = "rest_of_season"
    @State private var sendHint = ""
    @State private var want: Set<String> = []

    private let positions = ["QB", "RB", "WR", "TE", "K", "DST"]

    var body: some View {
        List {
            Section("Find trades") {
                Stepper("Suggestions: \(count)", value: $count, in: 1...8)

                Picker("Judge on", selection: $horizon) {
                    Text("This week").tag("this_week")
                    Text("Rest of season").tag("rest_of_season")
                }
                .pickerStyle(.segmented)

                TextField("Trade away (player or position)", text: $sendHint)
                    .textInputAutocapitalization(.words)
                    .autocorrectionDisabled()

                VStack(alignment: .leading, spacing: 6) {
                    Text("Looking for").font(.caption).foregroundStyle(.secondary)
                    // Wrapping chips: a segmented control would squeeze six
                    // positions into unreadable slivers on a phone.
                    HStack(spacing: 6) {
                        ForEach(positions, id: \.self) { pos in
                            Button {
                                if want.contains(pos) { want.remove(pos) } else { want.insert(pos) }
                            } label: {
                                Text(pos)
                                    .font(.caption.bold())
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 6)
                                    .background(want.contains(pos) ? Brand.purple : Brand.bgLight)
                                    .foregroundStyle(want.contains(pos) ? .black : Brand.text)
                                    .clipShape(Capsule())
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }

                Toggle("Allow chained trades", isOn: $multiTier)
                if multiTier {
                    Text("Each extra leg is another manager who has to agree.")
                        .font(.caption2).foregroundStyle(.secondary)
                }

                Button {
                    Task {
                        await state.scanTrades(
                            count: count, multiTier: multiTier, horizon: horizon,
                            sendHints: sendHint.split(separator: ",")
                                .map { $0.trimmingCharacters(in: .whitespaces) }
                                .filter { !$0.isEmpty },
                            wantPositions: Array(want)
                        )
                    }
                } label: {
                    if state.isBusy("trade_scan") {
                        BusyLabel(text: "Comparing every roster…")
                    } else {
                        Label("Scan league for trades", systemImage: "sparkle.magnifyingglass")
                    }
                }
                .disabled(state.isBusy("trade_scan"))
            }

            if let err = state.errors["trade_scan"] {
                ErrorBanner(message: err).listRowBackground(Color.clear)
            }

            ForEach(Array(state.tradeIdeas.enumerated()), id: \.offset) { i, idea in
                Section {
                    TradeCard(index: i, idea: idea, selected: $selected)
                }
            }

            if let raw = state.tradeRaw, state.tradeIdeas.isEmpty {
                // Structured parsing failed — show what came back rather than
                // silently reporting nothing.
                Section("Could not read that as structured trades") {
                    Text(raw).font(.caption.monospaced())
                }
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Trades")
    }
}

/// One proposal, rendered as a card rather than prose.
struct TradeCard: View {
    @EnvironmentObject var state: AppState
    let index: Int
    let idea: TradeIdea
    @Binding var selected: Player?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("#\(index + 1)").font(.caption).foregroundStyle(.secondary)
                Text(idea.headline.isEmpty ? "Trade idea" : idea.headline)
                    .font(.headline)
                Spacer()
            }
            // Derived from the steps, not from the model's headline: an idea
            // asked to chain sometimes comes back as two unrelated trades.
            if let shape = idea.shapeLabel {
                Text(shape).font(.caption).foregroundStyle(Brand.purple)
            }

            ForEach(Array(idea.steps.enumerated()), id: \.offset) { n, step in
                VStack(alignment: .leading, spacing: 6) {
                    Text(idea.isMultiTier
                         ? "Step \(n + 1) · with \(step.partner)"
                         : "With \(step.partner)")
                        .font(.caption.bold())

                    label("You send", step.send, Brand.bad)
                    label("You get", step.receive, Brand.good)

                    if !step.why.isEmpty {
                        Text(step.why).font(.caption).foregroundStyle(.secondary)
                    }
                }
            }

            if idea.isChained {
                Divider().overlay(Brand.stroke)
                Text("Net effect").font(.caption.bold())
                label("Out", idea.netSend, Brand.bad)
                label("In", idea.netReceive, Brand.good)
            }

            if !idea.why.isEmpty { Text(idea.why).font(.callout) }
            if !idea.risk.isEmpty {
                Text("Risk: \(idea.risk)").font(.caption).foregroundStyle(Brand.warn)
            }
        }
    }

    private func label(_ title: String, _ names: [String], _ colour: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption2).foregroundStyle(.secondary)
            ForEach(names, id: \.self) { name in
                chip(name, colour)
            }
        }
    }

    @ViewBuilder
    private func chip(_ name: String, _ colour: Color) -> some View {
        if let p = state.snapshot.allRosters
            .flatMap(\.players)
            .first(where: { $0.name.caseInsensitiveCompare(name) == .orderedSame }) {
            Button { selected = p } label: {
                HStack(spacing: 8) {
                    Headshot(player: p, size: 26)
                    Text(p.name).foregroundStyle(colour)
                    Text("\(p.position) \(p.team) · \(p.projectedPoints, specifier: "%.1f")")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
            .buttonStyle(.plain)
        } else {
            // A name the model invented must not look like a real player.
            Text("\(name)  (not on any roster)")
                .font(.callout.italic())
                .foregroundStyle(Brand.warn)
        }
    }
}
