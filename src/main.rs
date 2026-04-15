mod api;
mod app;
mod config;
mod db;
mod mqtt;
mod tui;
mod view_model;
mod web;

use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
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

    /// Poll interval in seconds
    #[arg(long, default_value = "300")]
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
        // Auto-pass vehicleID variable if query declares it
        let vars = if custom_query.contains("$vehicleID") {
            Some(serde_json::json!({ "vehicleID": tokens.vehicle_id }))
        } else {
            None
        };
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

async fn run_tui(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: &Cli,
    config: AppConfig,
    web_listener: Option<(tokio::net::TcpListener, std::net::SocketAddr)>,
) -> Result<()> {
    let mqtt = config
        .enabled_mqtt()
        .cloned()
        .map(MqttPublisher::start)
        .transpose()?;
    let mut app = App::new(cli.debug, mqtt);

    if let Some(mqtt_config) = config.enabled_mqtt() {
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
        app.log(
            LogLevel::Info,
            &format!("Web dashboard listening on http://{addr}/"),
        );
        tokio::spawn(async move {
            if let Err(e) = web::serve(listener, shared).await {
                eprintln!("web server error: {e}");
            }
        });
    }

    app.poll_interval_secs = cli.poll_interval;
    app.try_load_auth();

    // If we loaded tokens, kick off initial fetches
    if app.tokens.is_some() {
        app.poll_vehicle_state();
        app.fetch_charging_history();
    }

    let mut last_poll = Instant::now();
    let tick_rate = Duration::from_millis(200);
    let mut needs_initial_fetch = false;

    loop {
        // Draw
        terminal.draw(|f| tui::draw(f, &app))?;

        // Drain background events
        let prev_mode = app.mode.clone();
        app.drain_events();

        // After successful login, trigger initial fetches
        if prev_mode != Mode::Dashboard && app.mode == Mode::Dashboard && !needs_initial_fetch {
            needs_initial_fetch = true;
        }

        if needs_initial_fetch && app.tokens.is_some() {
            app.poll_vehicle_state();
            app.fetch_charging_history();
            last_poll = Instant::now();
            needs_initial_fetch = false;
        }

        // Auto-poll on interval when authenticated and on dashboard
        if app.mode == Mode::Dashboard
            && app.tokens.is_some()
            && last_poll.elapsed().as_secs() >= app.poll_interval_secs
        {
            app.poll_vehicle_state();
            last_poll = Instant::now();
        }

        // Handle input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
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
                                    app.poll_vehicle_state();
                                    last_poll = Instant::now();
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
                        KeyCode::Char(c) => {
                            app.active_login_input().push(c);
                        }
                        _ => {}
                    },
                    Mode::MfaPrompt => {
                        app.login_field = app::LoginField::Otp;
                        match key.code {
                            KeyCode::Esc => {
                                app.cancel_auth_flow();
                            }
                            KeyCode::Enter => {
                                app.submit_otp();
                            }
                            KeyCode::Backspace => {
                                app.login_otp.pop();
                            }
                            KeyCode::Char(c) => {
                                app.login_otp.push(c);
                            }
                            _ => {}
                        }
                    }
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

                if app.should_quit {
                    break;
                }
            }
        }
    }

    Ok(())
}
