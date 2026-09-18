import Foundation

// Mirrors of the Rust wire types. Field names match the Rust serde output, so
// no CodingKeys are needed unless a name would shadow a Swift keyword.

struct Player: Codable, Identifiable, Hashable {
    let id: String
    let name: String
    let position: String
    let rosterSlot: String
    let team: String
    let projectedPoints: Double
    let avgPoints: Double
    let status: String
    let opponent: String?
    let byeWeek: Int?
    let news: [String]

    enum CodingKeys: String, CodingKey {
        case id, name, position, team, status, opponent, news
        case rosterSlot = "roster_slot"
        case projectedPoints = "projected_points"
        case avgPoints = "avg_points"
        case byeWeek = "bye_week"
    }

    /// Sleeper serves a portrait per player id. Team defenses use a team code
    /// as their id and have no portrait, hence the nil.
    var headshotURL: URL? {
        guard !id.isEmpty, Int(id) != nil else { return nil }
        return URL(string: "https://sleepercdn.com/content/nfl/players/\(id).jpg")
    }

    var isStarter: Bool {
        !["BE", "IR", "?"].contains(rosterSlot)
    }
}

struct Roster: Codable, Identifiable, Hashable {
    let teamId: String
    let teamName: String
    let owner: String?
    let players: [Player]
    let wins: Int
    let losses: Int
    let ties: Int
    let pointsFor: Double
    let pointsAgainst: Double

    var id: String { teamId }

    enum CodingKeys: String, CodingKey {
        case owner, players, wins, losses, ties
        case teamId = "team_id"
        case teamName = "team_name"
        case pointsFor = "points_for"
        case pointsAgainst = "points_against"
    }

    var record: String { "\(wins)-\(losses)-\(ties)" }
}

struct Matchup: Codable, Hashable {
    let week: Int
    let homeTeam: String
    let awayTeam: String
    let homeProjected: Double
    let awayProjected: Double
    let homeScore: Double
    let awayScore: Double

    enum CodingKeys: String, CodingKey {
        case week
        case homeTeam = "home_team"
        case awayTeam = "away_team"
        case homeProjected = "home_projected"
        case awayProjected = "away_projected"
        case homeScore = "home_score"
        case awayScore = "away_score"
    }
}

struct LeagueSettings: Codable, Hashable {
    let scoring: String
    let teamCount: Int

    enum CodingKeys: String, CodingKey {
        case scoring
        case teamCount = "team_count"
    }
}

struct NewsItem: Codable, Hashable, Identifiable {
    let title: String
    let summary: String
    let source: String
    let url: String
    let published: String

    var id: String { url.isEmpty ? title : url }
}

struct TrendingPlayer: Codable, Identifiable, Hashable {
    let player: Player
    let count: Int
    let direction: String

    var id: String { player.id }
}

struct Snapshot: Codable {
    let week: Int
    let season: String
    let teamName: String
    let roster: Roster?
    let allRosters: [Roster]
    let settings: LeagueSettings?
    let matchups: [Matchup]
    let news: [NewsItem]
    let trendingAdd: [TrendingPlayer]
    let trendingDrop: [TrendingPlayer]
    let lastError: String?
    let perfSeason: String
    let hasPerf: Bool

    enum CodingKeys: String, CodingKey {
        case week, season, roster, settings, matchups, news
        case teamName = "team_name"
        case allRosters = "all_rosters"
        case trendingAdd = "trending_add"
        case trendingDrop = "trending_drop"
        case lastError = "last_error"
        case perfSeason = "perf_season"
        case hasPerf = "has_perf"
    }

    static let empty = Snapshot(
        week: 0, season: "", teamName: "", roster: nil, allRosters: [],
        settings: nil, matchups: [], news: [], trendingAdd: [], trendingDrop: [],
        lastError: nil, perfSeason: "", hasPerf: false
    )

    /// This week's matchup for my team, if one is scheduled.
    var myMatchup: Matchup? {
        guard let me = roster?.teamName else { return nil }
        return matchups.first { $0.homeTeam == me || $0.awayTeam == me }
    }
}

// MARK: - Lineup

