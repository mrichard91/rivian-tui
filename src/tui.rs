use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::api::types::{KM_PER_MI, METERS_PER_MI};
use crate::app::{App, LogLevel, LoginField, Mode};
use crate::view_model::{
    humanize_iso_local, AlertSeverity, AlertsView, ChargeInsightView, ChargingStatsView,
    LiveChargeHistoryView, LiveChargeView, SoftwareView, TripView, VehicleMetadataView,
};

/// Render a label:value line with the label padded to `w` chars
fn kv(w: usize, label: &str, val: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label:.<w$} ", w = w),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(val.to_string(), Style::default().fg(color)),
    ])
}

/// Shorthand for white value
fn kvw(w: usize, label: &str, val: &str) -> Line<'static> {
    kv(w, label, val, Color::White)
}

/// Render a pair of status values (e.g., door left/right)
fn kv_pair(w: usize, label: &str, l: &(String, Color), r: &(String, Color)) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {label:.<w$} ", w = w),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(l.0.clone(), Style::default().fg(l.1)),
        Span::styled(" / ", Style::default().fg(Color::DarkGray)),
        Span::styled(r.0.clone(), Style::default().fg(r.1)),
    ])
}

fn status_color(value: &str) -> Color {
    let value = value.to_ascii_lowercase();

    if value.contains("open")
        || value.contains("low")
        || value.contains("fail")
        || value.contains("error")
        || value == "true"
    {
        Color::Red
    } else if value.contains("unlock")
        || value.contains("limited")
        || value.contains("download")
        || value.contains("install")
        || value.contains("warning")
    {
        Color::Yellow
    } else if value.contains("closed")
        || value.contains("locked")
        || value == "ok"
        || value.contains("enabled")
        || value.contains("normal")
        || value.contains("success")
        || value.contains("ready")
    {
        Color::Green
    } else if value.contains("unknown")
        || value.contains("signal")
        || value.contains("not_")
        || value.contains("inactive")
        || value.contains("disabled")
        || value.contains("off")
        || value.contains("idle")
        || value == "false"
    {
        Color::DarkGray
    } else {
        Color::White
    }
}

fn bool_badge(flag: Option<bool>, true_label: &str, false_label: &str) -> (String, Color) {
    match flag {
        Some(true) => (true_label.into(), Color::Green),
        Some(false) => (false_label.into(), Color::DarkGray),
        None => ("—".into(), Color::DarkGray),
    }
}

fn tagged_value(tag: &str, value: &str, color: Color) -> (String, Color) {
    (format!("{tag}:{value}"), color)
}

const R1T_ART: [&str; 5] = [
    "      .-~~~~~~~~~~~~-.      ",
    "   .-'                '-.   ",
    "  /   (|)==========(|)   \\  ",
    " |_____|            |_____| ",
    " '----/______________\\----' ",
];

