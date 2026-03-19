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

/// Main draw dispatcher
pub fn draw(frame: &mut Frame, app: &App) {
    match app.mode {
        Mode::Dashboard => draw_dashboard(frame, app),
        Mode::Login => draw_login(frame, app),
        Mode::MfaPrompt => draw_mfa(frame, app),
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
        if let Some(entry) = app.activity_log.get(app.log_scroll) {
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
        Constraint::Min(3),
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
    let capacity = vs.battery_capacity_kwh().map(|c| format!("{c:.1} kWh")).unwrap_or_else(|| "—".into());

    let lines = vec![
        kvw(CW, "State", vs.charger_state_str()),
        kvw(CW, "Charger", vs.charger_status_str()),
        kvw(CW, "Time Left", &time_left),
        kvw(CW, "Capacity", &capacity),
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
    let cabin = vs.cabin_temp_f().map(|t| format!("{t:.1} F")).unwrap_or_else(|| "—".into());
    let precon = vs.get_str(&vs.cabin_preconditioning_status);
    let sw_heat = vs.get_str(&vs.steering_wheel_heat);
    let seat_l = vs.get_str(&vs.seat_front_left_heat);
    let seat_r = vs.get_str(&vs.seat_front_right_heat);

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

    let lines = vec![
        kv(VW, "Power", power, power_color),
        kvw(VW, "Gear", vs.gear_str()),
        kvw(VW, "Mode", vs.drive_mode_str()),
        kvw(VW, "Odometer", &mileage),
        kvw(VW, "Cabin", &cabin),
        kvw(VW, "Precon", precon),
        kvw(VW, "Wheel", sw_heat),
        kvw(VW, "Seats", &format!("{seat_l}/{seat_r}")),
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
        Constraint::Length(5),  // doors
        Constraint::Length(4),  // tires
        Constraint::Length(7),  // OTA
        Constraint::Min(3),    // status
    ])
    .split(area);

    const RW: usize = 8;

    // --- Doors ---
    let door_icon = |field: &Option<crate::api::types::StateValue>| -> (String, Color) {
        match field.as_ref().and_then(|v| v.as_str()) {
            Some("closed") => ("Shut".into(), Color::Green),
            Some("open") => ("OPEN".into(), Color::Red),
            Some(other) => (other.into(), Color::Yellow),
            None => ("—".into(), Color::DarkGray),
        }
    };

    let fl = door_icon(&vs.door_front_left_closed);
    let fr = door_icon(&vs.door_front_right_closed);
    let rl = door_icon(&vs.door_rear_left_closed);
    let rr = door_icon(&vs.door_rear_right_closed);
    let frunk = door_icon(&vs.closure_frunk_closed);
    let trunk = door_icon(&vs.closure_liftgate_closed);

    let door_lines = vec![
        kv_pair(RW, "Front", &fl, &fr),
        kv_pair(RW, "Rear", &rl, &rr),
        kv_pair(RW, "Frk/Trk", &frunk, &trunk),
    ];
    frame.render_widget(
        Paragraph::new(door_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Doors "),
        ),
        rows[0],
    );

    // --- Tires ---
    let tire_icon = |field: &Option<crate::api::types::StateValue>| -> (String, Color) {
        match field.as_ref().and_then(|v| v.as_str()) {
            Some("OK") => ("OK".into(), Color::Green),
            Some(s) if s.contains("Low") || s.contains("low") => (s.into(), Color::Red),
            Some(other) => (other.into(), Color::Yellow),
            None => ("—".into(), Color::DarkGray),
        }
    };

    let tfl = tire_icon(&vs.tire_pressure_status_front_left);
    let tfr = tire_icon(&vs.tire_pressure_status_front_right);
    let trl = tire_icon(&vs.tire_pressure_status_rear_left);
    let trr = tire_icon(&vs.tire_pressure_status_rear_right);

    let tire_lines = vec![
        kv_pair(RW, "Front", &tfl, &tfr),
        kv_pair(RW, "Rear", &trl, &trr),
    ];
    frame.render_widget(
        Paragraph::new(tire_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Tires "),
        ),
        rows[1],
    );

    // --- OTA ---
    let current = vs.get_str(&vs.ota_current_version);
    let available = vs.get_str(&vs.ota_available_version);
    let ota_status = vs.get_str(&vs.ota_status);
    let last_result = vs.get_str(&vs.ota_current_status);
    let install_ready = vs.get_str(&vs.ota_install_ready);

    let has_update = available != "0.0.0" && available != "unknown" && available != current;
    let avail_color = if has_update { Color::Yellow } else { Color::DarkGray };
    let avail_str = if has_update { available } else { "up to date" };

    let ota_lines = vec![
        kvw(RW, "Current", current),
        kv(RW, "Avail", avail_str, avail_color),
        kvw(RW, "Status", ota_status),
        kvw(RW, "Last", last_result),
        kvw(RW, "Ready", install_ready),
    ];
    frame.render_widget(
        Paragraph::new(ota_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if has_update { Color::Yellow } else { Color::Cyan }))
                .title(if has_update { " OTA UPDATE! " } else { " OTA " }),
        ),
        rows[2],
    );

    // --- Status ---
    let last_sync = vs.last_sync().unwrap_or("—");

    let status_lines = vec![
        kvw(RW, "Sync", last_sync),
        kvw(RW, "Guard", vs.get_str(&vs.gear_guard_locked)),
        kvw(RW, "Video", vs.get_str(&vs.gear_guard_video_status)),
        kvw(RW, "Pet", vs.get_str(&vs.pet_mode_status)),
        kvw(RW, "Wiper", vs.get_str(&vs.wiper_fluid_state)),
        kvw(RW, "12V", vs.get_str(&vs.twelve_volt_battery_health)),
        kvw(RW, "Trailer", vs.get_str(&vs.trailer_status)),
    ];
    frame.render_widget(
        Paragraph::new(status_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Status "),
        ),
        rows[3],
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

            let is_selected = start + i == app.log_scroll && app.debug;
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
