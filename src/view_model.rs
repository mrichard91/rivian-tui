//! Presentation layer shared between the web dashboard and (eventually) any
//! other non-TUI renderer. The TUI still draws directly from the raw
//! `VehicleStateFields` helpers; this module exists so the web server can
//! render pre-formatted strings and serialize them as JSON without pulling in
//! ratatui-specific code.

use chrono::{DateTime, Local, Utc};
use serde::Serialize;

use crate::api::types::VehicleStateFields;
use crate::app::DashboardData;
use crate::db::{ChargeSessionSummary, VehicleTrendPoint};

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
    pub last_charge: Option<ChargeInsightView>,
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
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LocationView {
    pub coordinates: String,
    pub heading: String,
    pub altitude_ft: String,
    pub last_sync: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrendPointView {
    pub battery_percent: Option<f64>,
    pub range_miles: Option<f64>,
    pub mileage_miles: Option<f64>,
    pub speed_mph: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChargeInsightView {
    pub when: String,
    pub energy_kwh: String,
    pub range_added_miles: String,
    pub location: String,
    pub charger_type: String,
}

impl DashboardView {
    pub fn from_data(data: &DashboardData) -> Self {
        let (battery, charging, climate, vehicle, software, location) =
            match data.vehicle_state.as_ref() {
                Some(vs) => (
                    BatteryView::from_state(vs),
                    ChargingView::from_state(vs),
                    ClimateView::from_state(vs),
                    VehicleView::from_state(vs),
                    SoftwareView::from_state(vs),
                    LocationView::from_state(vs),
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
            last_charge: data
                .last_charge_session
                .as_ref()
                .map(ChargeInsightView::from),
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
            state: vs.charger_state_str().to_string(),
            status: vs.charger_status_str().to_string(),
            time_to_full: vs.time_to_full().unwrap_or_else(|| "—".into()),
            port_state: vs.get_str(&vs.charge_port_state).to_string(),
            is_active: vs.is_actively_charging(),
        }
    }
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
        let all_closed = if closures
            .iter()
            .all(|f| vs.get_boolish(f).unwrap_or(true))
        {
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
        fn display(vs: &VehicleStateFields, field: &Option<crate::api::types::StateValue>) -> String {
            let s = vs.get_str(field);
            if s == "unknown" {
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
                (Some(y), Some(w)) => format!("{y:.0}w{w:02.0}"),
                _ => "—".into(),
            }
        }

        let current = display(vs, &vs.ota_current_version);
        let available = display(vs, &vs.ota_available_version);
        let update_available = available != "—" && available != current;

        let download_progress = vs
            .get_f64(&vs.ota_download_progress)
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into());
        let install_progress = vs
            .get_f64(&vs.ota_install_progress)
            .map(|v| format!("{v:.0}%"))
            .unwrap_or_else(|| "—".into());

        let install_ready = match vs.get_boolish(&vs.ota_install_ready) {
            Some(true) => "ready".into(),
            Some(false) => "not ready".into(),
            None => "—".into(),
        };

        let install_duration = vs
            .get_f64(&vs.ota_install_duration)
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

        Self {
            current_version: current,
            current_version_date: version_date(
                vs,
                &vs.ota_current_version_year,
                &vs.ota_current_version_week,
            ),
            available_version: available,
            available_version_date: version_date(
                vs,
                &vs.ota_available_version_year,
                &vs.ota_available_version_week,
            ),
            status: display(vs, &vs.ota_status),
            install_type: display(vs, &vs.ota_install_type),
            install_ready,
            download_progress,
            install_progress,
            install_duration,
            install_time: display(vs, &vs.ota_install_time),
            progress_summary: vs.ota_progress_summary().unwrap_or_else(|| "idle".into()),
            update_available,
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
            last_sync: vs.last_sync().unwrap_or("—").to_string(),
        }
    }
}

impl From<&VehicleTrendPoint> for TrendPointView {
    fn from(p: &VehicleTrendPoint) -> Self {
        Self {
            battery_percent: p.battery_level,
            range_miles: p.range_km.map(|km| km / 1.60934),
            mileage_miles: p.vehicle_mileage_m.map(|m| m / 1609.344),
            speed_mph: p.speed_kmh.map(|kmh| kmh / 1.60934),
        }
    }
}

impl From<&ChargeSessionSummary> for ChargeInsightView {
    fn from(session: &ChargeSessionSummary) -> Self {
        let when = session
            .start_instant
            .clone()
            .or_else(|| session.end_instant.clone())
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

        Self {
            when,
            energy_kwh: session
                .total_energy_kwh
                .map(|v| format!("{v:.1} kWh"))
                .unwrap_or_else(|| "—".into()),
            range_added_miles: session
                .range_added_km
                .map(|km| format!("{:.0} mi", km / 1.60934))
                .unwrap_or_else(|| "—".into()),
            location,
            charger_type,
        }
    }
}

fn format_last_update(ts: Option<DateTime<Utc>>) -> String {
    let Some(ts) = ts else {
        return "never".to_string();
    };
    let local: DateTime<Local> = ts.into();
    let now = Utc::now();
    let elapsed = now.signed_duration_since(ts);
    let rel = if elapsed.num_seconds() < 60 {
        "just now".to_string()
    } else if elapsed.num_minutes() < 60 {
        format!("{}m ago", elapsed.num_minutes())
    } else if elapsed.num_hours() < 24 {
        format!("{}h ago", elapsed.num_hours())
    } else {
        format!("{}d ago", elapsed.num_days())
    };
    format!("{} ({})", local.format("%Y-%m-%d %H:%M:%S"), rel)
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
}