/// Main draw dispatcher
pub fn draw(frame: &mut Frame, app: &App) {
    match app.mode {
        Mode::Dashboard => draw_dashboard(frame, app),
        Mode::Login => draw_login(frame, app),
        Mode::MfaPrompt => draw_mfa(frame, app),
        Mode::VehicleSelect => draw_vehicle_select(frame, app),
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

fn draw_dashboard(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let mut constraints = vec![
        Constraint::Length(3), // header
        Constraint::Min(8),    // body
    ];
    if app.show_log {
        constraints.push(Constraint::Length(if app.debug { 16 } else { 10 }));
    }
    constraints.push(Constraint::Length(1)); // footer

    let outer = Layout::vertical(constraints).split(area);

    let mut idx = 0;
    draw_header(frame, outer[idx], app);
    idx += 1;
    draw_body(frame, outer[idx], app);
    idx += 1;
    if app.show_log {
        draw_activity_log(frame, outer[idx], app);
        idx += 1;
    }
    draw_footer(frame, outer[idx], app);

    // Debug detail overlay
    if app.show_debug_detail {
        if let Some(entry) = app.activity_log.get(app.log_selected) {
            if let Some(detail) = &entry.detail {
                draw_debug_overlay(frame, area, detail);
            }
        }
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let connected = app.tokens.is_some();
    let status_icon = if connected { "●" } else { "○" };
    let status_color = if connected { Color::Green } else { Color::Red };

    let vehicle_label = app
        .vehicle_metadata
        .as_ref()
        .map(|meta| meta.display_name.as_str())
        .or_else(|| app.tokens.as_ref().map(|t| t.vehicle_id.as_str()))
        .unwrap_or("not connected");

    let mut spans = vec![
        Span::styled(
            " RIVIAN ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(status_icon, Style::default().fg(status_color)),
        Span::raw(format!(" Vehicle: {vehicle_label}")),
    ];

    if app.debug {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            " DEBUG ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let title = Line::from(spans);

    let last_update = app
        .last_update
        .map(|t| {
            let local = t.with_timezone(&chrono::Local);
            format!("  Updated: {}", local.format("%H:%M:%S"))
        })
        .unwrap_or_default();

    let header = Paragraph::new(title).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title_bottom(Line::from(last_update).right_aligned()),
    );
    frame.render_widget(header, area);
}

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
    let Some(vs) = &app.vehicle_state else {
        let (title, border_color, lines) = if let Some(error) = &app.vehicle_state_error {
            let summary = error
                .lines()
                .next()
                .unwrap_or("Vehicle-state request failed");
            (
                " Dashboard error ",
                Color::Red,
                vec![
                    Line::from(Span::styled(
                        "Unable to load vehicle data",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    )),
                    Line::from(summary),
                    Line::from(""),
                    Line::from(Span::styled(
                        "Press r to retry or l to open the activity log",
                        Style::default().fg(Color::DarkGray),
                    )),
                ],
            )
        } else if app.tokens.is_some() {
            (
                " Dashboard ",
                Color::DarkGray,
                vec![Line::from("Fetching vehicle data...")],
            )
        } else {
            (
                " Dashboard ",
                Color::DarkGray,
                vec![Line::from("Not connected")],
            )
        };
        let waiting = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(border_color))
                    .title(title),
            );
        frame.render_widget(waiting, area);
        return;
    };

    let alerts = AlertsView::from_state(vs);
    let mut body_constraints = Vec::new();
    if !alerts.items.is_empty() {
        let content_width = area.width.saturating_sub(4).max(1) as usize;
        let content_len = alerts
            .items
            .iter()
            .map(|alert| alert.message.len() + 3)
            .sum::<usize>()
            + alerts.data_age.as_ref().map_or(8, |age| age.len() + 15);
        let wrapped_lines = content_len.div_ceil(content_width).clamp(1, 3);
        body_constraints.push(Constraint::Length(wrapped_lines as u16 + 2));
    }
    body_constraints.push(Constraint::Min(12));
    body_constraints.push(Constraint::Length(9));

    let sections = Layout::vertical(body_constraints).split(area);
    let mut section_idx = 0;

    if !alerts.items.is_empty() {
        draw_alert_strip(frame, sections[section_idx], &alerts);
        section_idx += 1;
    }

    let cols = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
    ])
    .split(sections[section_idx]);

    draw_col_battery(frame, cols[0], vs);
    draw_col_vehicle(frame, cols[1], app, vs);
    draw_col_status(frame, cols[2], app, vs);

    let insights = Layout::horizontal([
        Constraint::Percentage(40),
        Constraint::Percentage(28),
        Constraint::Percentage(32),
    ])
    .split(sections[section_idx + 1]);

    draw_trend_panel(frame, insights[0], app, vs);
    draw_trips_panel(frame, insights[1], app);
    draw_charge_insights(frame, insights[2], app, vs);
}

/// Left column: battery gauge + charging
fn draw_col_battery(frame: &mut Frame, area: Rect, vs: &crate::api::types::VehicleStateFields) {
    let rows = Layout::vertical([Constraint::Length(5), Constraint::Min(6)]).split(area);

    // Battery gauge
    let pct = vs.battery_percent().unwrap_or(0.0);
    let range = vs.range_miles().unwrap_or(0.0);
    let limit = vs.battery_limit_percent().unwrap_or(100.0);

    let gauge_color = if pct > 50.0 {
        Color::Green
    } else if pct > 20.0 {
        Color::Yellow
    } else {
        Color::Red
    };

    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Battery "),
        )
        .gauge_style(Style::default().fg(gauge_color).bg(Color::DarkGray))
        .percent(pct.clamp(0.0, 100.0) as u16)
        .label(format!("{pct:.0}% | {range:.0} mi | Lim {limit:.0}%"));
    frame.render_widget(gauge, rows[0]);

    // Charging
    const CW: usize = 10;
    let time_left = vs.time_to_full().unwrap_or_else(|| "—".into());
    let capacity = vs
        .battery_capacity_kwh()
        .map(|c| format!("{c:.1} kWh"))
        .unwrap_or_else(|| "—".into());
    let remote = bool_badge(
        vs.get_boolish(&vs.remote_charging_available),
        "ready",
        "off",
    );
    let thermal = vs.get_str(&vs.battery_hv_thermal_event);
    let thermal_color = status_color(thermal);
    let derate = vs.get_str(&vs.charger_derate_status);
    let derate_color = if derate.eq_ignore_ascii_case("none") {
        Color::DarkGray
    } else {
        status_color(derate)
    };

    let lines = vec![
        kvw(CW, "State", vs.charger_state_str()),
        kvw(CW, "Charger", vs.charger_status_str()),
        kv(
            CW,
            "Port",
            vs.get_str(&vs.charge_port_state),
            status_color(vs.get_str(&vs.charge_port_state)),
        ),
        kv(CW, "Remote", &remote.0, remote.1),
        kv(CW, "Thermal", thermal, thermal_color),
        kv(CW, "Derate", derate, derate_color),
        kvw(CW, "Time/Cap", &format!("{time_left} / {capacity}")),
    ];

    let charging = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Charging "),
    );
    frame.render_widget(charging, rows[1]);
}

