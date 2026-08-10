//! Presentation layer shared between the web dashboard and (eventually) any
//! other non-TUI renderer. The TUI still draws directly from the raw
//! `VehicleStateFields` helpers; this module exists so the web server can
//! render pre-formatted strings and serialize them as JSON without pulling in
//! ratatui-specific code.

use chrono::{DateTime, Local, Utc};
use serde::Serialize;

use crate::api::types::{
    LiveChargingSession, LiveSessionHistory, OtaUpdateDetails, StateValue, VehicleMetadata,
    VehicleStateFields, KM_PER_MI, METERS_PER_MI,
};
use crate::app::DashboardData;
use crate::db::{ChargeSessionSummary, ChargingStats, Trip, VehicleTrendPoint};

/// A flat, pre-formatted view of the dashboard intended for HTML/JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardView {
    pub vehicle_id: Option<String>,
    pub vehicle_label: String,
    pub vehicle_metadata: Option<VehicleMetadataView>,
    pub last_update_human: String,
    pub last_update_iso: Option<String>,
    pub has_data: bool,
    pub battery: BatteryView,
    pub charging: ChargingView,
    pub climate: ClimateView,
    pub vehicle: VehicleView,
    pub software: SoftwareView,
    pub location: LocationView,
    pub trend: Vec<TrendPointView>,
    pub trips: Vec<TripView>,
    pub last_charge: Option<ChargeInsightView>,
    pub live_charge: Option<LiveChargeView>,
    pub live_charge_history: Option<LiveChargeHistoryView>,
    pub ota_details: Option<OtaDetailsView>,
    pub charging_stats: Option<ChargingStatsView>,
    pub alerts: AlertsView,
}

/// What an alert is about — lets renderers special-case categories (e.g. the
/// web dashboard renders its own richer OTA banner and skips the generic one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Closure,
    Window,
    Tire,
    Charging,
    Battery,
    Brake,
    Hardware,
    Fluid,
    ColdLimits,
    Ota,
    TwelveVolt,
    Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlertView {
    pub kind: AlertKind,
    pub severity: AlertSeverity,
    pub message: String,
}

/// Alerts derived from the current vehicle state, shared by the TUI strip and
/// the web banner so the two surfaces can't drift on what counts as an alert.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AlertsView {
    pub items: Vec<AlertView>,
    /// Set when the vehicle's cloud sync is stale: every alert value reflects
    /// state as of this long ago, not "now". The Rivian cloud serves the
    /// last-synced snapshot while the truck sleeps, so e.g. "window open" can
    /// persist long after the window was actually closed — renderers must
    /// qualify the alerts with this age instead of presenting them as live.
    pub data_age: Option<String>,
}

impl AlertsView {
    /// How stale `cloudConnection.lastSync` must be before we qualify alerts
    /// with an age. Within this window the data is effectively live.
    const STALE_AFTER_MINS: i64 = 10;

