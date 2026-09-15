use std::sync::{Arc, RwLock};
use std::time::Instant;

use chrono::{DateTime, Local, Utc};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::api::auth::{authenticated_headers, AuthManager, LoginOutcome, PendingVehicleSelection};
use crate::api::client::{RequestLog, RivianClient, SessionExpired, API_URL, CHARGING_URL};
use crate::api::queries;
use crate::api::types::*;
use crate::db::{ChargeSessionSummary, ChargingStats, Db, Trip, VehicleTrendPoint};
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
    pub vehicle_metadata: Option<VehicleMetadata>,
    pub recent_trend: Vec<VehicleTrendPoint>,
    pub recent_trips: Vec<Trip>,
    pub last_charge_session: Option<ChargeSessionSummary>,
    pub live_charging_session: Option<LiveChargingSession>,
    pub live_charging_history: Option<LiveSessionHistory>,
    pub ota_update_details: Option<OtaUpdateDetails>,
    pub charging_stats: Option<ChargingStats>,
    pub last_update: Option<DateTime<Utc>>,
    pub vehicle_id: Option<String>,
}

/// Readers (the web server) clone the inner `Arc` under a brief read lock
/// and then work on an immutable snapshot, so a page render never holds the
/// lock and never deep-copies the trend/chart series. The writer swaps in a
/// fresh `Arc` per update.
pub type SharedDashboardData = Arc<RwLock<Arc<DashboardData>>>;

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

/// Which background operation an `AppEvent::Error` came from. The handler
/// routes on this (login-screen errors vs. dashboard poll errors vs. purely
/// informational fetch failures) instead of sniffing message prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSource {
    Login,
    Otp,
    Poll,
    LiveSession,
    LiveHistory,
    OtaDetails,
    Metadata,
    ChargingHistory,
}

impl ErrorSource {
    /// Human prefix for the activity log / error panels.
    pub fn label(self) -> &'static str {
        match self {
            Self::Login => "Login failed",
            Self::Otp => "OTP failed",
            Self::Poll => "Poll failed",
            Self::LiveSession => "Live session",
            Self::LiveHistory => "Live charge history",
            Self::OtaDetails => "OTA details",
            Self::Metadata => "Vehicle metadata",
            Self::ChargingHistory => "Charging history",
        }
    }
}

/// Events sent from background tasks to the main loop
pub enum AppEvent {
    /// Log message produced by a long-lived service (e.g. MQTT). Bypasses
    /// the generation filter because it isn't tied to a specific
    /// auth/session lifecycle.
    ServiceLog(LogEntry),
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
        source: ErrorSource,
        msg: String,
    },
    /// The API rejected the saved session; the user must sign in again.
    SessionExpired {
        generation: u64,
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
    LiveChargingSession {
        generation: u64,
        session: Option<Box<LiveChargingSession>>,
    },
    LiveChargingHistory {
        generation: u64,
        history: Option<LiveSessionHistory>,
    },
    OtaDetails {
        generation: u64,
        details: Option<OtaUpdateDetails>,
    },
    VehicleMetadata {
        generation: u64,
        metadata: Option<VehicleMetadata>,
    },
}

