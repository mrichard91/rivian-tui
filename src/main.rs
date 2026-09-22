mod api;
mod app;
mod config;
mod db;
mod mqtt;
mod tui;
mod vehicle_art;
mod view_model;
mod web;

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;

use api::auth::{authenticated_headers, AuthManager};
use api::client::{RivianClient, CONTENT_URL, GATEWAY_URL, ORDERS_URL, T2D_URL};
use api::queries;
use app::{App, LogLevel, Mode};
use config::AppConfig;
use mqtt::MqttPublisher;

#[derive(Parser)]
#[command(
    name = "rivian-tui",
    about = "Terminal UI dashboard for Rivian vehicles"
)]
struct Cli {
    /// Enable debug mode (shows full request/response data)
    #[arg(long, short)]
    debug: bool,

    /// Poll interval in seconds (minimum 30s to avoid hammering the API)
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(30..))]
    poll_interval: u64,

    /// Dump raw vehicle state JSON to stdout and exit (no TUI)
    #[arg(long)]
    stdout: bool,

    /// Run a custom GraphQL query (use with --stdout)
    #[arg(long)]
    query: Option<String>,

    /// GraphQL endpoint for --stdout: gateway (default), charging, orders, content
    #[arg(long, default_value = "gateway")]
    endpoint: String,

    /// Optional path to a config TOML file (defaults to ~/.config/rivian-tui/config.toml)
    #[arg(long)]
    config: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.stdout {
        return run_stdout(&cli).await;
    }

    let config = AppConfig::load(cli.config.as_deref())?;

    // Bind the web server (if enabled) before entering the alternate screen so
    // any bind failure lands on the real terminal, not mid-TUI.
    let web_listener = if let Some(web_cfg) = config.enabled_web() {
        Some(web::bind(web_cfg).await?)
    } else {
        None
    };

    // Any panic past this point would otherwise leave the terminal in raw
    // mode + alternate screen with the panic message invisible. Restore the
    // terminal before the default hook prints.
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
        default_panic_hook(info);
    }));

    // Terminal setup
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run_tui(&mut terminal, &cli, config, web_listener).await;

    // Restore terminal
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Dump vehicle state (or custom query) to stdout as JSON
async fn run_stdout(cli: &Cli) -> Result<()> {
    let tokens = AuthManager::load_tokens()?
        .context("No saved credentials. Run the TUI first to log in.")?;

    let client = RivianClient::new()?;

    let headers = authenticated_headers(&tokens);

    let (op_name, query_str, variables) = if let Some(custom_query) = &cli.query {
        let vars = stdout_variables(custom_query, &tokens.vehicle_id);
        // Extract operation name from query (e.g., "query GetVehicleState(...)" -> "GetVehicleState")
        let op_name = custom_query
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.split('(').next())
            .unwrap_or("CustomQuery");
        (op_name, custom_query.as_str(), vars)
    } else {
        (
            "GetVehicleState",
            queries::GET_VEHICLE_STATE,
            Some(serde_json::json!({ "vehicleID": tokens.vehicle_id })),
        )
    };

    let url = match cli.endpoint.as_str() {
        "gateway" => GATEWAY_URL,
        "charging" => api::client::CHARGING_URL,
        "orders" => ORDERS_URL,
        "content" => CONTENT_URL,
        "t2d" => T2D_URL,
        other => {
            eprintln!("Unknown endpoint: {other}. Use: gateway, charging, orders, content, t2d");
            std::process::exit(1);
        }
    };

    let result: serde_json::Value = client
        .graphql(url, op_name, query_str, variables, Some(headers))
        .await?;

    println!("{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

/// Variables for a `--stdout --query` run. The gateway schema names the
/// vehicle variable `$vehicleID` and the charging schema `$vehicleId`; inject
/// whichever the query declares so both endpoints work without typing the
/// VIN on the command line.
fn stdout_variables(query: &str, vehicle_id: &str) -> Option<serde_json::Value> {
    if query.contains("$vehicleID") {
        Some(serde_json::json!({ "vehicleID": vehicle_id }))
    } else if query.contains("$vehicleId") {
        Some(serde_json::json!({ "vehicleId": vehicle_id }))
    } else {
        None
    }
}

async fn run_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: &Cli,
    config: AppConfig,
    web_listener: Option<(tokio::net::TcpListener, std::net::SocketAddr)>,
) -> Result<()> {
    // Build the app first so MQTT can use its event channel to surface
    // broker/publish errors as activity-log entries.
    let mut app = App::new(cli.debug, None);

    if let Some(mqtt_config) = config.enabled_mqtt() {
        let mqtt = MqttPublisher::start(mqtt_config.clone(), app.event_tx.clone())?;
        app.mqtt = Some(mqtt);
        app.log(
            LogLevel::Info,
            &format!("MQTT publishing enabled: {}", mqtt_config.broker_label()),
        );
    }

    // Launch the optional web dashboard. The listener was bound in `main()`
    // before we entered the alternate screen so any error was already
    // reported.
    if let Some((listener, addr)) = web_listener {
        let shared = app.shared_data_handle();
        // Cap the browser refresh at 60s regardless of the vehicle-state poll
        // interval: live-charging data updates every 60s, and a page that
        // reloads every 5 minutes would render that cadence invisible.
        let refresh_interval = cli.poll_interval.min(60);
        app.log(LogLevel::Info, &web_listen_message(addr));
        tokio::spawn(async move {
            if let Err(e) = web::serve(listener, shared, refresh_interval).await {
                eprintln!("web server error: {e}");
            }
        });
    }

    app.poll_interval_secs = cli.poll_interval;
    app.try_load_auth();

    // If we loaded tokens, kick off the session bundle. Login and vehicle
    // selection call the same `start_session` from the event handler, so
    // there is no separate post-login fetch path here.
    if app.tokens.is_some() {
        app.start_session();
    }

    let tick_rate = Duration::from_millis(200);

    loop {
        // Draw
        terminal.draw(|f| tui::draw(f, &app))?;

        // Drain background events
        app.drain_events();

        // Auto-poll on interval when authenticated and on dashboard. A poll
        // already in flight is skipped; `App` restarts the timer whenever a
        // request actually goes out, including the post-login bundle.
        if app.mode == Mode::Dashboard && app.poll_due() {
            app.poll_vehicle_state();
        }

        // While the vehicle is actively charging, poll the live-session
        // endpoint on a faster cadence than the full vehicle-state fetch so
        // kW / SOC / energy delivered update at a reasonable rate. The
        // vehicle-state poll alone can be 5+ minutes apart, which is too
        // slow to feel "live".
        const LIVE_POLL_INTERVAL_SECS: u64 = 60;
        let live_charging = app
            .vehicle_state
            .as_ref()
            .map(|vs| vs.is_actively_charging())
            .unwrap_or(false);
        let live_due = app
            .last_live_fetch
            .map(|t| t.elapsed().as_secs() >= LIVE_POLL_INTERVAL_SECS)
            .unwrap_or(true);
        if app.mode == Mode::Dashboard && app.tokens.is_some() && live_charging && live_due {
            app.fetch_live_session();
        }

        // Handle input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                handle_key(&mut app, key);

                if app.should_quit {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// Activity-log line announcing the web dashboard. The page has no auth and
/// shows live location, so say so when it's reachable beyond this machine.
fn web_listen_message(addr: std::net::SocketAddr) -> String {
    if addr.ip().is_loopback() {
        format!("Web dashboard listening on http://{addr}/")
    } else {
        format!(
            "Web dashboard listening on http://{addr}/ — reachable from the network \
             with no authentication (location, VIN, lock state)"
        )
    }
}

/// Apply one key press to the app for whichever screen is active.
fn handle_key(app: &mut App, key: KeyEvent) {
    // Raw mode delivers Ctrl+C as a key event instead of SIGINT, so honour
    // it here on every screen.
    let chord = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    match app.mode {
        Mode::Dashboard => {
            if app.show_debug_detail {
                match key.code {
                    KeyCode::Char('d') | KeyCode::Esc => {
                        app.show_debug_detail = false;
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Char('r') => {
                        if !app.poll_vehicle_state() && app.tokens.is_some() {
                            app.log(LogLevel::Info, "Refresh already in progress");
                        }
                    }
                    KeyCode::Char('L') => {
                        app.logout();
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        app.scroll_log_down();
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        app.scroll_log_up();
                    }
                    KeyCode::Char('l') => {
                        app.show_log = !app.show_log;
                    }
                    KeyCode::Char('d') if app.debug => {
                        if let Some(entry) = app.activity_log.get(app.log_selected) {
                            if entry.detail.is_some() {
                                app.show_debug_detail = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        Mode::Login => match key.code {
            KeyCode::Esc => {
                app.should_quit = true;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                app.next_login_field();
            }
            KeyCode::Enter => {
                app.start_login();
            }
            KeyCode::Backspace => {
                app.active_login_input().pop();
            }
            KeyCode::Char(c) if !chord => {
                app.active_login_input().push(c);
            }
            _ => {}
        },
        Mode::MfaPrompt => match key.code {
            KeyCode::Esc => {
                app.cancel_auth_flow();
            }
            KeyCode::Enter => {
                app.submit_otp();
            }
            KeyCode::Backspace => {
                app.login_otp.pop();
            }
            KeyCode::Char(c) if !chord => {
                app.login_otp.push(c);
            }
            _ => {}
        },
        Mode::VehicleSelect => match key.code {
            KeyCode::Esc => {
                app.cancel_auth_flow();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                app.select_vehicle_down();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.select_vehicle_up();
            }
            KeyCode::Enter => {
                app.confirm_vehicle_selection();
            }
            _ => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_str(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn web_listen_message_warns_when_reachable_from_the_network() {
        let lan: std::net::SocketAddr = "0.0.0.0:8787".parse().unwrap();
        let local: std::net::SocketAddr = "127.0.0.1:8787".parse().unwrap();
        assert!(web_listen_message(lan).contains("no authentication"));
        assert!(!web_listen_message(local).contains("no authentication"));
    }

    #[test]
    fn login_form_is_usable_after_backing_out_of_mfa() {
        let mut app = App::new(false, None);
        app.mode = Mode::MfaPrompt;
        type_str(&mut app, "12");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Login);

        type_str(&mut app, "me@example.com");
        press(&mut app, KeyCode::Tab);
        type_str(&mut app, "pw");

        assert_eq!(app.login_email, "me@example.com");
        assert_eq!(app.login_password, "pw");
    }

    #[test]
    fn logout_returns_focus_to_the_email_field() {
        let _auth = api::auth::AuthTestContext::new();
        let mut app = App::new(false, None);
        app.mode = Mode::Login;
        press(&mut app, KeyCode::Tab); // focus Password, as when submitting
        app.mode = Mode::Dashboard;

        press(&mut app, KeyCode::Char('L'));
        type_str(&mut app, "me@example.com");

        assert_eq!(app.login_email, "me@example.com");
        assert!(app.login_password.is_empty());
    }

    #[test]
    fn ctrl_c_quits_from_any_screen_without_typing_a_c() {
        for mode in [
            Mode::Dashboard,
            Mode::Login,
            Mode::MfaPrompt,
            Mode::VehicleSelect,
        ] {
            let mut app = App::new(false, None);
            app.mode = mode.clone();
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            );
            assert!(app.should_quit, "Ctrl+C must quit from {mode:?}");
            assert!(app.login_email.is_empty() && app.login_otp.is_empty());
        }
    }

    #[test]
    fn control_chords_are_not_typed_into_login_fields() {
        let mut app = App::new(false, None);
        app.mode = Mode::Login;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
        );
        assert!(app.login_email.is_empty());
    }

    #[test]
    fn stdout_injects_whichever_vehicle_variable_the_query_declares() {
        // Gateway queries use `$vehicleID`; charging queries use `$vehicleId`.
        let gateway = "query Q($vehicleID: String!) { vehicleState(id: $vehicleID) { batteryLevel { value } } }";
        let charging =
            "query L($vehicleId: ID!) { getLiveSessionData(vehicleId: $vehicleId) { chargerId } }";
        let neither = "query getUserInfo { currentUser { vehicles { id } } }";

        assert_eq!(
            stdout_variables(gateway, "VIN1"),
            Some(serde_json::json!({ "vehicleID": "VIN1" }))
        );
        assert_eq!(
            stdout_variables(charging, "VIN1"),
            Some(serde_json::json!({ "vehicleId": "VIN1" }))
        );
        assert_eq!(stdout_variables(neither, "VIN1"), None);
    }
}
