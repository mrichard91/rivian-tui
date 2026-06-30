//! Presentation layer shared between the web dashboard and (eventually) any
//! other non-TUI renderer. The TUI still draws directly from the raw
//! `VehicleStateFields` helpers; this module exists so the web server can
//! render pre-formatted strings and serialize them as JSON without pulling in
//! ratatui-specific code.

use chrono::{DateTime, Local, Utc};
use serde::Serialize;

use crate::api::types::{LiveChargingSession, VehicleStateFields, KM_PER_MI, METERS_PER_MI};
use crate::app::DashboardData;
use crate::db::{ChargeSessionSummary, ChargingStats, Trip, VehicleTrendPoint};

/// A flat, pre-formatted view of the dashboard intended for HTML/JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardView {
    pub vehicle_id: Option<String>,
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
        ];
        if door_fields
            .iter()
            .any(|field| matches!(field.as_ref().and_then(|v| v.as_str()), Some("open")))
        {
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
        if window_fields
            .iter()
            .any(|field| matches!(field.as_ref().and_then(|v| v.as_str()), Some("open")))
        {
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
            field
                .as_ref()
                .and_then(|v| v.as_str())
                .map(|s| s.to_ascii_lowercase().contains("low"))
                .unwrap_or(false)
        }) {
            push(
                AlertKind::Tire,
                AlertSeverity::Critical,
                "Low tire pressure".into(),
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
    pub available_version: String,
    pub available_version_date: String,
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
    fn from_state(vs: &VehicleStateFields) -> Self {
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
            available_version,
            available_version_date,
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