impl AppEvent {
    /// Generation associated with the event, if any. Service-level events
    /// (e.g. MQTT) have no generation and are processed unconditionally.
    fn maybe_generation(&self) -> Option<u64> {
        match self {
            Self::ServiceLog(_) => None,
            Self::VehicleState { generation, .. }
            | Self::AuthSuccess { generation, .. }
            | Self::MfaRequired { generation, .. }
            | Self::VehicleSelectionRequired { generation, .. }
            | Self::Error { generation, .. }
            | Self::SessionExpired { generation }
            | Self::Log { generation, .. }
            | Self::RequestLog { generation, .. }
            | Self::ChargingSessions { generation, .. }
            | Self::LiveChargingSession { generation, .. }
            | Self::LiveChargingHistory { generation, .. }
            | Self::OtaDetails { generation, .. }
            | Self::VehicleMetadata { generation, .. } => Some(*generation),
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
    pub vehicle_metadata: Option<VehicleMetadata>,
    pub recent_trend: Vec<VehicleTrendPoint>,
    pub recent_trips: Vec<Trip>,
    pub last_charge_session: Option<ChargeSessionSummary>,
    pub live_charging_session: Option<LiveChargingSession>,
    pub live_charging_history: Option<LiveSessionHistory>,
    pub ota_update_details: Option<OtaUpdateDetails>,
    pub charging_stats: Option<ChargingStats>,
    pub last_update: Option<DateTime<Utc>>,
    pub vehicle_state_error: Option<String>,
    pub poll_interval_secs: u64,
    /// A vehicle-state request is outstanding. Prevents `r` mashing or an
    /// overlapping interval tick from stacking identical polls.
    pub poll_in_flight: bool,

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

    /// Shared reqwest client (and its connection pool) reused across every
    /// spawned request. Cloning it is cheap — internally Arc — so each
    /// per-task `RivianClient` reuses the same pool instead of building a
    /// fresh one.
    http_client: reqwest::Client,

    /// Endpoint URLs. Fields (not consts) so tests can point at an
    /// unroutable local address.
    pub api_url: String,
    pub charging_url: String,

    /// When the last `fetch_live_session` fired. Lets the main loop poll
    /// the charging endpoint on a faster cadence than the full vehicle-state
    /// query while a session is active.
    pub last_live_fetch: Option<Instant>,

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
            vehicle_metadata: None,
            recent_trend: Vec::new(),
            recent_trips: Vec::new(),
            last_charge_session: None,
            live_charging_session: None,
            live_charging_history: None,
            ota_update_details: None,
            charging_stats: None,
            last_update: None,
            vehicle_state_error: None,
            poll_interval_secs: 300,
            poll_in_flight: false,

            activity_log: Vec::new(),
            log_scroll: 0,
            log_selected: 0,
            show_debug_detail: false,
            show_log: false,

            db: None,
            mqtt,
            db_snapshot_count: 0,
            generation: 0,

            http_client: RivianClient::build_http()
                .expect("reqwest client builder uses static config; should not fail"),

            api_url: API_URL.to_string(),
            charging_url: CHARGING_URL.to_string(),

            last_live_fetch: None,

            event_tx,
            event_rx,
            shared_data: Arc::new(RwLock::new(Arc::new(DashboardData::default()))),
        }
    }

    /// Copy the current dashboard-relevant fields into the shared snapshot so
    /// other readers (web server, etc.) can observe the latest state.
    fn sync_shared_data(&self) {
        let snapshot = DashboardData {
            vehicle_state: self.vehicle_state.clone(),
            vehicle_metadata: self.vehicle_metadata.clone(),
            recent_trend: self.recent_trend.clone(),
            recent_trips: self.recent_trips.clone(),
            last_charge_session: self.last_charge_session.clone(),
            live_charging_session: self.live_charging_session.clone(),
            live_charging_history: self.live_charging_history.clone(),
            ota_update_details: self.ota_update_details.clone(),
            charging_stats: self.charging_stats.clone(),
            last_update: self.last_update,
            vehicle_id: self.tokens.as_ref().map(|t| t.vehicle_id.clone()),
        };
        if let Ok(mut guard) = self.shared_data.write() {
            *guard = Arc::new(snapshot);
        }
    }

    /// Handle to the shared dashboard snapshot. Clone this to pass to
    /// background tasks (e.g. the web server).
    pub fn shared_data_handle(&self) -> SharedDashboardData {
        Arc::clone(&self.shared_data)
    }