/// Middle column: vehicle info
fn draw_col_vehicle(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    vs: &crate::api::types::VehicleStateFields,
) {
    const VW: usize = 10;
    let show_art = area.width >= 30 && area.height >= 16;
    let sections = if show_art {
        Layout::vertical([Constraint::Length(7), Constraint::Min(8)]).split(area)
    } else {
        Layout::vertical([Constraint::Min(8)]).split(area)
    };

    let power = vs.power_state_str();
    let power_color = if power == "ready" || power == "go" {
        Color::Green
    } else {
        Color::Gray
    };
    let mileage = vs
        .mileage()
        .map(|m| format!("{m:.0} mi"))
        .unwrap_or_else(|| "—".into());
    let cabin = vs
        .cabin_temp_f()
        .map(|t| format!("{t:.1} F"))
        .unwrap_or_else(|| "—".into());
    let driver = vs
        .driver_temp_f()
        .map(|t| format!("{t:.1} F"))
        .unwrap_or_else(|| "—".into());
    let precon = vs.get_str(&vs.cabin_preconditioning_status);
    let precon_type = vs.get_str(&vs.cabin_preconditioning_type);
    let defrost = vs.get_str(&vs.defrost_defog_status);
    let sw_heat = vs.get_str(&vs.steering_wheel_heat);
    let seat_fl = vs.get_str(&vs.seat_front_left_heat);
    let seat_fr = vs.get_str(&vs.seat_front_right_heat);
    let seat_rl = vs.get_str(&vs.seat_rear_left_heat);
    let seat_rr = vs.get_str(&vs.seat_rear_right_heat);
    let vent_fl = vs.get_str(&vs.seat_front_left_vent);
    let vent_fr = vs.get_str(&vs.seat_front_right_vent);

    let accel_cold = vs.get_f64(&vs.limited_accel_cold).unwrap_or(0.0);
    let regen_cold = vs.get_f64(&vs.limited_regen_cold).unwrap_or(0.0);
    let cold_active = accel_cold > 0.0 || regen_cold > 0.0;
    let cold_str = if cold_active {
        format!(
            "Accel:{} Regen:{}",
            if accel_cold > 0.0 { "Ltd" } else { "OK" },
            if regen_cold > 0.0 { "Ltd" } else { "OK" },
        )
    } else {
        "None".into()
    };
    let cold_color = if cold_active {
        Color::Yellow
    } else {
        Color::Green
    };
    let climate_summary = if precon_type != "unknown" {
        format!("{precon} / {precon_type}")
    } else {
        precon.to_string()
    };
    let defrost_color = status_color(defrost);

    let metadata = app.vehicle_metadata.as_ref().map(VehicleMetadataView::from);
    let mut lines = Vec::new();
    if let Some(meta) = &metadata {
        lines.push(kvw(VW, "Name", &meta.display_name));
        lines.push(kvw(VW, "Model", &meta.model));
        if meta.trim != "—" {
            lines.push(kvw(VW, "Trim", &meta.trim));
        }
        if meta.exterior_color != "—" {
            lines.push(kvw(VW, "Color", &meta.exterior_color));
        }
    }

    lines.extend([
        kv(VW, "Power", power, power_color),
        kvw(VW, "Gear", vs.gear_str()),
        kvw(VW, "Mode", vs.drive_mode_str()),
        kvw(VW, "Odometer", &mileage),
        kvw(VW, "Cabin", &format!("{cabin} / Drv {driver}")),
        kvw(VW, "Climate", &climate_summary),
        kv(VW, "Defrost", defrost, defrost_color),
        kvw(VW, "Seats F", &format!("{seat_fl}/{seat_fr}")),
        kvw(VW, "Seats R", &format!("{seat_rl}/{seat_rr}")),
        kvw(VW, "Vent", &format!("{vent_fl}/{vent_fr}")),
        kvw(VW, "Wheel", sw_heat),
        kv(VW, "Cold", &cold_str, cold_color),
    ]);

    if show_art {
        let art: Vec<Line> = R1T_ART
            .iter()
            .map(|line| {
                Line::from(Span::styled(
                    *line,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect();

        frame.render_widget(
            Paragraph::new(art).alignment(Alignment::Center).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(
                        metadata
                            .as_ref()
                            .map(|meta| format!(" {} ", meta.model))
                            .unwrap_or_else(|| " R1T ".into()),
                    ),
            ),
            sections[0],
        );
    }

    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Vehicle "),
    );
    frame.render_widget(panel, sections[sections.len() - 1]);
}

/// Right column: doors, tires, OTA, status
fn draw_col_status(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    vs: &crate::api::types::VehicleStateFields,
) {
    let rows = Layout::vertical([
        Constraint::Length(9), // access
        Constraint::Min(11),   // system
    ])
    .split(area);

    const RW: usize = 8;

    // --- Access ---
    let access_icon = |closed: &Option<crate::api::types::StateValue>,
                       locked: &Option<crate::api::types::StateValue>|
     -> (String, Color) {
        match (
            closed.as_ref().and_then(|v| v.as_str()),
            locked.as_ref().and_then(|v| v.as_str()),
        ) {
            (Some("open"), _) => ("OPEN".into(), Color::Red),
            (Some("closed"), Some("locked")) => ("Shut+Lk".into(), Color::Green),
            (Some("closed"), Some("unlocked")) => ("Shut+Un".into(), Color::Yellow),
            (Some("closed"), _) => ("Shut".into(), Color::Green),
            (Some(state), Some(lock)) => (format!("{state}/{lock}"), Color::Yellow),
            (Some(state), None) => (state.into(), status_color(state)),
            (None, Some(lock)) => (lock.into(), status_color(lock)),
            (None, None) => ("—".into(), Color::DarkGray),
        }
    };

    let window_icon = |field: &Option<crate::api::types::StateValue>| -> (String, Color) {
        match field.as_ref().and_then(|v| v.as_str()) {
            Some("closed") => ("Shut".into(), Color::Green),
            Some("open") => ("OPEN".into(), Color::Red),
            Some(other) => (other.into(), status_color(other)),
            None => ("—".into(), Color::DarkGray),
        }
    };

    let fl = access_icon(&vs.door_front_left_closed, &vs.door_front_left_locked);
    let fr = access_icon(&vs.door_front_right_closed, &vs.door_front_right_locked);
    let rl = access_icon(&vs.door_rear_left_closed, &vs.door_rear_left_locked);
    let rr = access_icon(&vs.door_rear_right_closed, &vs.door_rear_right_locked);
    let frunk = access_icon(&vs.closure_frunk_closed, &vs.closure_frunk_locked);
    let trunk = access_icon(&vs.closure_liftgate_closed, &vs.closure_liftgate_locked);

    let tire_icon = |field: &Option<crate::api::types::StateValue>| -> (String, Color) {
        match field.as_ref().and_then(|v| v.as_str()) {
            Some("OK") => ("OK".into(), Color::Green),
            Some(s) if s.contains("Low") || s.contains("low") => (s.into(), Color::Red),
            Some(other) => (other.into(), Color::Yellow),
            None => ("—".into(), Color::DarkGray),
        }
    };

    let access_lines = vec![
        kv_pair(RW, "Front", &fl, &fr),
        kv_pair(RW, "Rear", &rl, &rr),
        kv_pair(RW, "Frk/Trk", &frunk, &trunk),
        kv_pair(
            RW,
            "Tire F",
            &tire_icon(&vs.tire_pressure_status_front_left),
            &tire_icon(&vs.tire_pressure_status_front_right),
        ),
        kv_pair(
            RW,
            "Tire R",
            &tire_icon(&vs.tire_pressure_status_rear_left),
            &tire_icon(&vs.tire_pressure_status_rear_right),
        ),
        kv_pair(
            RW,
            "Win F",
            &window_icon(&vs.window_front_left_closed),
            &window_icon(&vs.window_front_right_closed),
        ),
        kv_pair(
            RW,
            "Win R",
            &window_icon(&vs.window_rear_left_closed),
            &window_icon(&vs.window_rear_right_closed),
        ),
    ];
    frame.render_widget(
        Paragraph::new(access_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Access "),
        ),
        rows[0],
    );

    // --- System ---
    let sw = SoftwareView::from_state(vs);
    let current = vs.get_str(&vs.ota_current_version);
    let available = vs.get_str(&vs.ota_available_version);
    let ota_status = vs.get_str(&vs.ota_status);
    let install_ready = vs.get_str(&vs.ota_install_ready);
    let progress = vs.ota_progress_summary().unwrap_or_else(|| "—".into());
    let location = vs.location_summary().unwrap_or_else(|| "—".into());
    let heading = vs.heading_summary().unwrap_or_else(|| "—".into());
    let last_sync = vs
        .last_sync()
        .map(humanize_iso_local)
        .unwrap_or_else(|| "—".to_string());
    let alarm = bool_badge(vs.get_boolish(&vs.alarm_sound_status), "on", "off");
    let guard = vs.get_str(&vs.gear_guard_video_status);
    let guard_color = status_color(guard);
    let service = vs.get_str(&vs.service_mode);
    let wash = vs.get_str(&vs.car_wash_mode);
    let mode_left = tagged_value("Svc", service, status_color(service));
    let mode_right = tagged_value("Wash", wash, status_color(wash));

    let has_update = vs.update_available();
    let avail_color = if has_update {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let avail_str = if has_update { available } else { "up to date" };

    let notes = app
        .ota_update_details
        .as_ref()
        .map(|details| {
            if details
                .available
                .as_ref()
                .and_then(|d| d.url.as_ref())
                .is_some()
            {
                ("Avail", Color::Yellow)
            } else if details
                .current
                .as_ref()
                .and_then(|d| d.url.as_ref())
                .is_some()
            {
                ("Current", Color::Green)
            } else {
                ("—", Color::DarkGray)
            }
        })
        .unwrap_or(("—", Color::DarkGray));

    let system_lines = vec![
        kvw(RW, "Current", current),
        kvw(RW, "Build", &sw.current_version_number),
        kv(RW, "Avail", avail_str, avail_color),
        kvw(RW, "AvBuild", &sw.available_version_number),
        kv(RW, "OTA", ota_status, status_color(ota_status)),
        kvw(RW, "Prog", &progress),
        kvw(RW, "Ready", install_ready),
        kv(RW, "Notes", notes.0, notes.1),
        kvw(RW, "Sync", &last_sync),
        kvw(RW, "Loc", &location),
        kvw(RW, "Head", &heading),
        kv_pair(
            RW,
            "Guard/Alm",
            &tagged_value("G", guard, guard_color),
            &tagged_value("A", &alarm.0, alarm.1),
        ),
        kv_pair(RW, "Mode", &mode_left, &mode_right),
    ];
    frame.render_widget(
        Paragraph::new(system_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if has_update {
                    Color::Yellow
                } else {
                    Color::Cyan
                }))
                .title(if has_update {
                    " System / OTA "
                } else {
                    " System "
                }),
        ),
        rows[1],
    );
}

fn alert_color(severity: AlertSeverity) -> Color {
    match severity {
        AlertSeverity::Critical => Color::Red,
        AlertSeverity::Warning => Color::Yellow,
    }
}

fn draw_alert_strip(frame: &mut Frame, area: Rect, alerts: &AlertsView) {
    let mut spans = vec![Span::styled(
        " Alerts ",
        Style::default()
            .fg(Color::Black)
            .bg(Color::Red)
            .add_modifier(Modifier::BOLD),
    )];

    // The cloud serves the last-synced snapshot while the truck sleeps, so
    // these alerts can describe state from a while ago. Say so up front
    // rather than presenting e.g. "Window open" as live.
    if let Some(age) = &alerts.data_age {
        spans.push(Span::styled(
            format!(" as of {age} "),
            Style::default().fg(Color::DarkGray),
        ));
    }

    for (idx, alert) in alerts.items.iter().enumerate() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            alert.message.clone(),
            Style::default().fg(alert_color(alert.severity)),
        ));
        if idx + 1 != alerts.items.len() {
            spans.push(Span::styled(" • ", Style::default().fg(Color::DarkGray)));
        }
    }

    let paragraph = Paragraph::new(Line::from(spans))
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        );
    frame.render_widget(paragraph, area);
}

