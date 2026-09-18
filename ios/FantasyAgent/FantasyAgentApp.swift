import SwiftUI

@main
struct FantasyAgentApp: App {
    @StateObject private var state = AppState()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(state)
                // The desktop app is dark-only and the palette is built for it,
                // so pin the scheme rather than half-supporting light mode.
                .preferredColorScheme(.dark)
                .tint(Brand.purple)
                .task { await state.start() }
        }
    }
}

struct RootView: View {
    @EnvironmentObject var state: AppState
    /// Player whose detail sheet is showing. On a phone this is a sheet rather
    /// than the desktop's side panel — there is no room to split the screen.
    @State private var selected: Player?

    var body: some View {
        TabView {
            NavigationStack { RosterView(selected: $selected) }
                .tabItem { Label("Roster", systemImage: "person.3.fill") }

            NavigationStack { MatchupView(selected: $selected) }
                .tabItem { Label("Matchup", systemImage: "sportscourt.fill") }

            NavigationStack { LineupView(selected: $selected) }
                .tabItem { Label("Lineup", systemImage: "wand.and.stars") }

            NavigationStack { TradesView(selected: $selected) }
                .tabItem { Label("Trades", systemImage: "arrow.left.arrow.right") }

            NavigationStack { MoreView(selected: $selected) }
                .tabItem { Label("More", systemImage: "ellipsis.circle.fill") }
        }
        .brandBackground()
        .sheet(item: $selected) { player in
            NavigationStack {
                PlayerDetailView(playerId: player.id)
            }
            .presentationDetents([.large])
        }
    }
}

/// Overflow for the tabs that do not fit the bar. iOS shows at most five, and
/// the app has nine screens, so the less-used ones live behind a list.
struct MoreView: View {
    @EnvironmentObject var state: AppState
    @Binding var selected: Player?

    var body: some View {
        List {
            Section {
                NavigationLink { WaiverView(selected: $selected) } label: {
                    Label("Waiver", systemImage: "figure.stand.line.dotted.figure.stand")
                }
                NavigationLink { TrendingView(selected: $selected) } label: {
                    Label("Trending", systemImage: "chart.line.uptrend.xyaxis")
                }
                NavigationLink { LeagueView(selected: $selected) } label: {
                    Label("League", systemImage: "chart.pie.fill")
                }
                NavigationLink { NewsView() } label: {
                    Label("News", systemImage: "newspaper.fill")
                }
            }
            Section {
                NavigationLink { SettingsView() } label: {
                    Label("Settings", systemImage: "gearshape.fill")
                }
            } footer: {
                Text("Core \(state.coreVersion)")
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("More")
    }
}
