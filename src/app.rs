use chrono::{DateTime, Local, Utc};
use tokio::sync::mpsc;

use crate::api::auth::{AuthManager, LoginOutcome};
use crate::api::client::{API_URL, CHARGING_URL, RequestLog, RivianClient};
use crate::api::queries;
use crate::api::types::*;
use crate::db::Db;

/// UI mode / active screen
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Dashboard,
    Login,
    MfaPrompt,
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
    VehicleState(VehicleStateFields),
    AuthSuccess(AuthTokens),
    MfaRequired(MfaState),
    Error(String),
    Log(LogEntry),
    RequestLog(RequestLog),
    ChargingSessions(Vec<ChargingSession>),
}

pub struct App {
    pub mode: Mode,
    pub should_quit: bool,
    pub debug: bool,

    // Auth
    pub tokens: Option<AuthTokens>,
    pub mfa_state: Option<MfaState>,

    // Login form
    pub login_email: String,
    pub login_password: String,
    pub login_otp: String,
    pub login_field: LoginField,
    pub login_error: Option<String>,
    pub login_busy: bool,

    // Vehicle data
    pub vehicle_state: Option<VehicleStateFields>,
    pub last_update: Option<DateTime<Utc>>,
    pub poll_interval_secs: u64,

    // Activity log
    pub activity_log: Vec<LogEntry>,
    pub log_scroll: usize,
    pub show_debug_detail: bool,
    pub show_log: bool,

    // Database
    pub db: Option<Db>,
    pub db_snapshot_count: i64,

    // Channel for receiving background events
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,
}

impl App {
    pub fn new(debug: bool) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            mode: Mode::Dashboard,
            should_quit: false,
            debug,

            tokens: None,
            mfa_state: None,

            login_email: String::new(),
            login_password: String::new(),
            login_otp: String::new(),
            login_field: LoginField::Email,
            login_error: None,
            login_busy: false,

            vehicle_state: None,
            last_update: None,
            poll_interval_secs: 300,

            activity_log: Vec::new(),
            log_scroll: 0,
            show_debug_detail: false,
            show_log: false,

            db: None,
            db_snapshot_count: 0,