struct LineupSlot: Codable, Hashable {
    let slot: String
    let player: Player?
}

struct Lineup: Codable {
    let week: Int
    let starters: [LineupSlot]
    let bench: [Player]
    let projectedTotal: Double
    let reasoning: String

    enum CodingKeys: String, CodingKey {
        case week, starters, bench, reasoning
        case projectedTotal = "projected_total"
    }
}

// MARK: - Waiver

struct PlayerMetrics: Codable, Hashable {
    let player: Player
    let rosValue: Double
    let adjustedNextWeek: Double

    enum CodingKeys: String, CodingKey {
        case player
        case rosValue = "ros_value"
        case adjustedNextWeek = "adjusted_next_week"
    }
}

struct DropCandidate: Codable, Hashable {
    let player: Player
    let netRosDelta: Double

    enum CodingKeys: String, CodingKey {
        case player
        case netRosDelta = "net_ros_delta"
    }
}

struct WaiverCandidate: Codable, Identifiable, Hashable {
    let priority: Int
    let player: Player
    let metrics: PlayerMetrics
    let dropCandidate: DropCandidate?

    var id: String { player.id }

    enum CodingKeys: String, CodingKey {
        case priority, player, metrics
        case dropCandidate = "drop_candidate"
    }
}

struct WaiverReport: Codable {
    let candidates: [WaiverCandidate]
    let raw: String
}

// MARK: - Trades

struct TradeStep: Codable, Hashable {
    let partner: String
    let send: [String]
    let receive: [String]
    let why: String
}

struct TradeIdea: Codable, Identifiable, Hashable {
    let headline: String
    let steps: [TradeStep]
    let why: String
    let risk: String

    var id: String { headline + steps.map(\.partner).joined() }

    var isMultiTier: Bool { steps.count > 1 }

    /// Whether the legs genuinely hand off, mirroring `TradeIdea::is_chained`
    /// in Rust. A model asked for a chain will sometimes return two unrelated
    /// trades under a chain-sounding headline; those are labelled differently.
    var isChained: Bool {
        var acquired: Set<String> = []
        for step in steps {
            if step.send.contains(where: { acquired.contains($0) }) { return true }
            acquired.formUnion(step.receive)
        }
        return false
    }

    var shapeLabel: String? {
        guard steps.count > 1 else { return nil }
        return isChained ? "\(steps.count)-step chain" : "\(steps.count) independent trades"
    }

    /// Players that actually leave the roster once the chain completes.
    var netSend: [String] {
        let acquired = Set(steps.flatMap(\.receive))
        var seen: Set<String> = []
        return steps.flatMap(\.send).filter { name in
            if acquired.contains(name), !seen.contains(name) {
                seen.insert(name)
                return false
            }
            return true
        }
    }

    /// Players that actually stay once the chain completes.
    var netReceive: [String] {
        let sent = Set(steps.flatMap(\.send))
        return steps.flatMap(\.receive).filter { !sent.contains($0) }
    }
}

struct TradeScanResult: Codable {
    let ideas: [TradeIdea]
    let raw: String
}

// MARK: - League comparison

struct PosRank: Codable, Identifiable, Hashable {
    let position: String
    let starters: Double
    let bench: Double
    let rankStarters: Int
    let rankBench: Int
    let teams: Int
    let leagueAvgStarters: Double
    let starterScore: Double
    let vsAverage: Double
    let isWeakness: Bool
    let isStrength: Bool

    var id: String { position }

    enum CodingKeys: String, CodingKey {
        case position, starters, bench, teams
        case rankStarters = "rank_starters"
        case rankBench = "rank_bench"
        case leagueAvgStarters = "league_avg_starters"
        case starterScore = "starter_score"
        case vsAverage = "vs_average"
        case isWeakness = "is_weakness"
        case isStrength = "is_strength"
    }

    var rankOrdinal: String { PosRank.ordinal(rankStarters) }

    static func ordinal(_ n: Int) -> String {
        let suffix: String
        switch (n % 10, n % 100) {
        case (_, 11), (_, 12), (_, 13): suffix = "th"
        case (1, _): suffix = "st"
        case (2, _): suffix = "nd"
        case (3, _): suffix = "rd"
        default: suffix = "th"
        }
        return "\(n)\(suffix)"
    }
}

