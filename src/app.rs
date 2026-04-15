use std::sync::{Arc, RwLock};

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::api::auth::{authenticated_headers, AuthManager, LoginOutcome, PendingVehicleSelection};
use crate::api::client::{RequestLog, RivianClient, API_URL, CHARGING_URL};
use crate::api::queries;
use crate::api::types::*;
use crate::db::{ChargeSessionSummary, Db, VehicleTrendPoint};
use crate::mqtt::MqttPublisher;

/// Cap on retained activity log entries. The oldest entries are dropped once
/// this threshold is exceeded so the log cannot grow without bound during a
/// long-running session.
pub const MAX_LOG_ENTRIES: usize = 500;

/// Snapshot of all dashboard-relevant state, shared between the TUI and the
/// optional web server via an `Arc<RwLock<_>>`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DashboardData {
    pub vehicle_state: Option<VehicleStateFields>,
    pub recent_trend: Vec<VehicleTrendPoint>,
    pub last_charge_session: Option<ChargeSessionSummary>,
    pub last_update: Option<DateTime<Utc>>,
    pub vehicle_id: Option<String>,
}

pub type SharedDashboardData = Arc<RwLock<DashboardData>>;

/// UI mode / active screen
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Dashboard,
    Login,
    MfaPrompt,
    VehicleSelect,
}

/// Input field currently focused during login
#[derive(Debug, Clone, PartialEq)]
pub enum LoginField {
    Email,
    Password,
    Otp,
}

#[derive(Debug, Clone)]
pub enum LogLevel {
    Info,
    Error,
    Debug,
}

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Local>,
    pub level: LogLevel,
    pub message: String,
    /// Debug detail (request/response bodies, headers)
    pub detail: Option<String>,
}

/// Events sent from background tasks to the main loop
pub enum AppEvent {
    VehicleState {
        generation: u64,
        state: Box<VehicleStateFields>,
    },
    AuthSuccess {
        generation: u64,
        tokens: AuthTokens,
    },
    MfaRequired {
        generation: u64,
        mfa: MfaState,
    },
    VehicleSelectionRequired {
        generation: u64,
        pending: PendingVehicleSelection,
    },
    Error {
        generation: u64,
        msg: String,
    },
    Log {
        generation: u64,
        entry: LogEntry,
    },
    RequestLog {
        generation: u64,
        req_log: RequestLog,
    },
    ChargingSessions {
        generation: u64,
        sessions: Vec<ChargingSession>,
    },
}

impl AppEvent {
    fn generation(&self) -> u64 {
        match self {
            Self::VehicleState { generation, .. }
            | Self::AuthSuccess { generation, .. }
            | Self::MfaRequired { generation, .. }
            | Self::VehicleSelectionRequired { generation, .. }
            | Self::Error { generation, .. }
            | Self::Log { generation, .. }
            | Self::RequestLog { generation, .. }
            | Self::ChargingSessions { generation, .. } => *generation,
        }
    }
}

pub struct App {
    pub mode: Mode,
    pub should_quit: bool,
    pub debug: bool,

    // Auth
    pub tokens: Option<AuthTokens>,
    pub mfa_state: Option<MfaState>,
    pub pending_vehicle_selection: Option<PendingVehicleSelection>,

    // Login form
    pub login_email: String,
    pub login_password: String,
    pub login_otp: String,
    pub login_field: LoginField,
    pub login_error: Option<String>,
    pub login_busy: bool,
    pub vehicle_selection_index: usize,

    // Vehicle data
    pub vehicle_state: Option<VehicleStateFields>,
    pub recent_trend: Vec<VehicleTrendPoint>,
    pub last_charge_session: Option<ChargeSessionSummary>,
    pub last_update: Option<DateTime<Utc>>,
    pub poll_interval_secs: u64,

    // Activity log
    pub activity_log: Vec<LogEntry>,
    pub log_scroll: usize,
    pub log_selected: usize,
    pub show_debug_detail: bool,
    pub show_log: bool,

    // Database
    pub db: Option<Db>,
    pub mqtt: Option<MqttPublisher>,
    pub db_snapshot_count: i64,
    pub generation: u64,

    // Channel for receiving background events
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,

    // Shared snapshot for out-of-process readers (e.g. the web server). Kept
    // in sync with the owned fields above whenever dashboard state changes.
    pub shared_data: SharedDashboardData,
}

