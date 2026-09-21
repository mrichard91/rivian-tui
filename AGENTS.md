# rivian-tui

A Rust-based terminal UI dashboard for Rivian vehicles.

## Architecture

- **TUI**: ratatui 0.29 + crossterm 0.28, immediate-mode rendering at 200ms tick
- **Async**: tokio with background tasks communicating via unbounded channels
- **API**: GraphQL queries to Rivian's API (gateway for auth, api.rivian.com for data)
- **Auth**: OAuth flow with CSRF → Login → MFA(OTP) → token storage in OS keychain (`keyring` crate with the platform backend features enabled — without `apple-native` / `windows-native` / `sync-secret-service` the crate silently uses an in-memory mock and nothing persists). Linux builds need libdbus headers. `~/.config/rivian-tui/tokens.json` is written only if the keychain is unavailable.
- **Modules**: Flat structure under `src/`, API types in `src/api/`

## Module layout

- `main.rs` — entry point, event loop, key handling
- `app.rs` — app state, modes, background task dispatching
- `tui.rs` — all ratatui rendering (dashboard, login, MFA screens)
- `api/client.rs` — HTTP client wrapping reqwest for GraphQL
- `api/auth.rs` — authentication manager + keychain persistence
- `api/queries.rs` — GraphQL query/mutation strings
- `api/types.rs` — request/response serde types

## Build & run

```bash
cargo run
cargo check
```

## Key conventions

- Error handling: `anyhow::Result<T>` everywhere
- No secrets in the repo — auth tokens live in the OS keychain
- Headers mimic the iOS Rivian app for API compatibility
- Vehicle state fields use `Option<StateValue<T>>` pattern from the GraphQL API
- Every background fetch goes through `App::spawn_query` (token guard, generation tag, typed `ErrorSource`, session-expiry detection); the post-auth bundle is `App::start_session`
- Persisted vehicle-state columns live in one table (`db::VEHICLE_STATE_DATA_COLUMNS`) that drives CREATE, migration, and INSERT — add a row there, nowhere else
- Alert vocabulary: check `rivian.db` for observed tokens (`SELECT col, COUNT(*) ... GROUP BY col`) before adding a predicate; the API says `empty` not `low`, `ajar` as well as `open`, `close` not `closed`
- Status messages fade after 8 seconds
- Modal input: Dashboard, Login, MfaPrompt (more coming)

## Public repo

This will be pushed to github.com/mrichard91/rivian-tui as a public repo.
Never commit secrets, credentials, .env files, or auth tokens.