// MARK: - Player detail

struct UpcomingGame: Codable, Hashable, Identifiable {
    let week: Int
    let opponent: String
    let home: Bool
    let date: String?

    var id: Int { week }
    var label: String { home ? "vs \(opponent)" : "@ \(opponent)" }
}

struct PerfRecord: Codable, Hashable {
    let games: Int
    let beat: Int
    let totalProj: Double
    let totalActual: Double
    let bestDiff: Double
    let worstDiff: Double

    enum CodingKeys: String, CodingKey {
        case games, beat
        case totalProj = "total_proj"
        case totalActual = "total_actual"
        case bestDiff = "best_diff"
        case worstDiff = "worst_diff"
    }

    var beatPct: Double { games == 0 ? 0 : 100 * Double(beat) / Double(games) }
    var avgProj: Double { games == 0 ? 0 : totalProj / Double(games) }
    var avgActual: Double { games == 0 ? 0 : totalActual / Double(games) }
    var avgDiff: Double { avgActual - avgProj }
}

struct PlayerDetail: Codable {
    let player: Player
    let owner: String?
    let upcoming: [UpcomingGame]
    let perf: PerfRecord?
    let perfSeason: String
    let totals: [[TotalValue]]
    let trendingAdd: Int?
    let trendingDrop: Int?
    let news: [String]

    enum CodingKeys: String, CodingKey {
        case player, owner, upcoming, perf, totals, news
        case perfSeason = "perf_season"
        case trendingAdd = "trending_add"
        case trendingDrop = "trending_drop"
    }

    /// Rust sends `Vec<(String, f64)>`, which is JSON `[["Games", 3], …]`.
    /// Swift has no heterogeneous tuple decoding, so each pair arrives as a
    /// two-element array of this enum and is flattened here.
    enum TotalValue: Codable, Hashable {
        case label(String)
        case number(Double)

        init(from decoder: Decoder) throws {
            let c = try decoder.singleValueContainer()
            if let s = try? c.decode(String.self) { self = .label(s) }
            else { self = .number(try c.decode(Double.self)) }
        }

        func encode(to encoder: Encoder) throws {
            var c = encoder.singleValueContainer()
            switch self {
            case .label(let s): try c.encode(s)
            case .number(let d): try c.encode(d)
            }
        }
    }

    var statLines: [(String, Double)] {
        totals.compactMap { pair in
            guard pair.count == 2,
                  case .label(let l) = pair[0],
                  case .number(let v) = pair[1] else { return nil }
            return (l, v)
        }
    }
}

// MARK: - Settings

struct AppConfig: Codable {
    var username: String
    var leagueId: String
    var apiKey: String
    var model: String
    var maxTokens: Int
    var strategy: String
    var newsSources: [String]
    var apiKeyFromEnv: Bool

    enum CodingKeys: String, CodingKey {
        case username, model, strategy
        case leagueId = "league_id"
        case apiKey = "api_key"
        case maxTokens = "max_tokens"
        case newsSources = "news_sources"
        case apiKeyFromEnv = "api_key_from_env"
    }

    var asDictionary: [String: Any] {
        [
            "username": username,
            "league_id": leagueId,
            "api_key": apiKey,
            "model": model,
            "max_tokens": maxTokens,
            "strategy": strategy,
            "news_sources": newsSources,
            "api_key_from_env": apiKeyFromEnv,
        ]
    }

    static let empty = AppConfig(
        username: "", leagueId: "", apiKey: "", model: "claude-sonnet-4-6",
        maxTokens: 2048, strategy: "balanced", newsSources: [], apiKeyFromEnv: false
    )
}

struct DiscoveredLeague: Codable, Identifiable, Hashable {
    let leagueId: String
    let name: String
    let season: String
    let totalRosters: Int
    let scoring: String

    var id: String { leagueId }

    enum CodingKeys: String, CodingKey {
        case name, season, scoring
        case leagueId = "league_id"
        case totalRosters = "total_rosters"
    }
}