    pub fn from_state(vs: &VehicleStateFields) -> Self {
        let mut items = Vec::new();
        let mut push = |kind: AlertKind, severity: AlertSeverity, message: String| {
            items.push(AlertView {
                kind,
                severity,
                message,
            });
        };

        let door_fields = [
            &vs.door_front_left_closed,
            &vs.door_front_right_closed,
            &vs.door_rear_left_closed,
            &vs.door_rear_right_closed,
            &vs.closure_frunk_closed,
            &vs.closure_liftgate_closed,
            &vs.closure_tailgate_closed,
            &vs.closure_side_bin_left_closed,
            &vs.closure_side_bin_right_closed,
            &vs.closure_tonneau_closed,
        ];
        if door_fields.iter().any(|field| state_is(field, "open")) {
            push(
                AlertKind::Closure,
                AlertSeverity::Critical,
                "Door or hatch open".into(),
            );
        }

        let window_fields = [
            &vs.window_front_left_closed,
            &vs.window_front_right_closed,
            &vs.window_rear_left_closed,
            &vs.window_rear_right_closed,
        ];
        if window_fields.iter().any(|field| state_is(field, "open")) {
            push(
                AlertKind::Window,
                AlertSeverity::Critical,
                "Window open".into(),
            );
        }

        let tire_fields = [
            &vs.tire_pressure_status_front_left,
            &vs.tire_pressure_status_front_right,
            &vs.tire_pressure_status_rear_left,
            &vs.tire_pressure_status_rear_right,
        ];
        if tire_fields.iter().any(|field| {
            normalized_state(field)
                .map(|state| state.contains("low"))
                .unwrap_or(false)
        }) {
            push(
                AlertKind::Tire,
                AlertSeverity::Critical,
                "Low tire pressure".into(),
            );
        }

        let invalid_tires = collect_labels(
            [
                ("FL", &vs.tire_pressure_status_valid_front_left),
                ("FR", &vs.tire_pressure_status_valid_front_right),
                ("RL", &vs.tire_pressure_status_valid_rear_left),
                ("RR", &vs.tire_pressure_status_valid_rear_right),
            ],
            state_is_invalid,
        );
        if !invalid_tires.is_empty() {
            push(
                AlertKind::Tire,
                AlertSeverity::Warning,
                format!("Tire pressure sensor unavailable ({invalid_tires})"),
            );
        }

        if flag_is_active(vs, &vs.charging_disabled_all)
            || state_is(&vs.charging_disabled_all, "disabled")
        {
            push(
                AlertKind::Charging,
                AlertSeverity::Critical,
                "Charging disabled".into(),
            );
        }

        if state_is_derated(&vs.charger_derate_status) {
            push(
                AlertKind::Charging,
                AlertSeverity::Warning,
                "Charging power limited".into(),
            );
        }

        if flag_is_active(vs, &vs.battery_needs_lfp_calibration) {
            push(
                AlertKind::Battery,
                AlertSeverity::Warning,
                "Battery calibration needed".into(),
            );
        }

        if state_is_problem(&vs.battery_hv_thermal_event)
            || state_is_problem(&vs.battery_hv_thermal_event_propagation)
        {
            push(
                AlertKind::Battery,
                AlertSeverity::Critical,
                "High-voltage battery thermal warning".into(),
            );
        }

        if flag_is_active(vs, &vs.brake_fluid_low) || state_is_problem(&vs.brake_fluid_low) {
            push(
                AlertKind::Brake,
                AlertSeverity::Critical,
                "Brake fluid low".into(),
            );
        }

        let uncalibrated_windows = collect_labels(
            [
                ("FL", &vs.window_front_left_calibrated),
                ("FR", &vs.window_front_right_calibrated),
                ("RL", &vs.window_rear_left_calibrated),
                ("RR", &vs.window_rear_right_calibrated),
            ],
            state_is_uncalibrated,
        );
        if !uncalibrated_windows.is_empty() {
            push(
                AlertKind::Window,
                AlertSeverity::Warning,
                format!("Window calibration needed ({uncalibrated_windows})"),
            );
        }

        let failed_modules = collect_labels(
            [
                ("front fascia", &vs.btm_ff_hardware_failure_status),
                ("controls", &vs.btm_ic_hardware_failure_status),
                ("left door", &vs.btm_lfd_hardware_failure_status),
                ("overhead", &vs.btm_oc_hardware_failure_status),
                ("right door", &vs.btm_rfd_hardware_failure_status),
                ("rear fascia", &vs.btm_rf_hardware_failure_status),
            ],
            state_is_problem,
        );
        if !failed_modules.is_empty() {
            push(
                AlertKind::Hardware,
                AlertSeverity::Warning,
                format!("Bluetooth hardware fault ({failed_modules})"),
            );
        }

        if state_is_problem(&vs.wiper_fluid_state) {
            push(
                AlertKind::Fluid,
                AlertSeverity::Warning,
                "Washer fluid low".into(),
            );
        }

        if vs.get_f64(&vs.limited_accel_cold).unwrap_or(0.0) > 0.0
            || vs.get_f64(&vs.limited_regen_cold).unwrap_or(0.0) > 0.0
        {
            push(
                AlertKind::ColdLimits,
                AlertSeverity::Warning,
                "Cold-limited accel/regen".into(),
            );
        }

        if vs.update_available() {
            let available = vs.get_str(&vs.ota_available_version);
            push(
                AlertKind::Ota,
                AlertSeverity::Warning,
                format!("OTA available {available}"),
            );
        }

        let battery_12v = vs.get_str(&vs.twelve_volt_battery_health);
        if battery_12v != "unknown" && battery_12v != "NORMAL_OPERATION" {
            push(
                AlertKind::TwelveVolt,
                AlertSeverity::Warning,
                format!("12V {battery_12v}"),
            );
        }

        for (label, field) in [
            ("Service mode", &vs.service_mode),
            ("Car wash mode", &vs.car_wash_mode),
            ("Pet mode", &vs.pet_mode_status),
        ] {
            // Compare case-insensitively: the wire reports e.g. "Off" and
            // "Disabled" for pet mode, and a case-sensitive list silently
            // treats "Off" as active — the alert then never clears.
            let value = vs.get_str(field).to_ascii_lowercase();
            if !matches!(value.as_str(), "unknown" | "off" | "disabled") {
                push(AlertKind::Mode, AlertSeverity::Warning, label.into());
            }
        }

        let data_age = vs
            .last_sync()
            .and_then(parse_iso)
            .map(|(utc, _)| Utc::now().signed_duration_since(utc))
            .filter(|age| age.num_minutes() >= Self::STALE_AFTER_MINS)
            .map(|age| {
                if age.num_hours() >= 24 {
                    format!("{}d ago", age.num_days())
                } else if age.num_hours() >= 1 {
                    format!("{}h {}m ago", age.num_hours(), age.num_minutes() % 60)
                } else {
                    format!("{}m ago", age.num_minutes())
                }
            });

        Self { items, data_age }
    }
}

fn normalized_state(field: &Option<StateValue>) -> Option<String> {
    let value = field.as_ref()?.to_display();
    let normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    if normalized.is_empty()
        || matches!(
            normalized.as_str(),
            "unknown" | "unavailable" | "signal_not_available" | "undefined" | "null"
        )
    {
        None
    } else {
        Some(normalized)
    }
}

fn state_is(field: &Option<StateValue>, expected: &str) -> bool {
    normalized_state(field).is_some_and(|state| state == expected)
}

fn flag_is_active(vs: &VehicleStateFields, field: &Option<StateValue>) -> bool {
    vs.get_boolish(field) == Some(true)
        || normalized_state(field).is_some_and(|state| {
            matches!(
                state.as_str(),
                "active" | "detected" | "present" | "charging_disabled" | "all_disabled"
            )
        })
}

fn state_is_problem(field: &Option<StateValue>) -> bool {
    let Some(state) = normalized_state(field) else {
        return false;
    };
    if matches!(
        state.as_str(),
        "false" | "0" | "off" | "inactive" | "none" | "ok" | "valid"
    ) || state.starts_with("normal")
        || state.starts_with("no_")
        || state.starts_with("not_")
        || state.contains("no_fault")
        || state.contains("no_failure")
        || state.contains("not_active")
        || state.contains("not_detected")
    {
        return false;
    }

    matches!(
        state.as_str(),
        "true" | "1" | "on" | "active" | "detected" | "present" | "low" | "invalid"
    ) || [
        "fault",
        "failure",
        "failed",
        "error",
        "warning",
        "critical",
        "low",
        "thermal_event",
        "propagation",
    ]
    .iter()
    .any(|needle| state.contains(needle))
}

fn state_is_derated(field: &Option<StateValue>) -> bool {
    normalized_state(field).is_some_and(|state| {
        !matches!(
            state.as_str(),
            "none" | "normal" | "not_derated" | "false" | "0" | "off"
        ) && !state.starts_with("no_")
            && !state.starts_with("not_")
    })
}

