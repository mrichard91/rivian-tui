//! Structured, display-only fields for cached companion dashboards.
//! No location, account identifiers, credentials, or vehicle commands.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::api::types::{StateValue, TsValue};
use crate::app::DashboardData;
use crate::view_model::{AlertKind, AlertsView};

fn timestamp(value: Option<&str>) -> Option<String> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|v| v.with_timezone(&Utc).to_rfc3339())
}

fn token(field: &Option<StateValue>) -> Option<String> {
    let value = field.as_ref()?.to_display().trim().to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 80
        || matches!(
            value.as_str(),
            "unknown" | "null" | "undefined" | "signal_not_available" | "unavailable"
        )
    {
        None
    } else {
        Some(value.replace([' ', '-'], "_"))
    }
}

pub(crate) fn closure_state(field: &Option<StateValue>) -> &'static str {
    match token(field).as_deref() {
        Some("open" | "ajar" | "false" | "0") => "open",
        Some("closed" | "true" | "1") => "closed",
        _ => "unknown",
    }
}

fn switch_state(field: &Option<StateValue>) -> Option<bool> {
    match token(field).as_deref() {
        Some("true" | "1" | "on" | "enabled" | "active" | "running" | "ready") => Some(true),
        Some("false" | "0" | "off" | "disabled" | "inactive" | "not_ready" | "stopped") => {
            Some(false)
        }
        _ => None,
    }
}

fn number(value: Option<f64>, min: f64, max: f64) -> Option<f64> {
    value.filter(|v| v.is_finite() && (min..=max).contains(v))
}

fn measured(value: &Option<TsValue>, min: f64, max: f64) -> Value {
    json!({
        "value": number(value.as_ref().and_then(TsValue::as_f64), min, max),
        "observed_at": timestamp(value.as_ref().and_then(|v| v.updated_at.as_deref()))
    })
}