fn trend_delta(app: &App) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    let first = app.recent_trend.first();
    let last = app.recent_trend.last();

    let battery = first
        .and_then(|point| point.battery_level)
        .zip(last.and_then(|point| point.battery_level))
        .map(|(start, end)| end - start);
    let range = first
        .and_then(|point| point.range_km)
        .zip(last.and_then(|point| point.range_km))
        .map(|(start, end)| (end - start) / KM_PER_MI);
    let mileage = first
        .and_then(|point| point.vehicle_mileage_m)
        .zip(last.and_then(|point| point.vehicle_mileage_m))
        .map(|(start, end)| (end - start) / METERS_PER_MI);
    let peak_speed = app
        .recent_trend
        .iter()
        .filter_map(|point| point.speed_kmh)
        .fold(None, |acc: Option<f64>, speed| {
            Some(acc.map_or(speed, |current| current.max(speed)))
        })
        .map(|kmh| kmh / KM_PER_MI);

    (battery, range, mileage, peak_speed)
}

fn sparkline_data(values: impl Iterator<Item = Option<f64>>) -> Vec<u64> {
    // Drop missing samples rather than rendering them as zero, which would
    // otherwise pull the line to the baseline and misrepresent the trend.
    let values: Vec<f64> = values.flatten().collect();
    if values.is_empty() {
        return Vec::new();
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    if span < f64::EPSILON {
        return vec![1; values.len()];
    }

    values
        .into_iter()
        .map(|value| (((value - min) / span) * 100.0).round() as u64)
        .collect()
}

fn trend_range(values: impl Iterator<Item = Option<f64>>) -> Option<(f64, f64)> {
    values.flatten().fold(None, |acc, value| match acc {
        Some((min, max)) => Some((f64::min(min, value), f64::max(max, value))),
        None => Some((value, value)),
    })
}

fn signed_value(value: Option<f64>, unit: &str, decimals: usize) -> String {
    value
        .map(|v| format!("{v:+.*}{unit}", decimals))
        .unwrap_or_else(|| format!("—{unit}"))
}

fn trend_summary(
    charging: bool,
    battery_delta: Option<f64>,
    range_delta: Option<f64>,
    mileage_delta: Option<f64>,
    speed_mph: f64,
) -> (String, Color) {
    if charging {
        return (
            format!(
                "charging: SOC {}  range {}",
                signed_value(battery_delta, "%", 1),
                signed_value(range_delta, "mi", 0),
            ),
            Color::Green,
        );
    }

    if mileage_delta.unwrap_or_default().abs() >= 0.1 {
        return (
            format!(
                "drove {}  SOC {}  range {}",
                signed_value(mileage_delta, "mi", 1),
                signed_value(battery_delta, "%", 1),
                signed_value(range_delta, "mi", 0),
            ),
            Color::White,
        );
    }

    if speed_mph >= 1.0 {
        return (format!("moving now at {speed_mph:.0} mph"), Color::Yellow);
    }

    (
        format!("parked: SOC {}", signed_value(battery_delta, "%", 1)),
        Color::DarkGray,
    )
}

fn draw_trend_panel(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    vs: &crate::api::types::VehicleStateFields,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Trends ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.recent_trend.len() < 2 {
        frame.render_widget(
            Paragraph::new("  Waiting for more snapshots...")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let (battery_delta, range_delta, mileage_delta, peak_speed) = trend_delta(app);
    let now_speed = vs.speed_mph().unwrap_or_default();
    let (summary, summary_color) = trend_summary(
        vs.is_actively_charging(),
        battery_delta,
        range_delta,
        mileage_delta,
        now_speed,
    );
    frame.render_widget(
        Paragraph::new(format!(
            " 24h / {} samples · {summary}",
            app.recent_trend.len()
        ))
        .style(Style::default().fg(summary_color)),
        rows[0],
    );

    let battery_data = sparkline_data(app.recent_trend.iter().map(|point| point.battery_level));
    let range_data = sparkline_data(app.recent_trend.iter().map(|point| point.range_km));
    let battery_band = trend_range(app.recent_trend.iter().map(|point| point.battery_level))
        .map(|(min, max)| format!("{min:.0}-{max:.0}%"))
        .unwrap_or_else(|| "—".into());
    let range_band = trend_range(
        app.recent_trend
            .iter()
            .map(|point| point.range_km.map(|km| km / KM_PER_MI)),
    )
    .map(|(min, max)| format!("{min:.0}-{max:.0}mi"))
    .unwrap_or_else(|| "—".into());

    frame.render_widget(
        Paragraph::new(format!(
            " SOC   {:>3.0}%  {}  band {battery_band}",
            vs.battery_percent().unwrap_or_default(),
            signed_value(battery_delta, "%", 1),
        ))
        .style(Style::default().fg(Color::Green)),
        rows[1],
    );
    frame.render_widget(
        Sparkline::default()
            .data(&battery_data)
            .style(Style::default().fg(Color::Green)),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(format!(
            " Range {:>3.0}mi  {}  band {range_band}",
            vs.range_miles().unwrap_or_default(),
            signed_value(range_delta, "mi", 0),
        ))
        .style(Style::default().fg(Color::Yellow)),
        rows[3],
    );
    frame.render_widget(
        Sparkline::default()
            .data(&range_data)
            .style(Style::default().fg(Color::Yellow)),
        rows[4],
    );

    let motion = format!(
        " Odo {}  now {:.0}mph  peak {:.0}mph",
        signed_value(mileage_delta, "mi", 1),
        now_speed,
        peak_speed.unwrap_or_default(),
    );
    frame.render_widget(
        Paragraph::new(motion).style(Style::default().fg(Color::DarkGray)),
        rows[5],
    );
}

/// Color the mi/kWh figure on a rough efficiency gradient. Rivian trucks live
/// around 2 mi/kWh, so green/yellow/red are scaled to that reality rather than
/// a sedan's.
fn efficiency_color(mi_per_kwh: f64) -> Color {
    if mi_per_kwh >= 2.5 {
        Color::Green
    } else if mi_per_kwh >= 1.8 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Last few driving trips, newest first: distance, mi/kWh efficiency, and end
/// time, derived from the snapshot history. The metrics lead the line so on a
/// narrow terminal it's the timestamp — not the mi/kWh — that gets truncated
/// at the panel edge.
fn draw_trips_panel(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Trips ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.recent_trips.is_empty() {
        frame.render_widget(
            Paragraph::new("  No trips yet — drive to log one")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let lines: Vec<Line> = app
        .recent_trips
        .iter()
        .map(|trip| {
            let view = TripView::from(trip);
            let eff_color = view
                .efficiency_value
                .map(efficiency_color)
                .unwrap_or(Color::DarkGray);
            let eff = view
                .efficiency_value
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "—".into());

            Line::from(vec![
                Span::styled(
                    format!(" {:>7}", view.distance.replace(' ', "")),
                    Style::default().fg(Color::White),
                ),
                Span::styled(format!(" {eff:<4}"), Style::default().fg(eff_color)),
                Span::styled(
                    format!(" {}", view.when_short),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_charge_insights(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    vs: &crate::api::types::VehicleStateFields,
) {
    const W: usize = 9;
    let active = vs.is_actively_charging();
    let title = if active {
        " Charge Live "
    } else {
        " Charge Summary "
    };
    let border = if active { Color::Green } else { Color::Cyan };

    let rows = if active {
        // Prefer the live-session feed when we have it: it carries real
        // power and per-session energy/efficiency that the vehicle-state
        // poll does not expose. Fall back to vehicle-state-only display
        // before the first live response arrives.
        if let Some(live) = &app.live_charging_session {
            let live = LiveChargeView::from(live);
            let history = app
                .live_charging_history
                .as_ref()
                .map(LiveChargeHistoryView::from);
            let history_line = history
                .as_ref()
                .map(|h| format!("avg {} pk {}", h.average_kw, h.peak_kw))
                .unwrap_or_else(|| "fetching".into());

            vec![
                kv(W, "Power", &live.power_kw, Color::Green),
                kvw(
                    W,
                    "SOC",
                    &format!(
                        "{:.0}% / {:.0} mi",
                        vs.battery_percent().unwrap_or_default(),
                        vs.range_miles().unwrap_or_default()
                    ),
                ),
                kvw(
                    W,
                    "Added",
                    &format!("{} / {}", live.energy_delivered_kwh, live.range_added_miles),
                ),
                kvw(W, "mi/kWh", &live.session_efficiency),
                kvw(W, "Time", &live.time_remaining),
                kvw(W, "History", &history_line),
            ]
        } else {
            let (battery_delta, range_delta, _, _) = trend_delta(app);
            vec![
                kv(
                    W,
                    "State",
                    vs.charger_state_str(),
                    status_color(vs.charger_state_str()),
                ),
                kvw(W, "Status", vs.charger_status_str()),
                kvw(
                    W,
                    "SOC",
                    &format!(
                        "{:.0}% / {:.0} mi",
                        vs.battery_percent().unwrap_or_default(),
                        vs.range_miles().unwrap_or_default()
                    ),
                ),
                kvw(W, "Time", &vs.time_to_full().unwrap_or_else(|| "—".into())),
                kvw(
                    W,
                    "Trend",
                    &format!(
                        "ΔSOC {:+.1}%  ΔMi {:+.1}",
                        battery_delta.unwrap_or_default(),
                        range_delta.unwrap_or_default()
                    ),
                ),
                kvw(
                    W,
                    "Where",
                    &vs.location_summary().unwrap_or_else(|| "—".into()),
                ),
            ]
        }
    } else if let Some(stats) = &app.charging_stats {
        let stats = ChargingStatsView::from(stats);
        let mut rows = vec![
            kvw(W, "Avg", &stats.avg_mi_per_kwh),
            kvw(
                W,
                "Sessions",
                &format!("{} / {}", stats.session_count, stats.total_energy_kwh),
            ),
            kvw(
                W,
                "Best/Wrst",
                &format!("{} / {}", stats.best_mi_per_kwh, stats.worst_mi_per_kwh),
            ),
            kvw(W, "Home", &stats.home_summary),
            kvw(W, "Public", &stats.public_summary),
        ];
        if let Some(session) = &app.last_charge_session {
            let session = ChargeInsightView::from(session);
            // Two rows for the latest session so where it happened isn't lost
            // to the lifetime stats: compact time + efficiency, then site.
            rows.push(kvw(
                W,
                "Last",
                &format!("{} · {}", session.when_short, session.efficiency_mi_per_kwh),
            ));
            rows.push(kvw(
                W,
                "Site",
                &format!("{} ({})", session.location, session.charger_type),
            ));
        }
        rows
    } else if let Some(session) = &app.last_charge_session {
        let session = ChargeInsightView::from(session);
        vec![
            kvw(W, "When", &session.when),
            kvw(W, "Energy", &session.energy_kwh),
            kvw(W, "Range", &session.range_added_miles),
            kvw(W, "mi/kWh", &session.efficiency_mi_per_kwh),
            kvw(W, "Site", &session.location),
            kvw(W, "Type", &session.charger_type),
        ]
    } else {
        vec![
            kvw(W, "Last", "No session data"),
            kvw(W, "Hint", "Fetch after next sync"),
        ]
    };

    frame.render_widget(
        Paragraph::new(rows).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border))
                .title(title),
        ),
        area,
    );
}

// ---------------------------------------------------------------------------
// Activity Log
// ---------------------------------------------------------------------------

fn draw_activity_log(frame: &mut Frame, area: Rect, app: &App) {
    let title = if app.debug {
        " Activity Log (DEBUG) — j/k:scroll  d:detail "
    } else {
        " Activity Log — j/k:scroll "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.activity_log.is_empty() {
        let empty =
            Paragraph::new("  No activity yet...").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(empty, inner);
        return;
    }

    let visible_height = inner.height as usize;
    let start = app.log_scroll;
    let end = (start + visible_height).min(app.activity_log.len());

    let lines: Vec<Line> = app.activity_log[start..end]
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let ts = entry.timestamp.format("%H:%M:%S").to_string();
            let (level_str, level_color) = match entry.level {
                LogLevel::Info => ("INFO ", Color::Cyan),
                LogLevel::Error => ("ERROR", Color::Red),
                LogLevel::Debug => ("DEBUG", Color::Yellow),
            };

            let is_selected = start + i == app.log_selected && app.debug;
            let has_detail = entry.detail.is_some();

            let mut spans = vec![
                Span::styled(format!(" {ts} "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{level_str} "), Style::default().fg(level_color)),
                Span::styled(
                    &entry.message,
                    if is_selected {
                        Style::default().fg(Color::White).bg(Color::DarkGray)
                    } else {
                        Style::default().fg(Color::Gray)
                    },
                ),
            ];

            if has_detail && app.debug {
                spans.push(Span::styled(" [+]", Style::default().fg(Color::Yellow)));
            }

            Line::from(spans)
        })
        .collect();

    let log_widget = Paragraph::new(lines);
    frame.render_widget(log_widget, inner);
}

fn draw_debug_overlay(frame: &mut Frame, area: Rect, detail: &str) {
    let popup = centered_rect_pct(80, 80, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Debug Detail — d:close ");

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line> = detail
        .lines()
        .map(|l| {
            let color = if l.starts_with("---") {
                Color::Yellow
            } else {
                Color::Gray
            };
            Line::from(Span::styled(format!(" {l}"), Style::default().fg(color)))
        })
        .collect();

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keybinds = if app.debug {
        " q:Quit  r:Refresh  l:Log  L:Logout  j/k:Scroll  d:Detail "
    } else {
        " q:Quit  r:Refresh  l:Log  L:Logout "
    };

    let footer = Line::from(vec![Span::styled(
        keybinds,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    )]);

    frame.render_widget(Paragraph::new(footer), area);
}

// ---------------------------------------------------------------------------
// Login screen
// ---------------------------------------------------------------------------

fn draw_login(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let popup = centered_rect(50, 16, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Rivian Login ");

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // spacer
        Constraint::Length(1), // email label
        Constraint::Length(1), // email input
        Constraint::Length(1), // spacer
        Constraint::Length(1), // password label
        Constraint::Length(1), // password input
        Constraint::Length(1), // spacer
        Constraint::Length(1), // error / submit hint
        Constraint::Min(0),
    ])
    .split(inner);

    let title = Paragraph::new("Sign in with your Rivian account")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::White));
    frame.render_widget(title, rows[0]);

    // Email
    let email_style = field_style(app.login_field == LoginField::Email);
    let email_label = Paragraph::new("  Email:").style(Style::default().fg(Color::DarkGray));
    let email_input = Paragraph::new(format!("  {}", app.login_email)).style(email_style);
    frame.render_widget(email_label, rows[2]);
    frame.render_widget(email_input, rows[3]);

    // Password
    let pw_style = field_style(app.login_field == LoginField::Password);
    let pw_label = Paragraph::new("  Password:").style(Style::default().fg(Color::DarkGray));
    let masked: String = "*".repeat(app.login_password.len());
    let pw_input = Paragraph::new(format!("  {masked}")).style(pw_style);
    frame.render_widget(pw_label, rows[5]);
    frame.render_widget(pw_input, rows[6]);

    // Error or hint
    let msg = if app.login_busy {
        Span::styled("  Logging in...", Style::default().fg(Color::Yellow))
    } else if let Some(err) = &app.login_error {
        Span::styled(format!("  {err}"), Style::default().fg(Color::Red))
    } else {
        Span::styled(
            "  Tab:switch field  Enter:submit  Esc:quit",
            Style::default().fg(Color::DarkGray),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(msg)), rows[8]);
}

// ---------------------------------------------------------------------------
// MFA prompt
// ---------------------------------------------------------------------------

fn draw_mfa(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let popup = centered_rect(50, 10, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" MFA Verification ");

    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // spacer
        Constraint::Length(1), // otp label
        Constraint::Length(1), // otp input
        Constraint::Length(1), // spacer
        Constraint::Length(1), // hint
        Constraint::Min(0),
    ])
    .split(inner);

    let title = Paragraph::new("Enter the code sent to your device")
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::White));
    frame.render_widget(title, rows[0]);

    let otp_label = Paragraph::new("  OTP Code:").style(Style::default().fg(Color::DarkGray));
    let otp_input = Paragraph::new(format!("  {}", app.login_otp)).style(field_style(true));
    frame.render_widget(otp_label, rows[2]);
    frame.render_widget(otp_input, rows[3]);

    let msg = if app.login_busy {
        Span::styled("  Verifying...", Style::default().fg(Color::Yellow))
    } else if let Some(err) = &app.login_error {
        Span::styled(format!("  {err}"), Style::default().fg(Color::Red))
    } else {
        Span::styled(
            "  Enter:submit  Esc:back",
            Style::default().fg(Color::DarkGray),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(msg)), rows[5]);
}

fn draw_vehicle_select(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let popup = centered_rect(60, 14, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Select Vehicle ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(inner);

    frame.render_widget(
        Paragraph::new("Choose the vehicle this session should use")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White)),
        rows[0],
    );

    let vehicles: Vec<Line> = app
        .vehicle_options()
        .iter()
        .enumerate()
        .map(|(idx, vehicle)| {
            let selected = idx == app.vehicle_selection_index;
            let label = vehicle.name.as_deref().unwrap_or(vehicle.id.as_str());
            let style = if selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            Line::from(vec![
                Span::styled(
                    if selected { " > " } else { "   " },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(label.to_string(), style),
                Span::styled(
                    format!("  [{}]", vehicle.id),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(vehicles), rows[2]);

    let msg = if let Some(err) = &app.login_error {
        Span::styled(format!("  {err}"), Style::default().fg(Color::Red))
    } else {
        Span::styled(
            "  Up/Down:select  Enter:confirm  Esc:back",
            Style::default().fg(Color::DarkGray),
        )
    };
    frame.render_widget(Paragraph::new(Line::from(msg)), rows[3]);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn field_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Gray)
    }
}

/// Create a centered rect with percentage width and fixed row height
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}

/// Create a centered rect with percentage width and percentage height
fn centered_rect_pct(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}
