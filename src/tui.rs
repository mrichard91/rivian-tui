use ratatui::prelude::*;
use ratatui::widgets::*;

use crate::app::{App, LogLevel, LoginField, Mode};

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
fn kv_pair(
    w: usize,
    label: &str,
    l: &(String, Color),
    r: &(String, Color),
) -> Line<'static> {
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
        Constraint::Min(8),   // body
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
    let status_color = if connected {
        Color::Green
    } else {
        Color::Red
    };

    let vehicle_id = app
        .tokens
        .as_ref()
        .map(|t| t.vehicle_id.as_str())
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
        Span::raw(format!(" Vehicle: {vehicle_id}")),
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
        let msg = if app.tokens.is_some() {
            "Fetching vehicle data..."
        } else {
            "Not connected"
        };
        let waiting = Paragraph::new(msg)
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(" Dashboard "),
            );
        frame.render_widget(waiting, area);
        return;
    };

    let cols = Layout::horizontal([
        Constraint::Percentage(33),
        Constraint::Percentage(34),
        Constraint::Percentage(33),
    ])
    .split(area);

    draw_col_battery(frame, cols[0], vs);
    draw_col_vehicle(frame, cols[1], vs);
    draw_col_status(frame, cols[2], vs);
}

/// Left column: battery gauge + charging
fn draw_col_battery(frame: &mut Frame, area: Rect, vs: &crate::api::types::VehicleStateFields) {
    let rows = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(6),
    ])
    .split(area);

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
    let remote = bool_badge(vs.get_boolish(&vs.remote_charging_available), "ready", "off");
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
        kv(CW, "Port", vs.get_str(&vs.charge_port_state), status_color(vs.get_str(&vs.charge_port_state))),
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
fn draw_col_vehicle(frame: &mut Frame, area: Rect, vs: &crate::api::types::VehicleStateFields) {
    const VW: usize = 10;

    let power = vs.power_state_str();
    let power_color = if power == "ready" || power == "go" {
        Color::Green
    } else {
        Color::Gray
    };
    let mileage = vs.mileage().map(|m| format!("{m:.0} mi")).unwrap_or_else(|| "—".into());
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
    let cold_color = if cold_active { Color::Yellow } else { Color::Green };
    let climate_summary = if precon_type != "unknown" {
        format!("{precon} / {precon_type}")
    } else {
        precon.to_string()
    };
    let defrost_color = status_color(defrost);

    let lines = vec![
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
    ];

    let panel = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Vehicle "),
    );
    frame.render_widget(panel, area);
}

/// Right column: doors, tires, OTA, status
fn draw_col_status(frame: &mut Frame, area: Rect, vs: &crate::api::types::VehicleStateFields) {
    let rows = Layout::vertical([
        Constraint::Length(9), // access
        Constraint::Min(11),   // system
    ])
    .split(area);

    const RW: usize = 8;

    // --- Access ---
    let access_icon = |
        closed: &Option<crate::api::types::StateValue>,
        locked: &Option<crate::api::types::StateValue>,
    | -> (String, Color) {
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
    let current = vs.get_str(&vs.ota_current_version);
    let available = vs.get_str(&vs.ota_available_version);
    let ota_status = vs.get_str(&vs.ota_status);
    let install_ready = vs.get_str(&vs.ota_install_ready);
    let progress = vs
        .ota_progress_summary()
        .unwrap_or_else(|| "—".into());
    let location = vs.location_summary().unwrap_or_else(|| "—".into());
    let heading = vs.heading_summary().unwrap_or_else(|| "—".into());
    let last_sync = vs.last_sync().unwrap_or("—");
    let alarm = bool_badge(vs.get_boolish(&vs.alarm_sound_status), "on", "off");
    let guard = vs.get_str(&vs.gear_guard_video_status);
    let guard_color = status_color(guard);
    let service = vs.get_str(&vs.service_mode);
    let wash = vs.get_str(&vs.car_wash_mode);
    let mode_left = tagged_value("Svc", service, status_color(service));
    let mode_right = tagged_value("Wash", wash, status_color(wash));

    let has_update = available != "0.0.0" && available != "unknown" && available != current;
    let avail_color = if has_update { Color::Yellow } else { Color::DarkGray };
    let avail_str = if has_update { available } else { "up to date" };

    let system_lines = vec![
        kvw(RW, "Current", current),
        kv(RW, "Avail", avail_str, avail_color),
        kv(RW, "OTA", ota_status, status_color(ota_status)),
        kvw(RW, "Prog", &progress),
        kvw(RW, "Ready", install_ready),
        kvw(RW, "Sync", last_sync),
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
                .border_style(Style::default().fg(if has_update { Color::Yellow } else { Color::Cyan }))
                .title(if has_update { " System / OTA " } else { " System " }),
        ),
        rows[1],
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
        let empty = Paragraph::new("  No activity yet...")
            .style(Style::default().fg(Color::DarkGray));
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
                Span::styled(
                    format!(" {ts} "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{level_str} "),
                    Style::default().fg(level_color),
                ),
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
                spans.push(Span::styled(
                    " [+]",
                    Style::default().fg(Color::Yellow),
                ));
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

    let footer = Line::from(vec![
        Span::styled(
            keybinds,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ]);

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
    let email_label =
        Paragraph::new("  Email:").style(Style::default().fg(Color::DarkGray));
    let email_input = Paragraph::new(format!("  {}", app.login_email)).style(email_style);
    frame.render_widget(email_label, rows[2]);
    frame.render_widget(email_input, rows[3]);

    // Password
    let pw_style = field_style(app.login_field == LoginField::Password);
    let pw_label =
        Paragraph::new("  Password:").style(Style::default().fg(Color::DarkGray));
    let masked: String = "*".repeat(app.login_password.len());
    let pw_input = Paragraph::new(format!("  {masked}")).style(pw_style);
    frame.render_widget(pw_label, rows[5]);
    frame.render_widget(pw_input, rows[6]);

    // Error or hint
    let msg = if app.login_busy {
        Span::styled("  Logging in...", Style::default().fg(Color::Yellow))
    } else if let Some(err) = &app.login_error {
        Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        )
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

    let otp_label =
        Paragraph::new("  OTP Code:").style(Style::default().fg(Color::DarkGray));
    let otp_input = Paragraph::new(format!("  {}", app.login_otp))
        .style(field_style(true));
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
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
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