fn state_is_invalid(field: &Option<StateValue>) -> bool {
    normalized_state(field).is_some_and(|state| matches!(state.as_str(), "invalid" | "false" | "0"))
}

fn state_is_uncalibrated(field: &Option<StateValue>) -> bool {
    normalized_state(field).is_some_and(|state| {
        matches!(
            state.as_str(),
            "false" | "0" | "invalid" | "uncalibrated" | "not_calibrated"
        )
    })
}

fn collect_labels<'a, const N: usize>(
    fields: [(&'a str, &'a Option<StateValue>); N],
    predicate: fn(&Option<StateValue>) -> bool,
) -> String {
    fields
        .into_iter()
        .filter_map(|(label, field)| predicate(field).then_some(label))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BatteryView {
    pub percent: String,
    pub percent_value: Option<f64>,
    pub range_miles: String,
    pub limit_percent: String,
    pub capacity_kwh: String,
    pub is_charging: bool,
    pub charging_label: String,
    pub time_to_full: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChargingView {
    pub state: String,
    pub status: String,
    pub time_to_full: String,
    pub port_state: String,
    pub is_active: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClimateView {
    pub cabin_temp_f: String,
    pub driver_set_temp_f: String,
    pub preconditioning: String,
    pub defrost: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VehicleView {
    pub power_state: String,
    pub gear: String,
    pub drive_mode: String,
    pub mileage: String,
    pub speed_mph: String,
    pub doors_locked: String,
    pub all_closed: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SoftwareView {
    pub current_version: String,
    pub current_version_date: String,
    pub current_version_number: String,
    pub current_git_hash: String,
    pub available_version: String,
    pub available_version_date: String,
    pub available_version_number: String,
    pub available_git_hash: String,
    pub status: String,
    pub install_type: String,
    pub install_ready: String,
    pub download_progress: String,
    pub install_progress: String,
    pub install_duration: String,
    pub install_time: String,
    pub progress_summary: String,
    pub update_available: bool,
    /// A download or install is actively progressing. Renderers gate the
    /// progress-detail rows on this instead of comparing formatted strings
    /// against the "—" placeholder, which silently breaks if the placeholder
    /// ever changes.
    pub is_installing: bool,
    /// The vehicle reports a staged install (otaInstallReady carries a real
    /// value) even if no progress is currently moving — e.g. downloaded and
    /// waiting for the scheduled install window.
    pub is_install_staged: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VehicleMetadataView {
    pub display_name: String,
    pub vin: String,
    pub model: String,
    pub trim: String,
    pub exterior_color: String,
    pub interior_color: String,
    pub drive_system: String,
    pub wheel: String,
    pub role: String,
    pub state: String,
    pub feature_count: usize,
    pub ota_early_access: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LocationView {
    pub coordinates: String,
    pub heading: String,
    pub altitude_ft: String,
    pub last_sync: String,
}

/// A single derived driving trip, pre-formatted for display. `when` is the
/// trip's end time (humanized); `distance`/`energy`/`efficiency` fall back to a
/// dash when the underlying snapshots couldn't supply them. `when_short` and
/// the raw `efficiency_value` exist for the TUI, which needs a compact
/// timestamp and a number to color-grade.
#[derive(Debug, Clone, Serialize)]
pub struct TripView {
    pub when: String,
    pub when_short: String,
    pub distance: String,
    pub energy: String,
    pub efficiency: String,
    pub efficiency_value: Option<f64>,
    pub soc_delta: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendPointView {
    pub battery_percent: Option<f64>,
    pub range_miles: Option<f64>,
    pub mileage_miles: Option<f64>,
    pub speed_mph: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveChargeView {
    pub power_kw: String,
    pub soc_percent: String,
    pub energy_delivered_kwh: String,
    pub range_added_miles: String,
    pub session_efficiency: String,
    pub time_remaining: String,
    pub charger_id: String,
    pub charger_state: String,
    pub started: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveChargeHistoryView {
    pub point_count: usize,
    pub peak_kw: String,
    pub average_kw: String,
    pub latest_kw: String,
    pub started: String,
    pub updated: String,
    pub points: Vec<LiveChargeHistoryPointView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LiveChargeHistoryPointView {
    pub kw: f64,
    pub time: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OtaDetailsView {
    pub current_url: Option<String>,
    pub current_version: String,
    pub current_locale: String,
    pub available_url: Option<String>,
    pub available_version: String,
    pub available_locale: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChargingStatsView {
    pub session_count: String,
    pub total_energy_kwh: String,
    pub total_range_miles: String,
    pub avg_mi_per_kwh: String,
    pub best_mi_per_kwh: String,
    pub worst_mi_per_kwh: String,
    pub home_summary: String,
    pub public_summary: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChargeInsightView {
    pub when: String,
    pub when_short: String,
    pub energy_kwh: String,
    pub range_added_miles: String,
    pub efficiency_mi_per_kwh: String,
    pub location: String,
    pub charger_type: String,
}

impl DashboardView {
    pub fn from_data(data: &DashboardData) -> Self {
        let (battery, charging, climate, vehicle, software, location, alerts) =
            match data.vehicle_state.as_ref() {
                Some(vs) => (
                    BatteryView::from_state(vs),
                    ChargingView::from_state(vs),
                    ClimateView::from_state(vs),
                    VehicleView::from_state(vs),
                    SoftwareView::from_state(vs),
                    LocationView::from_state(vs),
                    AlertsView::from_state(vs),
                ),
                None => Default::default(),
            };

        Self {
            vehicle_id: data.vehicle_id.clone(),
            vehicle_label: data
                .vehicle_metadata
                .as_ref()
                .map(|meta| meta.display_name.clone())
                .or_else(|| data.vehicle_id.clone())
                .unwrap_or_else(|| "no vehicle".into()),
            vehicle_metadata: data
                .vehicle_metadata
                .as_ref()
                .map(VehicleMetadataView::from),
            last_update_human: format_last_update(data.last_update),
            last_update_iso: data.last_update.map(|dt| dt.to_rfc3339()),
            has_data: data.vehicle_state.is_some(),
            battery,
            charging,
            climate,
            vehicle,
            software,
            location,
            trend: data.recent_trend.iter().map(TrendPointView::from).collect(),
            trips: data.recent_trips.iter().map(TripView::from).collect(),
            last_charge: data
                .last_charge_session
                .as_ref()
                .map(ChargeInsightView::from),
            live_charge: data
                .live_charging_session
                .as_ref()
                .map(LiveChargeView::from),
            live_charge_history: data
                .live_charging_history
                .as_ref()
                .map(LiveChargeHistoryView::from),
            ota_details: data.ota_update_details.as_ref().map(OtaDetailsView::from),
            charging_stats: data.charging_stats.as_ref().map(ChargingStatsView::from),
            alerts,
        }
    }
}

impl BatteryView {
    fn from_state(vs: &VehicleStateFields) -> Self {
        let is_charging = vs.is_actively_charging();
        let time_to_full = vs.time_to_full().unwrap_or_else(|| "—".into());
        let charging_label = if is_charging {
            if time_to_full != "—" {
                format!("Charging · {time_to_full} to full")
            } else {
                "Charging".to_string()
            }
        } else {
            let state = vs.charger_state_str();
            if state == "unknown" {
                "Not charging".to_string()
            } else {
                state.replace('_', " ")
            }
        };

        Self {
            percent: vs
                .battery_percent()
                .map(|v| format!("{v:.0}%"))
                .unwrap_or_else(|| "—".into()),
            percent_value: vs.battery_percent(),
            range_miles: vs
                .range_miles()
                .map(|v| format!("{v:.0} mi"))
                .unwrap_or_else(|| "—".into()),
            limit_percent: vs
                .battery_limit_percent()
                .map(|v| format!("{v:.0}%"))
                .unwrap_or_else(|| "—".into()),
            capacity_kwh: vs
                .battery_capacity_kwh()
                .map(|v| format!("{v:.1} kWh"))
                .unwrap_or_else(|| "—".into()),
            is_charging,
            charging_label,
            time_to_full,
        }
    }
}

impl ChargingView {
    fn from_state(vs: &VehicleStateFields) -> Self {
        Self {
            state: humanize_charger_token(vs.charger_state_str()),
            status: humanize_charger_token(vs.charger_status_str()),
            time_to_full: vs.time_to_full().unwrap_or_else(|| "—".into()),
            port_state: humanize_charger_token(vs.get_str(&vs.charge_port_state)),
            is_active: vs.is_actively_charging(),
        }
    }
}

/// Turn Rivian's snake_case charger tokens into something readable. Strips
/// the `chrgr_sts_` prefix on status tokens so "chrgr_sts_not_connected"
/// reads as "not connected" rather than "chrgr sts not connected".
fn humanize_charger_token(raw: &str) -> String {
    let stripped = raw.strip_prefix("chrgr_sts_").unwrap_or(raw);
    stripped.replace('_', " ")
}

impl ClimateView {
    fn from_state(vs: &VehicleStateFields) -> Self {
        Self {
            cabin_temp_f: vs
                .cabin_temp_f()
                .map(|v| format!("{v:.0}°F"))
                .unwrap_or_else(|| "—".into()),
            driver_set_temp_f: vs
                .driver_temp_f()
                .map(|v| format!("{v:.0}°F"))
                .unwrap_or_else(|| "—".into()),
            preconditioning: vs.get_str(&vs.cabin_preconditioning_status).to_string(),
            defrost: vs.get_str(&vs.defrost_defog_status).to_string(),
        }
    }
}

impl VehicleView {
    fn from_state(vs: &VehicleStateFields) -> Self {
        let doors_all_locked = [
            &vs.door_front_left_locked,
            &vs.door_front_right_locked,
            &vs.door_rear_left_locked,
            &vs.door_rear_right_locked,
        ]
        .iter()
        .map(|f| vs.get_boolish(f))
        .collect::<Vec<_>>();

        let doors_locked = if doors_all_locked.iter().all(|d| *d == Some(true)) {
            "all locked"
        } else if doors_all_locked.iter().all(|d| *d == Some(false)) {
            "unlocked"
        } else if doors_all_locked.iter().any(Option::is_none) {
            "unknown"
        } else {
            "mixed"
        };

        let closures = [
            &vs.door_front_left_closed,
            &vs.door_front_right_closed,
            &vs.door_rear_left_closed,
            &vs.door_rear_right_closed,
            &vs.closure_frunk_closed,
            &vs.closure_liftgate_closed,
            &vs.closure_tailgate_closed,
            &vs.closure_side_bin_left_closed,
            &vs.closure_side_bin_right_closed,
            &vs.closure_tonneau_closed,
        ];
        let all_closed = if closures.iter().all(|f| vs.get_boolish(f).unwrap_or(true)) {
            "all closed"
        } else {
            "open"
        };

        Self {
            power_state: vs.power_state_str().to_string(),
            gear: vs.gear_str().to_string(),
            drive_mode: vs.drive_mode_str().to_string(),
            mileage: vs
                .mileage()
                .map(|v| format!("{v:.0} mi"))
                .unwrap_or_else(|| "—".into()),
            speed_mph: vs
                .speed_mph()
                .map(|v| format!("{v:.0} mph"))
                .unwrap_or_else(|| "—".into()),
            doors_locked: doors_locked.to_string(),
            all_closed: all_closed.to_string(),
        }
    }
}

impl SoftwareView {
    pub(crate) fn from_state(vs: &VehicleStateFields) -> Self {
        fn display(
            vs: &VehicleStateFields,
            field: &Option<crate::api::types::StateValue>,
        ) -> String {
            let s = vs.get_str(field);
            if s == "unknown" || s.is_empty() {
                "—".into()
            } else {
                s.to_string()
            }
        }

        fn version_date(
            vs: &VehicleStateFields,
            year: &Option<crate::api::types::StateValue>,
            week: &Option<crate::api::types::StateValue>,
        ) -> String {
            match (vs.get_f64(year), vs.get_f64(week)) {
                (Some(y), Some(w)) if y > 0.0 => format!("{y:.0}w{w:02.0}"),
                _ => "—".into(),
            }
        }

        let current = display(vs, &vs.ota_current_version);
        let available_raw = display(vs, &vs.ota_available_version);
        let current_version_number = display(vs, &vs.ota_current_version_number);
        let available_version_number_raw = display(vs, &vs.ota_available_version_number);
        let current_git_hash = short_hash(&display(vs, &vs.ota_current_version_git_hash));
        let available_git_hash_raw = short_hash(&display(vs, &vs.ota_available_version_git_hash));

        let update_available = vs.update_available();
        let available_version = if update_available {
            available_raw
        } else {
            "—".into()
        };
        let available_version_date = if update_available {
            version_date(
                vs,
                &vs.ota_available_version_year,
                &vs.ota_available_version_week,
            )
        } else {
            "—".into()
        };
        let available_version_number = if update_available {
            available_version_number_raw
        } else {
            "—".into()
        };
        let available_git_hash = if update_available {
            available_git_hash_raw
        } else {
            "—".into()
        };

        let downloading = vs.get_f64(&vs.ota_download_progress).filter(|v| *v > 0.0);
        let installing = vs.get_f64(&vs.ota_install_progress).filter(|v| *v > 0.0);

        let download_progress = downloading
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into());
        let install_progress = installing
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into());

        // install_ready is a string on the wire (e.g. "true"/"not_ready"),
        // so use get_str and normalize.
        let install_ready = {
            let raw = vs.get_str(&vs.ota_install_ready);
            if raw == "unknown" || raw.is_empty() {
                "—".into()
            } else {
                raw.replace('_', " ")
            }
        };

        let install_duration = vs
            .get_f64(&vs.ota_install_duration)
            .filter(|v| *v > 0.0)
            .map(|mins| {
                let m = mins as u64;
                let h = m / 60;
                let rem = m % 60;
                if h > 0 {
                    format!("{h}h {rem}m")
                } else {
                    format!("{rem}m")
                }
            })
            .unwrap_or_else(|| "—".into());

        let install_time = display(vs, &vs.ota_install_time);

        // Progress summary: prefer live download/install %, otherwise "idle"
        // (the existing helper falls back to install_type which reads weird
        // when nothing is actually happening).
        let progress_summary = if let Some(v) = installing {
            format!("installing {v:.0}%")
        } else if let Some(v) = downloading {
            format!("downloading {v:.0}%")
        } else {
            "idle".into()
        };

        let is_installing = installing.is_some() || downloading.is_some();
        let is_install_staged = install_ready != "—";

        Self {
            current_version: current,
            current_version_date: version_date(
                vs,
                &vs.ota_current_version_year,
                &vs.ota_current_version_week,
            ),
            current_version_number,
            current_git_hash,
            available_version,
            available_version_date,
            available_version_number,
            available_git_hash,
            status: display(vs, &vs.ota_status),
            install_type: display(vs, &vs.ota_install_type),
            install_ready,
            download_progress,
            install_progress,
            install_duration,
            install_time,
            progress_summary,
            update_available,
            is_installing,
            is_install_staged,
        }
    }
}

fn short_hash(value: &str) -> String {
    if value == "—" || value.is_empty() {
        "—".into()
    } else {
        value.chars().take(8).collect()
    }
}

impl LocationView {
    fn from_state(vs: &VehicleStateFields) -> Self {
        Self {
            coordinates: vs.location_summary().unwrap_or_else(|| "—".into()),
            heading: vs.heading_summary().unwrap_or_else(|| "—".into()),
            altitude_ft: vs
                .altitude_ft()
                .map(|v| format!("{v:.0} ft"))
                .unwrap_or_else(|| "—".into()),
            last_sync: vs
                .last_sync()
                .map(humanize_iso)
                .unwrap_or_else(|| "—".into()),
        }
    }
}

impl From<&VehicleMetadata> for VehicleMetadataView {
    fn from(meta: &VehicleMetadata) -> Self {
        let dash = || "—".to_string();
        let model = match (&meta.model, meta.model_year) {
            (Some(model), Some(year)) => format!("{year} {model}"),
            (Some(model), None) => model.clone(),
            (None, Some(year)) => year.to_string(),
            (None, None) => dash(),
        };
        let role = if meta.roles.is_empty() {
            dash()
        } else {
            meta.roles.join(", ")
        };
        let ota_early_access = match meta.ota_early_access_status {
            Some(true) => "enabled".into(),
            Some(false) => "disabled".into(),
            None => dash(),
        };

        Self {
            display_name: meta.display_name.clone(),
            vin: meta.vin.clone().unwrap_or_else(dash),
            model,
            trim: meta.trim.clone().unwrap_or_else(dash),
            exterior_color: meta.exterior_color.clone().unwrap_or_else(dash),
            interior_color: meta.interior_color.clone().unwrap_or_else(dash),
            drive_system: meta.drive_system.clone().unwrap_or_else(dash),
            wheel: meta.wheel.clone().unwrap_or_else(dash),
            role,
            state: meta.state.clone().unwrap_or_else(dash),
            feature_count: meta.supported_features.len(),
            ota_early_access,
        }
    }
}

impl From<&VehicleTrendPoint> for TrendPointView {
    fn from(p: &VehicleTrendPoint) -> Self {
        Self {
            battery_percent: p.battery_level,
            range_miles: p.range_km.map(|km| km / KM_PER_MI),
            mileage_miles: p.vehicle_mileage_m.map(|m| m / METERS_PER_MI),
            speed_mph: p.speed_kmh.map(|kmh| kmh / KM_PER_MI),
        }
    }
}

impl From<&Trip> for TripView {
    fn from(trip: &Trip) -> Self {
        let when_ts = trip.when_ts();
        let when = when_ts.map(humanize_iso).unwrap_or_else(|| "—".into());
        let when_short = when_ts
            .map(humanize_iso_short)
            .unwrap_or_else(|| "—".into());

        let soc_delta = match (trip.start_soc, trip.end_soc) {
            (Some(start), Some(end)) => format!("{start:.0}% → {end:.0}%"),
            _ => "—".into(),
        };

        Self {
            when,
            when_short,
            distance: format!("{:.1} mi", trip.distance_mi),
            energy: trip
                .energy_kwh
                .map(|kwh| format!("{kwh:.1} kWh"))
                .unwrap_or_else(|| "—".into()),
            efficiency: trip
                .efficiency_mi_per_kwh
                .map(|eff| format!("{eff:.1} mi/kWh"))
                .unwrap_or_else(|| "—".into()),
            efficiency_value: trip.efficiency_mi_per_kwh,
            soc_delta,
        }
    }
}

impl From<&ChargeSessionSummary> for ChargeInsightView {
    fn from(session: &ChargeSessionSummary) -> Self {
        let when_ts = session
            .end_instant
            .as_deref()
            .or(session.start_instant.as_deref());
        let when = when_ts.map(humanize_iso).unwrap_or_else(|| "—".into());
        let when_short = when_ts
            .map(humanize_iso_short)
            .unwrap_or_else(|| "—".into());

        let location = match (session.vendor.as_deref(), session.city.as_deref()) {
            (Some(v), Some(c)) if !v.is_empty() && !c.is_empty() => format!("{v} · {c}"),
            (Some(v), _) if !v.is_empty() => v.to_string(),
            (_, Some(c)) if !c.is_empty() => c.to_string(),
            _ => {
                if session.is_home_charger == Some(true) {
                    "Home".into()
                } else {
                    "—".into()
                }
            }
        };

        let charger_type = session
            .charger_type
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "—".into());

        let efficiency_mi_per_kwh = match (session.range_added_km, session.total_energy_kwh) {
            (Some(km), Some(kwh)) if kwh > 0.0 && km > 0.0 => {
                format!("{:.1} mi/kWh", (km / KM_PER_MI) / kwh)
            }
            _ => "—".into(),
        };

        Self {
            when,
            when_short,
            energy_kwh: session
                .total_energy_kwh
                .map(|v| format!("{v:.1} kWh"))
                .unwrap_or_else(|| "—".into()),
            range_added_miles: session
                .range_added_km
                .map(|km| format!("{:.0} mi", km / KM_PER_MI))
                .unwrap_or_else(|| "—".into()),
            efficiency_mi_per_kwh,
            location,
            charger_type,
        }
    }
}

impl From<&LiveChargingSession> for LiveChargeView {
    fn from(s: &LiveChargingSession) -> Self {
        let dash = || "—".to_string();

        let power_kw = s
            .power_kw()
            .map(|kw| format!("{kw:.1} kW"))
            .unwrap_or_else(dash);
        let soc_percent = s
            .soc_percent()
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(dash);
        let energy_delivered_kwh = s
            .total_energy_kwh()
            .map(|v| format!("{v:.1} kWh"))
            .unwrap_or_else(dash);
        let range_added_miles = s
            .range_added_miles()
            .map(|v| format!("{v:.0} mi"))
            .unwrap_or_else(dash);
        let session_efficiency = s
            .efficiency_mi_per_kwh()
            .map(|v| format!("{v:.1} mi/kWh"))
            .unwrap_or_else(dash);
        let time_remaining = s
            .time_remaining_min()
            .map(|mins| {
                let m = mins as u64;
                if m >= 60 {
                    format!("{}h {}m", m / 60, m % 60)
                } else {
                    format!("{m}m")
                }
            })
            .unwrap_or_else(dash);
        let charger_id = s.charger_id.clone().unwrap_or_else(dash);
        let charger_state = s
            .vehicle_charger_state_str()
            .map(|s| s.replace('_', " "))
            .unwrap_or_else(dash);
        let started = s
            .start_time
            .as_deref()
            .map(humanize_iso)
            .unwrap_or_else(dash);

        Self {
            power_kw,
            soc_percent,
            energy_delivered_kwh,
            range_added_miles,
            session_efficiency,
            time_remaining,
            charger_id,
            charger_state,
            started,
        }
    }
}

impl From<&LiveSessionHistory> for LiveChargeHistoryView {
    fn from(history: &LiveSessionHistory) -> Self {
        let points: Vec<LiveChargeHistoryPointView> = history
            .chart_data
            .iter()
            .filter_map(|point| {
                Some(LiveChargeHistoryPointView {
                    kw: point.kw?,
                    time: point.time.clone()?,
                })
            })
            .collect();

        let point_count = points.len();
        let peak = points
            .iter()
            .map(|p| p.kw)
            .fold(None, |acc: Option<f64>, kw| {
                Some(acc.map_or(kw, |current| current.max(kw)))
            });
        let total: f64 = points.iter().map(|p| p.kw).sum();
        let average = if point_count > 0 {
            Some(total / point_count as f64)
        } else {
            None
        };
        let latest = points.last().map(|p| p.kw);
        let started = points
            .first()
            .map(|p| humanize_iso(&p.time))
            .unwrap_or_else(|| "—".into());
        let updated = points
            .last()
            .map(|p| humanize_iso(&p.time))
            .unwrap_or_else(|| "—".into());

        Self {
            point_count,
            peak_kw: peak
                .map(|kw| format!("{kw:.1} kW"))
                .unwrap_or_else(|| "—".into()),
            average_kw: average
                .map(|kw| format!("{kw:.1} kW"))
                .unwrap_or_else(|| "—".into()),
            latest_kw: latest
                .map(|kw| format!("{kw:.1} kW"))
                .unwrap_or_else(|| "—".into()),
            started,
            updated,
            points,
        }
    }
}

impl From<&OtaUpdateDetails> for OtaDetailsView {
    fn from(details: &OtaUpdateDetails) -> Self {
        let field =
            |detail: Option<&crate::api::types::OtaUpdateDetail>,
             f: fn(&crate::api::types::OtaUpdateDetail) -> Option<&String>| {
                detail
                    .and_then(f)
                    .filter(|value| !value.is_empty())
                    .cloned()
                    .unwrap_or_else(|| "—".into())
            };

        Self {
            current_url: details
                .current
                .as_ref()
                .and_then(|detail| detail.url.clone()),
            current_version: field(details.current.as_ref(), |detail| detail.version.as_ref()),
            current_locale: field(details.current.as_ref(), |detail| detail.locale.as_ref()),
            available_url: details
                .available
                .as_ref()
                .and_then(|detail| detail.url.clone()),
            available_version: field(details.available.as_ref(), |detail| detail.version.as_ref()),
            available_locale: field(details.available.as_ref(), |detail| detail.locale.as_ref()),
        }
    }
}

impl From<&ChargingStats> for ChargingStatsView {
    fn from(stats: &ChargingStats) -> Self {
        let fmt_eff = |v: Option<f64>| {
            v.map(|x| format!("{x:.2} mi/kWh"))
                .unwrap_or_else(|| "—".into())
        };

        let home_summary = if stats.home_session_count > 0 {
            format!(
                "{} session{} · {}",
                stats.home_session_count,
                if stats.home_session_count == 1 {
                    ""
                } else {
                    "s"
                },
                fmt_eff(stats.home_avg_mi_per_kwh),
            )
        } else {
            "no sessions".into()
        };
        let public_summary = if stats.public_session_count > 0 {
            format!(
                "{} session{} · {}",
                stats.public_session_count,
                if stats.public_session_count == 1 {
                    ""
                } else {
                    "s"
                },
                fmt_eff(stats.public_avg_mi_per_kwh),
            )
        } else {
            "no sessions".into()
        };

        Self {
            session_count: stats.session_count.to_string(),
            total_energy_kwh: format!("{:.0} kWh", stats.total_energy_kwh),
            total_range_miles: format!("{:.0} mi", stats.total_range_km / KM_PER_MI),
            avg_mi_per_kwh: fmt_eff(stats.avg_mi_per_kwh),
            best_mi_per_kwh: fmt_eff(stats.best_mi_per_kwh),
            worst_mi_per_kwh: fmt_eff(stats.worst_mi_per_kwh),
            home_summary,
            public_summary,
        }
    }
}

fn format_last_update(ts: Option<DateTime<Utc>>) -> String {
    let Some(ts) = ts else {
        return "never".to_string();
    };
    let local: DateTime<Local> = ts.into();
    // Include the UTC offset so a remote viewer can disambiguate the
    // server's local time from their own.
    format!(
        "{} ({})",
        local.format("%Y-%m-%d %H:%M:%S %z"),
        relative_age(ts)
    )
}

fn relative_age(ts: DateTime<Utc>) -> String {
    let elapsed = Utc::now().signed_duration_since(ts);
    let secs = elapsed.num_seconds();
    if secs < 0 {
        // Clock skew or future timestamp — fall back to a stable label.
        return "just now".into();
    }
    if secs < 60 {
        "just now".into()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m ago", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else {
        format!("{}d ago", elapsed.num_days())
    }
}

/// Parse an RFC3339 timestamp into (UTC, Local) once; every humanize_*
/// variant formats off this single parser so they can't drift on parse or
/// timezone behavior.
fn parse_iso(ts: &str) -> Option<(DateTime<Utc>, DateTime<Local>)> {
    let utc = DateTime::parse_from_rfc3339(ts).ok()?.with_timezone(&Utc);
    Some((utc, utc.into()))
}

/// Compact local-time label (`Jun 08 14:30`) for space-constrained renderers
/// like the TUI trips panel. Falls back to the raw string on parse failure.
pub fn humanize_iso_short(ts: &str) -> String {
    match parse_iso(ts) {
        Some((_, local)) => local.format("%b %d %H:%M").to_string(),
        None => ts.to_string(),
    }
}

/// Local time + relative age without a UTC offset (`Jun 08 14:30 (2h ago)`).
/// For the TUI, where the viewer is by definition in the server's timezone —
/// the `%z` suffix that remote web viewers need only eats panel width here.
pub fn humanize_iso_local(ts: &str) -> String {
    match parse_iso(ts) {
        Some((utc, local)) => {
            format!("{} ({})", local.format("%b %d %H:%M"), relative_age(utc))
        }
        None => ts.to_string(),
    }
}

/// Format an RFC3339 timestamp as local-time + UTC offset + relative age.
/// Falls back to the original string if parsing fails so we never silently
/// lose data. The offset lets a remote web viewer disambiguate the server's
/// local time from their own.
pub fn humanize_iso(ts: &str) -> String {
    match parse_iso(ts) {
        Some((utc, local)) => {
            format!("{} ({})", local.format("%b %d %H:%M %z"), relative_age(utc))
        }
        None => ts.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{StateValue, VehicleStateFields};
    use serde_json::json;

    fn state_with_interior_temp_c(c: f64) -> VehicleStateFields {
        VehicleStateFields {
            cabin_climate_interior_temperature: Some(StateValue { value: json!(c) }),
            ..Default::default()
        }
    }

    fn state_value(value: serde_json::Value) -> Option<StateValue> {
        Some(StateValue { value })
    }

    #[test]
    fn climate_view_converts_celsius_to_fahrenheit() {
        let vs = state_with_interior_temp_c(20.0);
        let view = ClimateView::from_state(&vs);
        assert_eq!(view.cabin_temp_f, "68°F");
    }

    #[test]
    fn empty_dashboard_data_has_no_data() {
        let data = DashboardData::default();
        let view = DashboardView::from_data(&data);
        assert!(!view.has_data);
        assert_eq!(view.last_update_human, "never");
    }

    #[test]
    fn humanize_iso_returns_input_for_unparseable_string() {
        assert_eq!(humanize_iso("not-a-date"), "not-a-date");
    }

    #[test]
    fn pet_mode_off_does_not_alert_regardless_of_case() {
        // The wire reports "Off" / "Disabled" (observed in real snapshots);
        // a case-sensitive clear-list treated "Off" as active and the pet
        // mode alert never cleared.
        for cleared in ["Off", "off", "Disabled", "disabled", "unknown"] {
            let vs = VehicleStateFields {
                pet_mode_status: Some(StateValue {
                    value: json!(cleared),
                }),
                ..Default::default()
            };
            let alerts = AlertsView::from_state(&vs);
            assert!(
                alerts.items.is_empty(),
                "pet mode '{cleared}' must not alert: {:?}",
                alerts.items
            );
        }

        let vs = VehicleStateFields {
            pet_mode_status: Some(StateValue { value: json!("On") }),
            ..Default::default()
        };
        let alerts = AlertsView::from_state(&vs);
        assert_eq!(alerts.items.len(), 1);
        assert_eq!(alerts.items[0].message, "Pet mode");
    }

    #[test]
    fn normal_diagnostic_states_stay_quiet() {
        let vs = VehicleStateFields {
            battery_needs_lfp_calibration: state_value(json!(false)),
            charging_disabled_all: state_value(json!(false)),
            charger_derate_status: state_value(json!("not_derated")),
            battery_hv_thermal_event: state_value(json!("normal_operation")),
            battery_hv_thermal_event_propagation: state_value(json!("no_propagation")),
            brake_fluid_low: state_value(json!(false)),
            btm_ff_hardware_failure_status: state_value(json!("no_failure")),
            tire_pressure_status_valid_front_left: state_value(json!("valid")),
            window_front_left_calibrated: state_value(json!("calibrated")),
            wiper_fluid_state: state_value(json!("normal")),
            ..Default::default()
        };

        assert!(AlertsView::from_state(&vs).items.is_empty());
    }

    #[test]
    fn diagnostic_faults_become_compact_actionable_alerts() {
        let vs = VehicleStateFields {
            battery_needs_lfp_calibration: state_value(json!(true)),
            charging_disabled_all: state_value(json!(true)),
            charger_derate_status: state_value(json!("reduced")),
            battery_hv_thermal_event: state_value(json!("thermal_event_active")),
            brake_fluid_low: state_value(json!("low")),
            btm_lfd_hardware_failure_status: state_value(json!("hardware_failure")),
            tire_pressure_status_valid_front_left: state_value(json!("invalid")),
            tire_pressure_status_valid_rear_right: state_value(json!(false)),
            window_front_right_calibrated: state_value(json!(false)),
            wiper_fluid_state: state_value(json!("low")),
            ..Default::default()
        };

        let alerts = AlertsView::from_state(&vs);
        let messages = alerts
            .items
            .iter()
            .map(|alert| alert.message.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "Tire pressure sensor unavailable (FL, RR)",
            "Charging disabled",
            "Charging power limited",
            "Battery calibration needed",
            "High-voltage battery thermal warning",
            "Brake fluid low",
            "Window calibration needed (FR)",
            "Bluetooth hardware fault (left door)",
            "Washer fluid low",
        ] {
            assert!(messages.contains(&expected), "missing alert: {expected}");
        }
        assert!(alerts
            .items
            .iter()
            .any(|alert| alert.severity == AlertSeverity::Critical));
    }

    #[test]
    fn side_bins_and_tonneau_are_included_in_closure_summary() {
        let vs = VehicleStateFields {
            closure_side_bin_left_closed: state_value(json!("open")),
            closure_tonneau_closed: state_value(json!("open")),
            ..Default::default()
        };

        let alerts = AlertsView::from_state(&vs);
        assert_eq!(alerts.items.len(), 1);
        assert_eq!(alerts.items[0].message, "Door or hatch open");
        assert_eq!(VehicleView::from_state(&vs).all_closed, "open");
    }

    #[test]
    fn alerts_carry_data_age_when_cloud_sync_is_stale() {
        use crate::api::types::CloudConnection;

        let stale_sync = (Utc::now() - chrono::Duration::minutes(95)).to_rfc3339();
        let vs = VehicleStateFields {
            window_front_left_closed: Some(StateValue {
                value: json!("open"),
            }),
            cloud_connection: Some(CloudConnection {
                last_sync: Some(stale_sync),
            }),
            ..Default::default()
        };

        let alerts = AlertsView::from_state(&vs);
        assert_eq!(alerts.items.len(), 1);
        assert_eq!(alerts.items[0].message, "Window open");
        let age = alerts.data_age.expect("stale sync must set data_age");
        assert!(age.starts_with("1h"), "expected ~1h35m age, got {age}");

        // Fresh sync → no age qualifier; the data is effectively live.
        let vs_fresh = VehicleStateFields {
            window_front_left_closed: Some(StateValue {
                value: json!("open"),
            }),
            cloud_connection: Some(CloudConnection {
                last_sync: Some(Utc::now().to_rfc3339()),
            }),
            ..Default::default()
        };
        assert!(AlertsView::from_state(&vs_fresh).data_age.is_none());
    }

    #[test]
    fn humanize_iso_renders_local_time_for_valid_rfc3339() {
        let formatted = humanize_iso("2026-02-28T15:34:30Z");
        assert!(
            formatted.contains("(") && formatted.contains("ago"),
            "expected relative age suffix, got {formatted}"
        );
        assert!(
            !formatted.contains("2026-02-28T"),
            "raw RFC3339 must not leak through, got {formatted}"
        );
    }
}