pub fn details(data: &DashboardData) -> Option<Value> {
    let vs = data.vehicle_state.as_ref()?;
    let closures: Vec<Value> = [
        (
            "front_left_door",
            "Front-left door",
            &vs.door_front_left_closed,
        ),
        (
            "front_right_door",
            "Front-right door",
            &vs.door_front_right_closed,
        ),
        (
            "rear_left_door",
            "Rear-left door",
            &vs.door_rear_left_closed,
        ),
        (
            "rear_right_door",
            "Rear-right door",
            &vs.door_rear_right_closed,
        ),
        ("frunk", "Frunk", &vs.closure_frunk_closed),
        ("liftgate", "Liftgate", &vs.closure_liftgate_closed),
        ("tailgate", "Tailgate", &vs.closure_tailgate_closed),
        (
            "left_side_bin",
            "Left gear tunnel",
            &vs.closure_side_bin_left_closed,
        ),
        (
            "right_side_bin",
            "Right gear tunnel",
            &vs.closure_side_bin_right_closed,
        ),
        ("tonneau", "Tonneau", &vs.closure_tonneau_closed),
        (
            "front_left_window",
            "Front-left window",
            &vs.window_front_left_closed,
        ),
        (
            "front_right_window",
            "Front-right window",
            &vs.window_front_right_closed,
        ),
        (
            "rear_left_window",
            "Rear-left window",
            &vs.window_rear_left_closed,
        ),
        (
            "rear_right_window",
            "Rear-right window",
            &vs.window_rear_right_closed,
        ),
    ]
    .into_iter()
    .map(|(id, label, field)| json!({"id": id, "label": label, "state": closure_state(field)}))
    .collect();

    let tires: Vec<Value> = [
        ("front_left", "Front left", &vs.tire_pressure_status_front_left, &vs.tire_pressure_status_valid_front_left),
        ("front_right", "Front right", &vs.tire_pressure_status_front_right, &vs.tire_pressure_status_valid_front_right),
        ("rear_left", "Rear left", &vs.tire_pressure_status_rear_left, &vs.tire_pressure_status_valid_rear_left),
        ("rear_right", "Rear right", &vs.tire_pressure_status_rear_right, &vs.tire_pressure_status_valid_rear_right),
    ].into_iter().map(|(id, label, field, validity)| {
        let valid = match token(validity).as_deref() {
            Some("valid" | "true" | "1") => Some(true),
            Some("invalid" | "false" | "0") => Some(false),
            _ => None,
        };
        let status = match token(field).as_deref() {
            Some("low" | "pressure_low" | "under_inflated") => "low",
            Some("normal" | "ok" | "normal_pressure") => "normal",
            Some("high" | "pressure_high" | "over_inflated") => "high",
            _ => "unknown",
        };
        json!({"id":id, "label":label, "status":if valid == Some(false) {"unknown"} else {status}, "sensor_valid":valid})
    }).collect();

    let mut alerts = AlertsView::from_state(vs).items;
    // Replace generic closure alerts with the same explicit states as the checklist.
    alerts.retain(|a| !matches!(a.kind, AlertKind::Closure | AlertKind::Window));
    let mut attention: Vec<Value> = alerts.into_iter().map(|a| json!(a)).collect();
    for closure in &closures {
        if closure["state"] == "open" {
            attention.push(json!({"kind":if closure["id"].as_str().unwrap_or("").ends_with("window") {"window"} else {"closure"}, "severity":"warning", "message":format!("{} open", closure["label"].as_str().unwrap_or("Closure"))}));
        }
    }
    if token(&vs.wiper_fluid_state).as_deref() == Some("empty")
        && !attention.iter().any(|a| a["kind"] == "fluid")
    {
        attention.push(json!({"kind":"fluid", "severity":"warning", "message":"Washer fluid low"}));
    }

    let state = token(&vs.charger_state);
    let status = token(&vs.charger_status);
    let plugged_in = match status.as_deref() {
        Some("chrgr_sts_not_connected" | "not_connected" | "disconnected") => Some(false),
        Some(
            "chrgr_sts_connected"
            | "connected"
            | "chrgr_sts_charging"
            | "chrgr_sts_complete"
            | "chrgr_sts_charge_complete",
        ) => Some(true),
        _ if vs.is_actively_charging() => Some(true),
        _ => None,
    };
    let charging_active = if vs.is_actively_charging() {
        Some(true)
    } else if plugged_in == Some(false)
        || matches!(
            state.as_deref(),
            Some("charging_inactive" | "charging_ready" | "charging_complete")
        )
    {
        Some(false)
    } else {
        None
    };

    let live = data
        .live_charging_session
        .as_ref()
        .filter(|_| charging_active == Some(true))
        .map(|session| {
            let started = timestamp(session.start_time.as_deref());
            let mut points: Vec<(DateTime<Utc>, f64)> = data
                .live_charging_history
                .as_ref()
                .map(|history| {
                    history
                        .chart_data
                        .iter()
                        .filter_map(|p| {
                            let time = DateTime::parse_from_rfc3339(p.time.as_deref()?)
                                .ok()?
                                .with_timezone(&Utc);
                            let kw = number(p.kw, 0.0, 1000.0)?;
                            if let Some(start) = &started {
                                if time
                                    < DateTime::parse_from_rfc3339(start)
                                        .ok()?
                                        .with_timezone(&Utc)
                                {
                                    return None;
                                }
                            } else {
                                return None;
                            }
                            Some((time, kw))
                        })
                        .collect()
                })
                .unwrap_or_default();
            points.sort_by_key(|p| p.0);
            points.dedup_by_key(|p| p.0);
            let stride = points.len().div_ceil(119).max(1);
            let mut sampled: Vec<Value> = points
                .iter()
                .step_by(stride)
                .map(|(time, kw)| json!({"time":time.to_rfc3339(),"kw":kw}))
                .collect();
            if let Some((time, kw)) = points.last() {
                if sampled
                    .last()
                    .is_none_or(|p| p["time"] != time.to_rfc3339())
                {
                    sampled.push(json!({"time":time.to_rfc3339(),"kw":kw}));
                }
            }
            json!({
                "started_at":started,
                "power_kw":measured(&session.power,0.0,1000.0),
                "soc_percent":measured(&session.soc,0.0,100.0),
                "energy_kwh":measured(&session.total_charged_energy,0.0,1000.0),
                "range_added_km":measured(&session.range_added_this_session,0.0,3000.0),
                "remaining_minutes":measured(&session.time_remaining,0.0,10080.0),
                "curve":sampled
            })
        });

    let software = &data.ota_update_details;
    Some(json!({
        "schema":1,
        "observed_at":timestamp(vs.last_sync()),
        "battery_limit_percent":number(vs.battery_limit_percent(),0.0,100.0),
        "charging_active":charging_active, "plugged_in":plugged_in,
        "attention":attention, "closures":closures, "tires":tires,
        "live_charge":live,
        "climate":{
            "cabin_f":number(vs.cabin_temp_f(),-100.0,250.0),
            "target_f":number(vs.driver_temp_f(),32.0,120.0),
            "preconditioning":switch_state(&vs.cabin_preconditioning_status),
            "defrost":switch_state(&vs.defrost_defog_status),
            "pet_mode":switch_state(&vs.pet_mode_status),
            "steering_wheel_heat":switch_state(&vs.steering_wheel_heat),
            "seat_heat":[
                {"label":"Driver", "level":number(vs.get_f64(&vs.seat_front_left_heat),0.0,3.0)},
                {"label":"Passenger", "level":number(vs.get_f64(&vs.seat_front_right_heat),0.0,3.0)},
                {"label":"Rear left", "level":number(vs.get_f64(&vs.seat_rear_left_heat),0.0,3.0)},
                {"label":"Rear right", "level":number(vs.get_f64(&vs.seat_rear_right_heat),0.0,3.0)}
            ]
        },
        "software":{
            "current_version":token(&vs.ota_current_version),
            "available_version":if vs.update_available() { token(&vs.ota_available_version) } else { None },
            "update_available":vs.update_available(),
            "status":token(&vs.ota_status),
            "download_percent":number(vs.get_f64(&vs.ota_download_progress),0.0,100.0),
            "install_percent":number(vs.get_f64(&vs.ota_install_progress),0.0,100.0),
            "install_ready":switch_state(&vs.ota_install_ready),
            "release_notes_url":software.as_ref().and_then(|d| d.available.as_ref().or(d.current.as_ref())).and_then(|d|d.url.as_deref())
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::VehicleStateFields;

    #[test]
    fn unknown_closures_never_become_closed() {
        for v in [
            Value::Null,
            json!("signal_not_available"),
            json!("unfamiliar"),
        ] {
            assert_eq!(closure_state(&Some(StateValue { value: v })), "unknown");
        }
        assert_eq!(
            closure_state(&Some(StateValue {
                value: json!("ajar")
            })),
            "open"
        );
        assert_eq!(
            closure_state(&Some(StateValue { value: json!(true) })),
            "closed"
        );
    }

    #[test]
    fn sleeping_snapshot_keeps_actual_source_time_and_excludes_identifiers() {
        let vs: VehicleStateFields = serde_json::from_value(json!({
            "cloudConnection":{"lastSync":"2026-09-15T01:00:00Z"},
            "doorFrontLeftClosed":{"value":"ajar"},
            "petModeStatus":{"value":"Off"},
            "gnssLocation":{"latitude":42.0,"longitude":-71.0}
        }))
        .unwrap();
        let data = DashboardData {
            vehicle_state: Some(vs),
            vehicle_id: Some("private-vehicle-id".into()),
            last_update: Some(Utc::now()),
            ..Default::default()
        };
        let result = details(&data).unwrap();
        assert_eq!(result["observed_at"], "2026-09-15T01:00:00+00:00");
        assert_eq!(result["closures"][0]["state"], "open");
        assert_eq!(result["climate"]["pet_mode"], false);
        assert!(result["charging_active"].is_null());
        assert!(!result.to_string().contains("private-vehicle-id"));
        assert!(!result.to_string().contains("latitude"));
    }
}
