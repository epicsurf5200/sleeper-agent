import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var state: AppState
    @State private var showKey = false

    private let models = ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"]
    private let strategies = [
        ("conservative", "Conservative"),
        ("balanced", "Balanced"),
        ("high_stakes", "High stakes"),
    ]

    var body: some View {
        Form {
            Section("Account") {
                TextField("Sleeper username", text: $state.config.username)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()

                Button {
                    Task { await state.discoverLeagues(username: state.config.username) }
                } label: {
                    if state.isBusy("leagues") {
                        BusyLabel(text: "Looking up leagues…")
                    } else {
                        Label("Find my leagues", systemImage: "magnifyingglass")
                    }
                }
                .disabled(state.config.username.isEmpty || state.isBusy("leagues"))

                if !state.leagues.isEmpty {
                    Picker("League", selection: $state.config.leagueId) {
                        Text("Auto-detect").tag("")
                        ForEach(state.leagues) { l in
                            Text("\(l.name) (\(l.totalRosters))").tag(l.leagueId)
                        }
                    }
                }
            }

            Section {
                Picker("Strategy", selection: $state.config.strategy) {
                    ForEach(strategies, id: \.0) { key, label in
                        Text(label).tag(key)
                    }
                }
            } header: {
                Text("Strategy")
            } footer: {
                Text("Adjusts both the local scoring model and the guidance given to Claude.")
            }

            Section {
                HStack {
                    Group {
                        if showKey {
                            TextField("sk-ant-…", text: $state.config.apiKey)
                        } else {
                            SecureField("sk-ant-…", text: $state.config.apiKey)
                        }
                    }
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()

                    Button(showKey ? "Hide" : "Show") { showKey.toggle() }
                        .font(.caption)
                        .buttonStyle(.borderless)
                }

                Picker("Model", selection: $state.config.model) {
                    ForEach(models, id: \.self) { Text($0).tag($0) }
                }

                Stepper("Max tokens: \(state.config.maxTokens)",
                        value: $state.config.maxTokens, in: 256...8192, step: 256)
            } header: {
                Text("Claude")
            } footer: {
                // The single most important thing to explain on this platform.
                Text("iOS cannot run the Claude Code CLI, so an API key is required for "
                     + "AI features. Unlike the desktop app, your Pro/Max subscription "
                     + "cannot be used here — the key is billed per token.")
            }

            if let err = state.errors["save"] ?? state.errors["leagues"] {
                Section { ErrorBanner(message: err) }
            }

            Section {
                Button {
                    Task {
                        await state.saveConfig()
                        await state.connect()
                    }
                } label: {
                    if state.isBusy("save") || state.isBusy("connect") {
                        BusyLabel(text: "Saving…")
                    } else {
                        Label("Save and reconnect", systemImage: "checkmark.circle")
                    }
                }
                .disabled(state.isBusy("save"))
            } footer: {
                Text("Core \(state.coreVersion)\(state.config.apiKeyFromEnv ? " · key from environment, not saved to disk" : "")")
            }
        }
        .scrollContentBackground(.hidden)
        .brandBackground()
        .navigationTitle("Settings")
    }
}
