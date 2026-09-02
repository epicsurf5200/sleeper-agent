import Foundation
import SwiftUI

/// Single observable store the whole UI reads from.
///
/// Every AI action keeps its own busy flag and its own error, so a failed trade
/// scan does not blank the roster or block a lineup request running beside it.
@MainActor
final class AppState: ObservableObject {
    @Published var snapshot: Snapshot = .empty
    @Published var config: AppConfig = .empty
    @Published var leagues: [DiscoveredLeague] = []

    @Published var lineup: Lineup?
    @Published var waiver: WaiverReport?
    @Published var tradeIdeas: [TradeIdea] = []
    @Published var tradeRaw: String?
    @Published var ranks: [PosRank] = []
    @Published var trendWhy: [String: String] = [:]

    /// Keyed by action so several can run at once.
    @Published var busy: Set<String> = []
    @Published var errors: [String: String] = [:]
    @Published var connected = false
    @Published var coreVersion = ""

    private var core: SACore?

    func start() async {
        do {
            let c = try SACore()
            core = c
            coreVersion = await c.version
            config = try await c.call(["op": "get_config"], as: AppConfig.self)
            if !config.username.isEmpty {
                await connect()
            }
        } catch {
            errors["core"] = error.localizedDescription
        }
    }

    func isBusy(_ key: String) -> Bool { busy.contains(key) }

    /// Run an action under a busy key, routing failures to a per-key error.
    private func run(_ key: String, _ body: @escaping (SACore) async throws -> Void) async {
        guard let core else {
            errors[key] = "The core is not running."
            return
        }
        guard !busy.contains(key) else { return }
        busy.insert(key)
        errors[key] = nil
        do {
            try await body(core)
        } catch {
            errors[key] = error.localizedDescription
        }
        busy.remove(key)
    }

    func connect() async {
        await run("connect") { core in
            let snap = try await core.call(
                ["op": "connect",
                 "username": self.config.username,
                 "league_id": self.config.leagueId],
                as: Snapshot.self
            )
            self.snapshot = snap
            self.connected = true
        }
    }

    func refresh() async {
        await run("refresh") { core in
            self.snapshot = try await core.call(["op": "refresh"], as: Snapshot.self)
        }
    }

    func generateLineup() async {
        await run("lineup") { core in
            self.lineup = try await core.call(["op": "lineup"], as: Lineup.self)
        }
    }

    func runWaiver() async {
        await run("waiver") { core in
            self.waiver = try await core.call(["op": "waiver"], as: WaiverReport.self)
        }
    }

    func scanTrades(count: Int, multiTier: Bool, horizon: String,
                    sendHints: [String], wantPositions: [String]) async {
        await run("trade_scan") { core in
            let r = try await core.call(
                ["op": "trade_scan",
                 "count": count,
                 "multi_tier": multiTier,
                 "horizon": horizon,
                 "send_hints": sendHints,
                 "want_positions": wantPositions],
                as: TradeScanResult.self
            )
            self.tradeIdeas = r.ideas
            // Kept so an unparseable reply can be shown rather than swallowed.
            self.tradeRaw = r.ideas.isEmpty ? r.raw : nil
        }
    }

    func loadRanks() async {
        await run("ranks") { core in
            self.ranks = try await core.call(["op": "league_ranks"], as: [PosRank].self)
        }
    }

    func explainTrending(_ player: Player, direction: String) async {
        // Per-player key so several explanations can run side by side.
        await run("why-\(player.id)") { core in
            struct Reply: Codable { let text: String }
            let r = try await core.call(
                ["op": "trend_why", "player_id": player.id, "direction": direction],
                as: Reply.self
            )
            self.trendWhy[player.id] = r.text
        }
    }

    func playerDetail(_ id: String) async throws -> PlayerDetail {
        guard let core else { throw SACore.CoreError(message: "The core is not running.") }
        return try await core.call(["op": "player_detail", "player_id": id], as: PlayerDetail.self)
    }

    func discoverLeagues(username: String) async {
        await run("leagues") { core in
            self.leagues = try await core.call(
                ["op": "discover_leagues", "username": username],
                as: [DiscoveredLeague].self
            )
        }
    }

    func saveConfig() async {
        await run("save") { core in
            self.config = try await core.call(
                ["op": "save_config", "config": self.config.asDictionary],
                as: AppConfig.self
            )
        }
    }
}