            event_tx,
            event_rx,
        }
    }

    /// Build a RivianClient wired to our event channel
    fn make_client(
        debug: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
    ) -> Result<RivianClient, anyhow::Error> {
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<RequestLog>();
        let app_tx = event_tx.clone();

        // Forward request logs to app events
        tokio::spawn(async move {
            while let Some(req_log) = log_rx.recv().await {
                let _ = app_tx.send(AppEvent::RequestLog(req_log));
            }
        });

        RivianClient::new().map(|c| c.with_debug(debug).with_logger(log_tx))
    }

    pub fn log(&mut self, level: LogLevel, msg: &str) {
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: None,
        });
        // Auto-scroll to bottom
        let visible = 10; // approximate visible log lines
        if self.activity_log.len() > visible {
            self.log_scroll = self.activity_log.len() - visible;
        }
    }

    fn log_with_detail(&mut self, level: LogLevel, msg: &str, detail: String) {
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: Some(detail),
        });
        let visible = 10;
        if self.activity_log.len() > visible {
            self.log_scroll = self.activity_log.len() - visible;
        }
    }

    /// Initialize database and load auth tokens on startup
    pub fn try_load_auth(&mut self) {
        match Db::open() {
            Ok(db) => {
                let count = db.snapshot_count().unwrap_or(0);
                self.db_snapshot_count = count;
                self.db = Some(db);
                self.log(LogLevel::Info, &format!("Database ready ({count} snapshots)"));
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Database failed: {e}"));
            }
        }
        // Log where we expect the file
        if let Some(path) = dirs::config_dir() {
            let token_path = path.join("rivian-tui").join("tokens.json");
            self.log(
                LogLevel::Info,
                &format!(
                    "Checking {} (exists: {})",
                    token_path.display(),
                    token_path.exists()
                ),
            );
        }

        match AuthManager::load_tokens() {
            Ok(Some(tokens)) => {
                let vid = tokens.vehicle_id.clone();
                self.tokens = Some(tokens);
                self.log(
                    LogLevel::Info,
                    &format!("Loaded credentials (vehicle: {vid})"),
                );
            }
            Ok(None) => {
                self.mode = Mode::Login;
                self.log(LogLevel::Info, "No saved credentials — please log in");
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
            match event {
                AppEvent::VehicleState(state) => {
                    // Record to database
                    if let Some(db) = &self.db {
                        let vid = self
                            .tokens
                            .as_ref()
                            .map(|t| t.vehicle_id.as_str())
                            .unwrap_or("unknown");
                        match db.insert_state(vid, &state) {
                            Ok(_) => {
                                self.db_snapshot_count =
                                    db.snapshot_count().unwrap_or(self.db_snapshot_count);
                            }
                            Err(e) => {
                                self.log(LogLevel::Error, &format!("DB write failed: {e}"));
                            }
                        }
                    }
                    self.vehicle_state = Some(state);
                    self.last_update = Some(Utc::now());
                    self.log(
                        LogLevel::Info,
                        &format!(
                            "Vehicle state updated ({}  snapshots recorded)",
                            self.db_snapshot_count
                        ),
                    );
                }
                AppEvent::AuthSuccess(tokens) => {
                    self.tokens = Some(tokens);
                    self.mfa_state = None;
                    self.mode = Mode::Dashboard;
                    self.login_busy = false;
                    self.login_error = None;
                    self.login_password.clear();
                    self.login_otp.clear();
                    self.log(LogLevel::Info, "Login successful — fetching vehicle state...");
                }
                AppEvent::MfaRequired(mfa) => {
                    self.mfa_state = Some(mfa);
                    self.mode = Mode::MfaPrompt;
                    self.login_busy = false;
                    self.log(
                        LogLevel::Info,
                        "MFA required — enter the OTP code sent to your device",
                    );
                }
                AppEvent::Error(msg) => {
                    self.login_busy = false;
                    self.login_error = Some(msg.clone());
                    self.log(LogLevel::Error, &msg);
                }
                AppEvent::Log(entry) => {
                    self.activity_log.push(entry);
                    let visible = 10;
                    if self.activity_log.len() > visible {
                        self.log_scroll = self.activity_log.len() - visible;
                    }
                }
                AppEvent::RequestLog(req_log) => {
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
                                    &serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.clone()),
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
                                    &serde_json::to_string_pretty(&v).unwrap_or_else(|_| resp.clone()),
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
                AppEvent::ChargingSessions(sessions) => {
                    if let Some(db) = &self.db {
                        match db.upsert_charging_sessions(&sessions) {
                            Ok(new) => {
                                let total = db.charging_session_count().unwrap_or(0);
                                self.log(
                                    LogLevel::Info,
                                    &format!(
                                        "Charging history: {new} new sessions ({total} total)"
                                    ),
                                );
                            }
                            Err(e) => {
                                self.log(
                                    LogLevel::Error,
                                    &format!("DB charging write failed: {e}"),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Kick off login in a background task
    pub fn start_login(&mut self) {
        if self.login_busy {
            return;
        }
        self.login_busy = true;
        self.login_error = None;
        self.log(LogLevel::Info, "Logging in...");

        let email = self.login_email.clone();
        let password = self.login_password.clone();
        let tx = self.event_tx.clone();
        let debug = self.debug;

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(e.to_string()));
                    return;
                }
            };
            let auth_mgr = AuthManager::new(client);

            let _ = tx.send(AppEvent::Log(LogEntry {
                timestamp: Local::now(),
                level: LogLevel::Info,
                message: "Fetching CSRF token...".into(),
                detail: None,
            }));

            match auth_mgr.login(&email, &password).await {
                Ok(LoginOutcome::Success(tokens)) => {
                    let _ = tx.send(AppEvent::AuthSuccess(tokens));
                }
                Ok(LoginOutcome::MfaRequired(mfa)) => {
                    let _ = tx.send(AppEvent::MfaRequired(mfa));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("Login failed: {e}")));
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

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(e.to_string()));
                    return;
                }
            };
            let auth_mgr = AuthManager::new(client);

            match auth_mgr.complete_mfa(&mfa, &otp).await {
                Ok(tokens) => {
                    let _ = tx.send(AppEvent::AuthSuccess(tokens));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("OTP failed: {e}")));
                }
            }
        });
    }

    /// Build the auth headers needed for authenticated API requests
    fn auth_headers(tokens: &AuthTokens) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("Bearer {}", tokens.access_token)),
            ("Csrf-Token", tokens.csrf_token.clone()),
            ("A-Sess", tokens.app_session_token.clone()),
            ("U-Sess", tokens.user_session_token.clone()),
            (
                "Dc-Cid",
                format!("m-ios-{}", uuid::Uuid::new_v4()),
            ),
        ]
    }

    /// Fetch vehicle state in the background
    pub fn poll_vehicle_state(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vehicle_id = tokens.vehicle_id.clone();
        let headers = Self::auth_headers(tokens);
        let tx = self.event_tx.clone();
        let debug = self.debug;
        self.log(LogLevel::Info, "Fetching vehicle state...");

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(e.to_string()));
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
                Ok(data) => {
                    let _ = tx.send(AppEvent::VehicleState(data.vehicle_state));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("Poll failed: {e}")));
                }
            }
        });
    }

    /// Fetch charging session history from the charging endpoint
    pub fn fetch_charging_history(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let headers = Self::auth_headers(tokens);
        let tx = self.event_tx.clone();
        let debug = self.debug;
        self.log(LogLevel::Info, "Fetching charging history...");

        tokio::spawn(async move {
            let client = match Self::make_client(debug, &tx) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("Charging fetch failed: {e}")));
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
                    let _ = tx.send(AppEvent::ChargingSessions(
                        data.get_completed_session_summaries,
                    ));
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error(format!("Charging history: {e}")));
                }
            }
        });
    }

    /// Log out: clear tokens and reset state
    pub fn logout(&mut self) {
        let _ = AuthManager::clear_tokens();
        self.tokens = None;
        self.vehicle_state = None;
        self.last_update = None;
        self.mfa_state = None;
        self.login_email.clear();
        self.login_password.clear();
        self.login_otp.clear();
        self.login_error = None;
        self.mode = Mode::Login;
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
        self.log_scroll = self.log_scroll.saturating_sub(1);
    }

    pub fn scroll_log_down(&mut self) {
        let max = self.activity_log.len().saturating_sub(1);
        if self.log_scroll < max {
            self.log_scroll += 1;
        }
    }
}
