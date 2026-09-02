import Foundation

/// Swift face of the Rust core.
///
/// The whole surface is one async `call` that sends a JSON request and decodes
/// the JSON reply. The C callback cannot capture Swift context, so each in
/// flight request parks its continuation in a box, hands the box across as an
/// opaque pointer, and reclaims it in the callback.
actor SACore {
    /// Thrown when the core reports a failure, or the reply cannot be decoded.
    struct CoreError: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    private var engine: OpaquePointer?

    /// Carries one request's continuation across the C boundary.
    private final class Pending {
        let resume: (Result<Data, Error>) -> Void
        init(_ resume: @escaping (Result<Data, Error>) -> Void) { self.resume = resume }
    }

    init() throws {
        let fm = FileManager.default
        // Application Support is the right home for config: backed up, not
        // user-visible, and not purgeable the way Caches is.
        let support = try fm.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ).appendingPathComponent("sleeper-agent", isDirectory: true)
        // The player DB and headshots are re-downloadable, so they belong in
        // Caches where the OS may reclaim them under storage pressure.
        let caches = try fm.url(
            for: .cachesDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true
        ).appendingPathComponent("sleeper-agent", isDirectory: true)

        try fm.createDirectory(at: support, withIntermediateDirectories: true)
        try fm.createDirectory(at: caches, withIntermediateDirectories: true)

        guard let e = sa_engine_new(support.path, caches.path) else {
            throw CoreError(message: "Could not start the sleeper-agent core.")
        }
        engine = e
    }

    deinit {
        if let e = engine {
            sa_engine_free(e)
        }
    }

    var version: String {
        guard let c = sa_version() else { return "unknown" }
        return String(cString: c)
    }

    /// Send a request and decode the `data` payload as `T`.
    func call<T: Decodable>(_ request: [String: Any], as type: T.Type) async throws -> T {
        let data = try await raw(request)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw CoreError(message: "Could not read the reply: \(error.localizedDescription)")
        }
    }

    /// Send a request whose payload is not needed.
    @discardableResult
    func call(_ request: [String: Any]) async throws -> Data {
        try await raw(request)
    }

    /// Send the request and hand back the `data` payload as raw JSON.
    private func raw(_ request: [String: Any]) async throws -> Data {
        guard let engine else {
            throw CoreError(message: "The core is not running.")
        }
        let body = try JSONSerialization.data(withJSONObject: request)
        guard let json = String(data: body, encoding: .utf8) else {
            throw CoreError(message: "Could not encode the request.")
        }

        let payload: Data = try await withCheckedThrowingContinuation { continuation in
            // Retained here, released in the callback exactly once.
            let box = Unmanaged.passRetained(Pending { result in
                continuation.resume(with: result)
            })
            json.withCString { cstr in
                sa_request(
                    engine,
                    cstr,
                    box.toOpaque()
                ) { ctx, response in
                    guard let ctx else { return }
                    let pending = Unmanaged<Pending>.fromOpaque(ctx).takeRetainedValue()
                    guard let response else {
                        pending.resume(.failure(CoreError(message: "Empty reply from the core.")))
                        return
                    }
                    // The C string dies when this callback returns, so copy now.
                    let text = String(cString: response)
                    pending.resume(.success(Data(text.utf8)))
                }
            }
        }

        return try unwrap(payload)
    }

    /// Split `{"ok":true,"data":…}` from `{"ok":false,"error":…}`.
    private func unwrap(_ data: Data) throws -> Data {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw CoreError(message: "Malformed reply from the core.")
        }
        if let ok = obj["ok"] as? Bool, ok == false {
            throw CoreError(message: obj["error"] as? String ?? "Unknown error.")
        }
        guard let inner = obj["data"] else {
            throw CoreError(message: "Reply had no payload.")
        }
        // `data` can be any JSON value, so re-encode rather than assuming a
        // dictionary — league ranks come back as a top-level array.
        return try JSONSerialization.data(
            withJSONObject: inner, options: [.fragmentsAllowed]
        )
    }
}