impl App {
    pub fn new(debug: bool, mqtt: Option<MqttPublisher>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            mode: Mode::Dashboard,
            should_quit: false,
            debug,

            tokens: None,
            mfa_state: None,
            pending_vehicle_selection: None,

            login_email: String::new(),
            login_password: String::new(),
            login_otp: String::new(),
            login_field: LoginField::Email,
            login_error: None,
            login_busy: false,
            vehicle_selection_index: 0,

            vehicle_state: None,
            recent_trend: Vec::new(),
            last_charge_session: None,
            last_update: None,
            poll_interval_secs: 300,

            activity_log: Vec::new(),
            log_scroll: 0,
            log_selected: 0,
            show_debug_detail: false,
            show_log: false,

            db: None,
            mqtt,
            db_snapshot_count: 0,
            generation: 0,

            event_tx,
            event_rx,
            shared_data: Arc::new(RwLock::new(DashboardData::default())),
        }
    }

    /// Copy the current dashboard-relevant fields into the shared snapshot so
    /// other readers (web server, etc.) can observe the latest state.
    fn sync_shared_data(&self) {
        let snapshot = DashboardData {
            vehicle_state: self.vehicle_state.clone(),
            recent_trend: self.recent_trend.clone(),
            last_charge_session: self.last_charge_session.clone(),
            last_update: self.last_update,
            vehicle_id: self.tokens.as_ref().map(|t| t.vehicle_id.clone()),
        };
        if let Ok(mut guard) = self.shared_data.write() {
            *guard = snapshot;
        }
    }

    /// Handle to the shared dashboard snapshot. Clone this to pass to
    /// background tasks (e.g. the web server).
    pub fn shared_data_handle(&self) -> SharedDashboardData {
        Arc::clone(&self.shared_data)
    }

    /// Build a RivianClient wired to our event channel
    fn make_client(
        debug: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        generation: u64,
    ) -> Result<RivianClient, anyhow::Error> {
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<RequestLog>();
        let app_tx = event_tx.clone();

        // Forward request logs to app events
        tokio::spawn(async move {
            while let Some(req_log) = log_rx.recv().await {
                let _ = app_tx.send(AppEvent::RequestLog {
                    generation,
                    req_log,
                });
            }
        });

        RivianClient::new().map(|c| c.with_debug(debug).with_logger(log_tx))
    }

    fn focus_last_log(&mut self) {
        if self.activity_log.is_empty() {
            self.log_scroll = 0;
            self.log_selected = 0;
            return;
        }

        let visible = 10;
        self.log_selected = self.activity_log.len() - 1;
        self.log_scroll = self.log_selected.saturating_sub(visible - 1);
    }

    /// Drop oldest log entries so the buffer stays within `MAX_LOG_ENTRIES`.
    /// Adjusts scroll/selection indices to remain consistent with the new
    /// length.
    fn trim_activity_log(&mut self) {
        if self.activity_log.len() <= MAX_LOG_ENTRIES {
            return;
        }
        let drop = self.activity_log.len() - MAX_LOG_ENTRIES;
        self.activity_log.drain(..drop);
        self.log_selected = self.log_selected.saturating_sub(drop);
        self.log_scroll = self.log_scroll.saturating_sub(drop);
    }

    pub fn log(&mut self, level: LogLevel, msg: &str) {
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: None,
        });
        self.trim_activity_log();
        self.focus_last_log();
    }

    fn log_with_detail(&mut self, level: LogLevel, msg: &str, detail: String) {
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: Some(detail),
        });
        self.trim_activity_log();
        self.focus_last_log();
    }

    fn refresh_dashboard_insights(&mut self) {
        let Some(vehicle_id) = self.tokens.as_ref().map(|tokens| tokens.vehicle_id.clone()) else {
            self.recent_trend.clear();
            self.last_charge_session = None;
            self.sync_shared_data();
            return;
        };
        let Some(db) = &self.db else {
            self.recent_trend.clear();
            self.last_charge_session = None;
            self.sync_shared_data();
            return;
        };

        let trend_result = db.recent_vehicle_trend(&vehicle_id, 24);
        let charge_result = db.latest_charging_session(&vehicle_id);

        match trend_result {
            Ok(points) => {
                self.recent_trend = points;
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Trend load failed: {e}"));
            }
        }

        match charge_result {
            Ok(session) => {
                self.last_charge_session = session;
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Charge summary load failed: {e}"));
            }
        }

        self.sync_shared_data();
    }

    /// Initialize database and load auth tokens on startup
    pub fn try_load_auth(&mut self) {
        match Db::open() {
            Ok(db) => {
                let count = db.snapshot_count().unwrap_or(0);
                self.db_snapshot_count = count;
                self.db = Some(db);
                self.log(
                    LogLevel::Info,
                    &format!("Database ready ({count} snapshots)"),
                );
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Database failed: {e}"));
            }
        }

        match AuthManager::load_tokens() {
            Ok(Some(tokens)) => {
                let vid = tokens.vehicle_id.clone();
                self.tokens = Some(tokens);
                self.refresh_dashboard_insights();
                self.log(
                    LogLevel::Info,
                    &format!("Loaded credentials (vehicle: {vid})"),
                );
            }
            Ok(None) => {
                self.mode = Mode::Login;
                self.log(
                    LogLevel::Info,
                    "No saved credentials in keychain — please log in",
                );
            }
            Err(e) => {
                self.mode = Mode::Login;
                self.log(LogLevel::Error, &format!("Auth load error: {e}"));
            }
        }
    }

    /// Drain all pending events from background tasks
    pub fn drain_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            if event.generation() != self.generation {
                continue;
            }

            match event {
                AppEvent::VehicleState { state, .. } => {
                    let vehicle_id = self
                        .tokens
                        .as_ref()
                        .map(|t| t.vehicle_id.clone())
                        .unwrap_or_else(|| "unknown".into());

                    if let Some(db) = &self.db {
                        match db.insert_state(&vehicle_id, &state) {
                            Ok(_) => {
                                self.db_snapshot_count =
                                    db.snapshot_count().unwrap_or(self.db_snapshot_count);
                            }
                            Err(e) => {
                                self.log(LogLevel::Error, &format!("DB write failed: {e}"));
                            }
                        }
                    }

                    if let Some(mqtt) = &self.mqtt {
                        if let Err(e) = mqtt.publish_vehicle_state(&vehicle_id, &state) {
                            self.log(LogLevel::Error, &format!("MQTT publish failed: {e}"));
                        }
                    }

                    self.vehicle_state = Some(*state);
                    self.last_update = Some(Utc::now());
                    self.refresh_dashboard_insights();
                    self.log(
                        LogLevel::Info,
                        &format!(
                            "Vehicle state updated ({}  snapshots recorded)",
                            self.db_snapshot_count
                        ),
                    );
                }
                AppEvent::AuthSuccess { tokens, .. } => {
                    self.tokens = Some(tokens);
                    self.mfa_state = None;
                    self.pending_vehicle_selection = None;
                    self.mode = Mode::Dashboard;
                    self.login_busy = false;
                    self.login_error = None;
                    self.login_password.clear();
                    self.login_otp.clear();
                    self.refresh_dashboard_insights();
                    self.log(
                        LogLevel::Info,
                        "Login successful — fetching vehicle state...",
                    );
                }
                AppEvent::MfaRequired { mfa, .. } => {
                    self.mfa_state = Some(mfa);
                    self.mode = Mode::MfaPrompt;
                    self.login_busy = false;
                    self.log(
                        LogLevel::Info,
                        "MFA required — enter the OTP code sent to your device",
                    );
                }
                AppEvent::VehicleSelectionRequired { pending, .. } => {
                    self.pending_vehicle_selection = Some(pending);
                    self.login_busy = false;
                    self.login_error = None;
                    self.vehicle_selection_index = 0;
                    self.mode = Mode::VehicleSelect;
                    self.log(
                        LogLevel::Info,
                        "Multiple vehicles found — choose a vehicle to continue",
                    );
                }
                AppEvent::Error { msg, .. } => {
                    self.login_busy = false;
                    self.login_error = Some(msg.clone());
                    self.log(LogLevel::Error, &msg);
                }
                AppEvent::Log { entry, .. } => {
                    self.activity_log.push(entry);
                    self.trim_activity_log();
                    self.focus_last_log();
                }
                AppEvent::RequestLog { req_log, .. } => {
                    let status_str = req_log
                        .status
                        .map(|s| format!("{s}"))
                        .unwrap_or_else(|| "???".into());
                    let summary = format!(
                        "{} -> {} {}ms",
                        req_log.operation, status_str, req_log.duration_ms
                    );

                    if let Some(err) = &req_log.error {
                        self.log(LogLevel::Error, &format!("{summary} ({err})"));
                    } else if self.debug {
                        let mut detail = String::new();
                        if let Some(hdrs) = &req_log.request_headers {
                            detail.push_str("--- Request Headers ---\n");
                            detail.push_str(hdrs);
                            detail.push('\n');
                        }
                        if let Some(body) = &req_log.request_body {
                            detail.push_str("--- Request Body ---\n");
                            // Pretty-print if possible
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                                detail.push_str(
                                    &serde_json::to_string_pretty(&v)
                                        .unwrap_or_else(|_| body.clone()),
                                );
                            } else {
                                detail.push_str(body);
                            }
                            detail.push('\n');
                        }
                        if let Some(resp) = &req_log.response_body {
                            detail.push_str("--- Response Body ---\n");
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(resp) {
                                detail.push_str(
                                    &serde_json::to_string_pretty(&v)
                                        .unwrap_or_else(|_| resp.clone()),
                                );
                            } else {
                                detail.push_str(resp);
                            }
                        }
                        self.log_with_detail(LogLevel::Debug, &summary, detail);
                    } else {
                        self.log(LogLevel::Info, &summary);
                    }
                }
                AppEvent::ChargingSessions { sessions, .. } => {
                    let new_sessions = if let Some(db) = &self.db {
                        match db.upsert_charging_sessions(&sessions) {
                            Ok(new_sessions) => {
                                let total = db.charging_session_count().unwrap_or(0);
                                self.log(
                                    LogLevel::Info,
                                    &format!(
                                        "Charging history: {} new sessions ({total} total)",
                                        new_sessions.len()
                                    ),
                                );
                                new_sessions
                            }
                            Err(e) => {
                                self.log(
                                    LogLevel::Error,
                                    &format!("DB charging write failed: {e}"),
                                );
                                Vec::new()
                            }
                        }
                    } else {
                        sessions.clone()
                    };

                    if let Some(mqtt) = &self.mqtt {
                        let vehicle_id = self
                            .tokens
                            .as_ref()
                            .map(|tokens| tokens.vehicle_id.as_str())
                            .unwrap_or("unknown");
                        let publish_sessions = if self.db.is_some() {
                            &new_sessions
                        } else {
                            &sessions
                        };

                        for session in publish_sessions {
                            if let Err(e) = mqtt.publish_charging_session(vehicle_id, session) {
                                self.log(LogLevel::Error, &format!("MQTT publish failed: {e}"));
                                break;
                            }
                        }
                    }

                    self.refresh_dashboard_insights();
                }
            }
        }
    }

    /// Kick off login in a background task
    pub fn start_login(&mut self) {
        if self.login_busy {
            return;
        }
        self.generation += 1;
        self.login_busy = true;
        self.login_error = None;
        self.log(LogLevel::Info, "Logging in...");

        let email = self.login_email.clone();
        let password = self.login_password.clone();
        let tx = self.event_tx.clone();
        let debug = self.debug;
        let generation = self.generation;

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx, generation) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: e.to_string(),
                    });
                    return;
                }
            };
            let auth_mgr = AuthManager::new(client);

            let _ = tx.send(AppEvent::Log {
                generation,
                entry: LogEntry {
                    timestamp: Local::now(),
                    level: LogLevel::Info,
                    message: "Fetching CSRF token...".into(),
                    detail: None,
                },
            });

            match auth_mgr.login(&email, &password).await {
                Ok(LoginOutcome::Success(tokens)) => {
                    let _ = tx.send(AppEvent::AuthSuccess { generation, tokens });
                }
                Ok(LoginOutcome::MfaRequired(mfa)) => {
                    let _ = tx.send(AppEvent::MfaRequired { generation, mfa });
                }
                Ok(LoginOutcome::VehicleSelectionRequired(pending)) => {
                    let _ = tx.send(AppEvent::VehicleSelectionRequired {
                        generation,
                        pending,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: format!("Login failed: {e}"),
                    });
                }
            }
        });
    }

    /// Submit OTP code for MFA
    pub fn submit_otp(&mut self) {
        if self.login_busy {
            return;
        }
        let Some(mfa) = self.mfa_state.clone() else {
            return;
        };
        self.login_busy = true;
        self.login_error = None;
        self.log(LogLevel::Info, "Verifying OTP...");

        let otp = self.login_otp.clone();
        let tx = self.event_tx.clone();
        let debug = self.debug;
        let generation = self.generation;

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx, generation) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: e.to_string(),
                    });
                    return;
                }
            };
            let auth_mgr = AuthManager::new(client);

            match auth_mgr.complete_mfa(&mfa, &otp).await {
                Ok(LoginOutcome::Success(tokens)) => {
                    let _ = tx.send(AppEvent::AuthSuccess { generation, tokens });
                }
                Ok(LoginOutcome::VehicleSelectionRequired(pending)) => {
                    let _ = tx.send(AppEvent::VehicleSelectionRequired {
                        generation,
                        pending,
                    });
                }
                Ok(LoginOutcome::MfaRequired(_)) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: "OTP verification returned another MFA challenge".into(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: format!("OTP failed: {e}"),
                    });
                }
            }
        });
    }

    /// Fetch vehicle state in the background
    pub fn poll_vehicle_state(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vehicle_id = tokens.vehicle_id.clone();
        let headers = authenticated_headers(tokens);
        let tx = self.event_tx.clone();
        let debug = self.debug;
        let generation = self.generation;
        self.log(LogLevel::Info, "Fetching vehicle state...");

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx, generation) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: e.to_string(),
                    });
                    return;
                }
            };

            let vars = serde_json::json!({ "vehicleID": vehicle_id });

            let result: Result<VehicleStateData, _> = client
                .graphql(
                    API_URL,
                    "GetVehicleState",
                    queries::GET_VEHICLE_STATE,
                    Some(vars),
                    Some(headers),
                )
                .await;

            match result {
                Ok(data) => match data.vehicle_state {
                    Some(state) => {
                        let _ = tx.send(AppEvent::VehicleState {
                            generation,
                            state: Box::new(state),
                        });
                    }
                    None => {
                        let _ = tx.send(AppEvent::Error {
                            generation,
                            msg: "Poll failed: vehicle state was missing from the response".into(),
                        });
                    }
                },
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: format!("Poll failed: {e}"),
                    });
                }
            }
        });
    }

    /// Fetch charging session history from the charging endpoint
    pub fn fetch_charging_history(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let headers = authenticated_headers(tokens);
        let tx = self.event_tx.clone();
        let debug = self.debug;
        let generation = self.generation;
        self.log(LogLevel::Info, "Fetching charging history...");

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx, generation) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: format!("Charging fetch failed: {e}"),
                    });
                    return;
                }
            };

            let result: Result<ChargingSessionsData, _> = client
                .graphql(
                    CHARGING_URL,
                    "getCompletedSessionSummaries",
                    queries::GET_CHARGING_SESSIONS,
                    None,
                    Some(headers),
                )
                .await;

            match result {
                Ok(data) => {
                    let _ = tx.send(AppEvent::ChargingSessions {
                        generation,
                        sessions: data.get_completed_session_summaries,
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        msg: format!("Charging history: {e}"),
                    });
                }
            }
        });
    }

    pub fn cancel_auth_flow(&mut self) {
        self.generation += 1;
        self.login_busy = false;
        self.login_error = None;
        self.mfa_state = None;
        self.pending_vehicle_selection = None;
        self.login_otp.clear();
        self.vehicle_selection_index = 0;
        self.mode = Mode::Login;
        self.log(LogLevel::Info, "Authentication canceled");
    }

    pub fn vehicle_options(&self) -> &[Vehicle] {
        self.pending_vehicle_selection
            .as_ref()
            .map(|pending| pending.vehicles.as_slice())
            .unwrap_or(&[])
    }

    pub fn select_vehicle_up(&mut self) {
        self.vehicle_selection_index = self.vehicle_selection_index.saturating_sub(1);
    }

    pub fn select_vehicle_down(&mut self) {
        let max = self.vehicle_options().len().saturating_sub(1);
        if self.vehicle_selection_index < max {
            self.vehicle_selection_index += 1;
        }
    }

    pub fn confirm_vehicle_selection(&mut self) {
        let Some(pending) = self.pending_vehicle_selection.clone() else {
            return;
        };
        let Some(vehicle) = pending.vehicles.get(self.vehicle_selection_index).cloned() else {
            return;
        };

        let tokens = pending.into_tokens(vehicle.id.clone());
        match AuthManager::save_tokens(&tokens) {
            Ok(()) => {
                self.tokens = Some(tokens);
                self.pending_vehicle_selection = None;
                self.mode = Mode::Dashboard;
                self.login_error = None;
                self.login_password.clear();
                self.login_otp.clear();
                self.refresh_dashboard_insights();
                self.log(
                    LogLevel::Info,
                    &format!("Selected vehicle {}", vehicle.name.unwrap_or(vehicle.id)),
                );
                self.poll_vehicle_state();
                self.fetch_charging_history();
            }
            Err(e) => {
                self.login_error = Some(format!("Saving vehicle selection failed: {e}"));
                self.log(
                    LogLevel::Error,
                    &format!("Saving vehicle selection failed: {e}"),
                );
            }
        }
    }

    /// Log out: clear tokens and reset state
    pub fn logout(&mut self) {
        self.generation += 1;
        let _ = AuthManager::clear_tokens();
        self.tokens = None;
        self.vehicle_state = None;
        self.recent_trend.clear();
        self.last_charge_session = None;
        self.last_update = None;
        self.mfa_state = None;
        self.pending_vehicle_selection = None;
        self.login_email.clear();
        self.login_password.clear();
        self.login_otp.clear();
        self.login_error = None;
        self.login_busy = false;
        self.vehicle_selection_index = 0;
        self.show_debug_detail = false;
        self.mode = Mode::Login;
        self.sync_shared_data();
        self.log(LogLevel::Info, "Logged out");
    }

    /// Cycle login field focus
    pub fn next_login_field(&mut self) {
        self.login_field = match self.login_field {
            LoginField::Email => LoginField::Password,
            LoginField::Password => LoginField::Email,
            LoginField::Otp => LoginField::Otp,
        };
    }

    /// Get the mutable input string for the currently focused login field
    pub fn active_login_input(&mut self) -> &mut String {
        match self.login_field {
            LoginField::Email => &mut self.login_email,
            LoginField::Password => &mut self.login_password,
            LoginField::Otp => &mut self.login_otp,
        }
    }

    pub fn scroll_log_up(&mut self) {
        if self.activity_log.is_empty() {
            return;
        }

        self.log_selected = self.log_selected.saturating_sub(1);
        if self.log_selected < self.log_scroll {
            self.log_scroll = self.log_selected;
        }
    }

    pub fn scroll_log_down(&mut self) {
        if self.activity_log.is_empty() {
            return;
        }

        let max = self.activity_log.len().saturating_sub(1);
        if self.log_selected < max {
            self.log_selected += 1;
        }

        let visible = 10;
        if self.log_selected >= self.log_scroll + visible {
            self.log_scroll = self.log_selected + 1 - visible;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::auth::AuthTestContext;

    fn sample_tokens() -> AuthTokens {
        AuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            user_session_token: "ust".into(),
            csrf_token: "csrf".into(),
            app_session_token: "ast".into(),
            vehicle_id: "vehicle".into(),
        }
    }

    #[test]
    fn ignores_stale_vehicle_state_events_after_logout() {
        let _auth = AuthTestContext::new();
        let mut app = App::new(false, None);
        app.tokens = Some(sample_tokens());
        let generation = app.generation;

        app.logout();
        app.event_tx
            .send(AppEvent::VehicleState {
                generation,
                state: Box::new(VehicleStateFields::default()),
            })
            .unwrap();

        app.drain_events();
        assert!(app.vehicle_state.is_none());
    }

    #[test]
    fn log_selection_moves_independently_from_scroll() {
        let mut app = App::new(true, None);
        for idx in 0..12 {
            app.log(LogLevel::Info, &format!("log {idx}"));
        }

        let scroll = app.log_scroll;
        app.scroll_log_up();

        assert_eq!(app.log_scroll, scroll);
        assert_eq!(app.log_selected, app.activity_log.len() - 2);
    }
}