    /// Build a RivianClient wired to our event channel. Reuses the shared
    /// reqwest connection pool so we don't spin up a fresh one per task.
    fn make_client(
        http: reqwest::Client,
        debug: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        generation: u64,
    ) -> RivianClient {
        let (log_tx, mut log_rx) = mpsc::unbounded_channel::<RequestLog>();
        let app_tx = event_tx.clone();

        // Forward request logs to app events. The forwarder captures the
        // generation at spawn time so a logout/login between the request
        // firing and the response arriving doesn't apply stale data.
        tokio::spawn(async move {
            while let Some(req_log) = log_rx.recv().await {
                let _ = app_tx.send(AppEvent::RequestLog {
                    generation,
                    req_log,
                });
            }
        });

        RivianClient::from_http(http)
            .with_debug(debug)
            .with_logger(log_tx)
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

    /// True when the user is currently focused on the latest log entry — i.e.
    /// the log is "tailing". When that's the case, new entries should keep the
    /// view pinned to the bottom; when it's not, a fresh entry should not
    /// yank focus away from whatever the user is reading.
    fn log_is_tailing(&self) -> bool {
        match self.activity_log.len() {
            0 => true,
            n => self.log_selected + 1 >= n,
        }
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
        let was_tailing = self.log_is_tailing();
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: None,
        });
        self.trim_activity_log();
        if was_tailing {
            self.focus_last_log();
        }
    }

    fn log_with_detail(&mut self, level: LogLevel, msg: &str, detail: String) {
        let was_tailing = self.log_is_tailing();
        self.activity_log.push(LogEntry {
            timestamp: Local::now(),
            level,
            message: msg.to_string(),
            detail: Some(detail),
        });
        self.trim_activity_log();
        if was_tailing {
            self.focus_last_log();
        }
    }

    fn refresh_dashboard_insights(&mut self) {
        let Some(vehicle_id) = self.tokens.as_ref().map(|tokens| tokens.vehicle_id.clone()) else {
            self.recent_trend.clear();
            self.recent_trips.clear();
            self.last_charge_session = None;
            self.charging_stats = None;
            self.sync_shared_data();
            return;
        };
        let Some(db) = &self.db else {
            self.recent_trend.clear();
            self.recent_trips.clear();
            self.last_charge_session = None;
            self.charging_stats = None;
            self.sync_shared_data();
            return;
        };

        let trend_result = db.recent_vehicle_trend(&vehicle_id, 24); // last 24 hours
        let trips_result = db.recent_trips(&vehicle_id, 5); // last 5 driving trips
        let charge_result = db.latest_charging_session(&vehicle_id);
        let stats_result = db.charging_session_stats(&vehicle_id);

        match trend_result {
            Ok(points) => {
                self.recent_trend = points;
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Trend load failed: {e}"));
            }
        }

        match trips_result {
            Ok(trips) => {
                self.recent_trips = trips;
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Trip load failed: {e}"));
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

        match stats_result {
            Ok(stats) => {
                self.charging_stats = stats;
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Charging stats load failed: {e}"));
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
            if let Some(gen) = event.maybe_generation() {
                if gen != self.generation {
                    continue;
                }
            }

            match event {
                AppEvent::ServiceLog(entry) => {
                    let was_tailing = self.log_is_tailing();
                    self.activity_log.push(entry);
                    self.trim_activity_log();
                    if was_tailing {
                        self.focus_last_log();
                    }
                }
                AppEvent::VehicleState { state, .. } => {
                    self.poll_in_flight = false;
                    self.vehicle_state_error = None;
                    let refresh_ota = self.ota_details_need_refresh(&state);
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

                    let charging_now = state.is_actively_charging();
                    self.vehicle_state = Some(*state);
                    self.last_update = Some(Utc::now());
                    self.refresh_dashboard_insights();
                    if refresh_ota {
                        self.fetch_ota_details();
                    }
                    self.log(
                        LogLevel::Info,
                        &format!(
                            "Vehicle state updated ({} snapshots recorded)",
                            self.db_snapshot_count
                        ),
                    );

                    if charging_now {
                        // Fire a live-session fetch alongside the regular poll
                        // so the dashboard sees current power / kWh delivered.
                        self.fetch_live_session();
                    } else if self.live_charging_session.take().is_some() {
                        // Session ended — drop the stale live snapshot and
                        // refresh the historical view so a brand-new
                        // completed session shows up immediately.
                        self.live_charging_history = None;
                        self.fetch_charging_history();
                        self.sync_shared_data();
                    }
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
                    self.log(
                        LogLevel::Info,
                        "Login successful — fetching vehicle state...",
                    );
                    self.start_session();
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
                AppEvent::Error { source, msg, .. } => {
                    match source {
                        ErrorSource::Login | ErrorSource::Otp => {
                            self.login_busy = false;
                            self.login_error = Some(msg.clone());
                        }
                        ErrorSource::Poll => {
                            self.poll_in_flight = false;
                            self.vehicle_state_error = Some(msg.clone());
                        }
                        ErrorSource::LiveSession
                        | ErrorSource::LiveHistory
                        | ErrorSource::OtaDetails
                        | ErrorSource::Metadata
                        | ErrorSource::ChargingHistory => {}
                    }
                    self.log(LogLevel::Error, &msg);
                }
                AppEvent::SessionExpired { .. } => {
                    const MSG: &str = "Session expired — please sign in again";
                    let _ = AuthManager::clear_tokens();
                    self.reset_session();
                    self.login_error = Some(MSG.into());
                    self.log(LogLevel::Error, MSG);
                }
                AppEvent::Log { entry, .. } => {
                    let was_tailing = self.log_is_tailing();
                    self.activity_log.push(entry);
                    self.trim_activity_log();
                    if was_tailing {
                        self.focus_last_log();
                    }
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
                    } else if !req_log.warnings.is_empty() {
                        self.log(
                            LogLevel::Error,
                            &format!(
                                "{} partial response: {}",
                                req_log.operation,
                                req_log.warnings.join("; ")
                            ),
                        );
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
                AppEvent::LiveChargingSession { session, .. } => {
                    let live = session.map(|boxed| *boxed);
                    if live.is_some()
                        && self
                            .vehicle_state
                            .as_ref()
                            .is_some_and(|state| !state.is_actively_charging())
                    {
                        // A live-session request can complete after a newer
                        // vehicle-state poll has already observed charging
                        // stopped. Do not let that stale response resurrect
                        // the in-progress card.
                        self.live_charging_session = None;
                        self.sync_shared_data();
                        continue;
                    }
                    let vehicle_id = self
                        .tokens
                        .as_ref()
                        .map(|t| t.vehicle_id.clone())
                        .unwrap_or_else(|| "unknown".into());

                    if let (Some(db), Some(snap)) = (&self.db, live.as_ref()) {
                        if let Err(e) = db.insert_live_charging_snapshot(&vehicle_id, snap) {
                            self.log(
                                LogLevel::Error,
                                &format!("DB live-session write failed: {e}"),
                            );
                        }
                    }

                    if let Some(snap) = &live {
                        let power = snap
                            .power_kw()
                            .map(|kw| format!("{kw:.1} kW"))
                            .unwrap_or_else(|| "?".into());
                        let soc = snap
                            .soc_percent()
                            .map(|s| format!("{s:.0}%"))
                            .unwrap_or_else(|| "?".into());
                        self.log(LogLevel::Info, &format!("Live charging: {power} @ {soc}"));
                    }

                    let has_live = live.is_some();
                    self.live_charging_session = live;
                    if !has_live {
                        self.live_charging_history = None;
                    }
                    self.sync_shared_data();
                    if has_live {
                        self.fetch_live_session_history();
                    }
                }
                AppEvent::LiveChargingHistory { history, .. } => {
                    self.live_charging_history = history;
                    self.sync_shared_data();
                }
                AppEvent::OtaDetails { details, .. } => {
                    self.ota_update_details = details;
                    self.sync_shared_data();
                }
                AppEvent::VehicleMetadata { metadata, .. } => {
                    self.vehicle_metadata = metadata;
                    self.sync_shared_data();
                }
                AppEvent::ChargingSessions { sessions, .. } => {
                    let default_vehicle_id = self
                        .tokens
                        .as_ref()
                        .map(|t| t.vehicle_id.clone())
                        .unwrap_or_default();
                    let new_sessions = if let Some(db) = &self.db {
                        match db.upsert_charging_sessions(&sessions, &default_vehicle_id) {
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
        let http = self.http_client.clone();
        let debug = self.debug;
        let generation = self.generation;

        tokio::spawn(async move {
            let client = Self::make_client(http, debug, &tx, generation);
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
                        source: ErrorSource::Login,
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
        let http = self.http_client.clone();
        let debug = self.debug;
        let generation = self.generation;

        tokio::spawn(async move {
            let client = Self::make_client(http, debug, &tx, generation);
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
                        source: ErrorSource::Otp,
                        msg: "OTP verification returned another MFA challenge".into(),
                    });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        generation,
                        source: ErrorSource::Otp,
                        msg: format!("OTP failed: {e}"),
                    });
                }
            }
        });
    }

    /// Spawn one authenticated GraphQL query. Owns the lifecycle every
    /// fetch shares: token guard, client construction, generation tagging,
    /// and the error path (a rejected session becomes `SessionExpired`;
    /// anything else becomes `Error { source }` with the source's label as
    /// the message prefix). `on_ok` turns the decoded payload into the event
    /// to deliver. Returns false when there are no tokens to send.
    fn spawn_query<T, F>(
        &self,
        url: String,
        op_name: &'static str,
        query: &'static str,
        vars: Option<serde_json::Value>,
        source: ErrorSource,
        on_ok: F,
    ) -> bool
    where
        T: DeserializeOwned + Send + 'static,
        F: FnOnce(u64, T) -> AppEvent + Send + 'static,
    {
        let Some(tokens) = &self.tokens else {
            return false;
        };
        let headers = authenticated_headers(tokens);
        let tx = self.event_tx.clone();
        let http = self.http_client.clone();
        let debug = self.debug;
        let generation = self.generation;

        tokio::spawn(async move {
            let client = Self::make_client(http, debug, &tx, generation);
            let result: anyhow::Result<T> = client
                .graphql(&url, op_name, query, vars, Some(headers))
                .await;
            let event = match result {
                Ok(data) => on_ok(generation, data),
                Err(e) if e.downcast_ref::<SessionExpired>().is_some() => {
                    AppEvent::SessionExpired { generation }
                }
                Err(e) => AppEvent::Error {
                    generation,
                    source,
                    msg: format!("{}: {e}", source.label()),
                },
            };
            let _ = tx.send(event);
        });
        true
    }

    /// Everything a freshly authenticated session needs: the one-shot
    /// metadata and OTA-detail lookups plus the first vehicle-state poll and
    /// charging history. Single entry point so startup, login, and vehicle
    /// selection can't drift on the bundle.
    pub fn start_session(&mut self) {
        self.refresh_dashboard_insights();
        self.fetch_vehicle_metadata();
        self.fetch_ota_details();
        self.poll_vehicle_state();
        self.fetch_charging_history();
    }

    /// Fetch vehicle state in the background. Returns false if a poll is
    /// already outstanding or there is no session.
    pub fn poll_vehicle_state(&mut self) -> bool {
        if self.poll_in_flight {
            return false;
        }
        let Some(tokens) = &self.tokens else {
            return false;
        };
        let vars = serde_json::json!({ "vehicleID": tokens.vehicle_id });
        let started = self.spawn_query::<VehicleStateData, _>(
            self.api_url.clone(),
            "GetVehicleState",
            queries::GET_VEHICLE_STATE,
            Some(vars),
            ErrorSource::Poll,
            |generation, data| match data.vehicle_state {
                Some(state) => AppEvent::VehicleState {
                    generation,
                    state: Box::new(state),
                },
                None => AppEvent::Error {
                    generation,
                    source: ErrorSource::Poll,
                    msg: "Poll failed: vehicle state was missing from the response".into(),
                },
            },
        );
        if started {
            self.poll_in_flight = true;
            self.vehicle_state_error = None;
            self.log(LogLevel::Info, "Fetching vehicle state...");
        }
        started
    }

    /// Release-note URLs only change when the installed or offered OTA
    /// version changes, so refetch on a poll only when those fields moved
    /// (or a previous fetch never produced details). With no prior state the
    /// session bundle has just requested them.
    pub fn ota_details_need_refresh(&self, new_state: &VehicleStateFields) -> bool {
        let Some(prev) = &self.vehicle_state else {
            return false;
        };
        if self.ota_update_details.is_none() {
            return true;
        }
        prev.get_str(&prev.ota_current_version) != new_state.get_str(&new_state.ota_current_version)
            || prev.get_str(&prev.ota_available_version)
                != new_state.get_str(&new_state.ota_available_version)
    }

    /// Fetch the current live charging session from the charging endpoint.
    /// Should only be called when the vehicle is actively charging — outside
    /// of an active session the API returns null and we treat that as "no
    /// live session" rather than an error.
    pub fn fetch_live_session(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vars = serde_json::json!({ "vehicleId": tokens.vehicle_id });
        self.last_live_fetch = Some(Instant::now());
        self.spawn_query::<LiveSessionData, _>(
            self.charging_url.clone(),
            "getLiveSessionData",
            queries::GET_LIVE_CHARGING_SESSION,
            Some(vars),
            ErrorSource::LiveSession,
            |generation, data| AppEvent::LiveChargingSession {
                generation,
                session: data.get_live_session_data.map(Box::new),
            },
        );
    }

    /// Fetch the current live charging session's power history. This is a
    /// charging-endpoint chart series (`kw`, `time`) and is only useful while
    /// a live session exists.
    pub fn fetch_live_session_history(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vars = serde_json::json!({ "vehicleId": tokens.vehicle_id });
        self.spawn_query::<LiveSessionHistoryData, _>(
            self.charging_url.clone(),
            "getLiveSessionHistory",
            queries::GET_LIVE_CHARGING_HISTORY,
            Some(vars),
            ErrorSource::LiveHistory,
            |generation, data| AppEvent::LiveChargingHistory {
                generation,
                history: data.get_live_session_history,
            },
        );
    }

    /// Fetch release-note/detail URLs for the current and available OTA
    /// versions.
    pub fn fetch_ota_details(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vars = serde_json::json!({ "vehicleId": tokens.vehicle_id });
        self.spawn_query::<OtaDetailsData, _>(
            self.api_url.clone(),
            "getOTAUpdateDetails",
            queries::GET_OTA_UPDATE_DETAILS,
            Some(vars),
            ErrorSource::OtaDetails,
            |generation, data| AppEvent::OtaDetails {
                generation,
                details: data.get_vehicle.map(OtaUpdateDetails::from),
            },
        );
    }

    /// Fetch richer selected-vehicle metadata for labels and web JSON. This
    /// intentionally does not surface personal account fields.
    pub fn fetch_vehicle_metadata(&mut self) {
        let Some(tokens) = &self.tokens else {
            return;
        };
        let vehicle_id = tokens.vehicle_id.clone();
        self.spawn_query::<UserInfoData, _>(
            self.api_url.clone(),
            "getUserInfo",
            queries::GET_USER_INFO,
            None,
            ErrorSource::Metadata,
            move |generation, data| AppEvent::VehicleMetadata {
                generation,
                metadata: data
                    .current_user
                    .vehicles
                    .iter()
                    .find(|vehicle| vehicle.id == vehicle_id)
                    .map(Vehicle::metadata),
            },
        );
    }

    /// Fetch charging session history from the charging endpoint
    pub fn fetch_charging_history(&mut self) {
        if self.tokens.is_none() {
            return;
        }
        self.log(LogLevel::Info, "Fetching charging history...");
        self.spawn_query::<ChargingSessionsData, _>(
            self.charging_url.clone(),
            "getCompletedSessionSummaries",
            queries::GET_CHARGING_SESSIONS,
            None,
            ErrorSource::ChargingHistory,
            |generation, data| AppEvent::ChargingSessions {
                generation,
                sessions: data.get_completed_session_summaries,
            },
        );
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
                self.log(
                    LogLevel::Info,
                    &format!("Selected vehicle {}", vehicle.display_name()),
                );
                self.start_session();
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
        let _ = AuthManager::clear_tokens();
        self.reset_session();
        self.log(LogLevel::Info, "Logged out");
    }

    /// Drop every piece of session state and return to the login screen.
    /// Bumps the generation so any in-flight response is discarded.
    fn reset_session(&mut self) {
        self.generation += 1;
        self.tokens = None;
        self.poll_in_flight = false;
        self.vehicle_state = None;
        self.vehicle_metadata = None;
        self.recent_trend.clear();
        self.recent_trips.clear();
        self.last_charge_session = None;
        self.live_charging_session = None;
        self.live_charging_history = None;
        self.ota_update_details = None;
        self.charging_stats = None;
        self.last_update = None;
        self.vehicle_state_error = None;
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
            device_id: Some("device".into()),
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

    #[test]
    fn new_log_does_not_yank_focus_when_user_scrolled_up() {
        let mut app = App::new(true, None);
        for idx in 0..12 {
            app.log(LogLevel::Info, &format!("log {idx}"));
        }

        // Scroll back through history.
        for _ in 0..5 {
            app.scroll_log_up();
        }
        let parked_selected = app.log_selected;
        let parked_scroll = app.log_scroll;

        // A new log entry arrives while user is reading older history. The
        // user's selection and scroll position should be preserved — no
        // unexpected jump to the bottom.
        app.log(LogLevel::Info, "new entry while reading history");

        assert_eq!(app.log_selected, parked_selected);
        assert_eq!(app.log_scroll, parked_scroll);
    }

    #[test]
    fn new_log_keeps_tailing_when_user_at_bottom() {
        let mut app = App::new(true, None);
        for idx in 0..12 {
            app.log(LogLevel::Info, &format!("log {idx}"));
        }
        // Default: focus is on the latest entry, so a new one should still
        // pin the view to the bottom.
        app.log(LogLevel::Info, "tail entry");
        assert_eq!(app.log_selected, app.activity_log.len() - 1);
    }

    #[test]
    fn stale_live_session_event_does_not_resurrect_finished_charge() {
        let mut app = App::new(true, None);
        app.vehicle_state = Some(VehicleStateFields {
            charger_state: Some(StateValue {
                value: serde_json::json!("charging_inactive"),
            }),
            ..Default::default()
        });

        app.event_tx
            .send(AppEvent::LiveChargingSession {
                generation: app.generation,
                session: Some(Box::new(LiveChargingSession {
                    power: Some(TsValue {
                        value: serde_json::json!(32.0),
                        updated_at: None,
                    }),
                    ..Default::default()
                })),
            })
            .unwrap();

        app.drain_events();

        assert!(app.live_charging_session.is_none());
        let shared = app.shared_data.read().unwrap();
        assert!(shared.live_charging_session.is_none());
    }

    #[test]
    fn vehicle_state_error_clears_after_successful_poll() {
        let mut app = App::new(false, None);
        app.event_tx
            .send(AppEvent::Error {
                generation: app.generation,
                source: ErrorSource::Poll,
                msg: "Poll failed: test failure".into(),
            })
            .unwrap();
        app.drain_events();
        assert_eq!(
            app.vehicle_state_error.as_deref(),
            Some("Poll failed: test failure")
        );

        app.event_tx
            .send(AppEvent::VehicleState {
                generation: app.generation,
                state: Box::default(),
            })
            .unwrap();
        app.drain_events();
        assert!(app.vehicle_state_error.is_none());
    }

    fn charging_state(charger_state: &str) -> VehicleStateFields {
        VehicleStateFields {
            charger_state: Some(StateValue {
                value: serde_json::json!(charger_state),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn background_fetch_errors_do_not_touch_login_screen_state() {
        let mut app = App::new(false, None);
        app.login_busy = true;
        app.event_tx
            .send(AppEvent::Error {
                generation: app.generation,
                source: ErrorSource::OtaDetails,
                msg: "OTA details: boom".into(),
            })
            .unwrap();
        app.drain_events();
        assert!(
            app.login_error.is_none(),
            "non-auth errors must not paint the login screen"
        );
        assert!(app.login_busy, "non-auth errors must not clear login_busy");
        assert!(app.vehicle_state_error.is_none());
        assert!(app.activity_log.iter().any(|e| e.message.contains("boom")));
    }

    #[test]
    fn login_errors_still_reach_login_screen() {
        let mut app = App::new(false, None);
        app.login_busy = true;
        app.event_tx
            .send(AppEvent::Error {
                generation: app.generation,
                source: ErrorSource::Login,
                msg: "Login failed: bad password".into(),
            })
            .unwrap();
        app.drain_events();
        assert_eq!(
            app.login_error.as_deref(),
            Some("Login failed: bad password")
        );
        assert!(!app.login_busy);
    }

    #[test]
    fn session_expiry_drops_to_login_and_clears_tokens() {
        let _auth = AuthTestContext::new();
        let mut app = App::new(false, None);
        app.tokens = Some(sample_tokens());
        app.vehicle_state = Some(VehicleStateFields::default());
        let generation = app.generation;

        app.event_tx
            .send(AppEvent::SessionExpired { generation })
            .unwrap();
        app.drain_events();

        assert_eq!(app.mode, Mode::Login);
        assert!(app.tokens.is_none());
        assert!(app.vehicle_state.is_none());
        assert!(app.login_error.as_deref().unwrap_or("").contains("expired"));
        assert_ne!(
            app.generation, generation,
            "in-flight responses must be invalidated"
        );
    }

    #[tokio::test]
    async fn second_poll_is_skipped_while_first_is_in_flight() {
        let mut app = App::new(false, None);
        app.tokens = Some(sample_tokens());
        // Unroutable local address: the request fails fast without leaving
        // the machine.
        app.api_url = "http://127.0.0.1:9/graphql".into();

        assert!(app.poll_vehicle_state(), "first poll should start");
        assert!(
            !app.poll_vehicle_state(),
            "second poll must be skipped while in flight"
        );

        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            app.drain_events();
            if app.vehicle_state_error.is_some() {
                break;
            }
        }
        assert!(app.vehicle_state_error.is_some(), "poll should have failed");
        assert!(
            app.poll_vehicle_state(),
            "poll must be allowed again after the previous one finished"
        );
    }

    #[test]
    fn ota_details_refresh_only_when_versions_change() {
        let mut app = App::new(false, None);
        let mut v1 = charging_state("charging_inactive");
        v1.ota_current_version = Some(StateValue {
            value: serde_json::json!("2026.10.0"),
        });
        v1.ota_available_version = Some(StateValue {
            value: serde_json::json!("0.0.0"),
        });

        // No previous state: start_session already fetched details.
        assert!(!app.ota_details_need_refresh(&v1));

        app.vehicle_state = Some(v1.clone());
        // Details never arrived (earlier fetch failed): retry.
        assert!(app.ota_details_need_refresh(&v1));

        app.ota_update_details = Some(OtaUpdateDetails::default());
        assert!(
            !app.ota_details_need_refresh(&v1),
            "unchanged versions must not refetch"
        );

        let mut v2 = v1.clone();
        v2.ota_available_version = Some(StateValue {
            value: serde_json::json!("2026.12.0"),
        });
        assert!(
            app.ota_details_need_refresh(&v2),
            "new available version must refetch"
        );
    }
}
