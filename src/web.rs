//! Mobile-friendly HTTP dashboard. Reads the shared `DashboardData` snapshot
//! populated by the TUI's polling loop and serves it as server-rendered HTML
//! plus a small JSON API.

use std::fmt::Write as _;

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use tokio::net::TcpListener;

use crate::app::{DashboardData, SharedDashboardData};
use crate::config::WebConfig;
use crate::view_model::DashboardView;

/// Bind a TcpListener and return it alongside the resolved address. Bind
/// happens before `serve()` so the caller can log the address (or surface a
/// bind error) before entering the TUI alternate screen.
pub async fn bind(config: &WebConfig) -> Result<(TcpListener, std::net::SocketAddr)> {
    let addr = config.socket_addr()?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind web server on {addr}"))?;
    let local = listener.local_addr().unwrap_or(addr);
    Ok((listener, local))
}

/// Serve the dashboard over the provided listener forever.
pub async fn serve(listener: TcpListener, data: SharedDashboardData) -> Result<()> {
    let app = Router::new()
        .route("/", get(dashboard_html))
        .route("/api/state", get(dashboard_json))
        .route("/healthz", get(healthz))
        .with_state(data);

    axum::serve(listener, app)
        .await
        .context("web server exited with error")
}

async fn healthz() -> &'static str {
    "ok"
}

async fn dashboard_json(State(data): State<SharedDashboardData>) -> Response {
    match current_view(&data) {
        Ok(view) => Json(view).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn dashboard_html(State(data): State<SharedDashboardData>) -> Response {
    match current_view(&data) {
        Ok(view) => {
            let body = render_html(&view);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                Html(body),
            )
                .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

fn current_view(data: &SharedDashboardData) -> Result<DashboardView, String> {
    let snapshot: DashboardData = data
        .read()
        .map_err(|_| "dashboard state lock poisoned".to_string())?
        .clone();
    Ok(DashboardView::from_data(&snapshot))
}

fn render_html(view: &DashboardView) -> String {
    let mut body = String::with_capacity(8 * 1024);

    // Header
    body.push_str(HTML_HEAD);

    let vehicle_id = view
        .vehicle_id
        .as_deref()
        .unwrap_or("no vehicle")
        .to_string();
    let _ = write!(
        body,
        r#"<header class="topbar">
  <div class="brand">rivian-tui</div>
  <div class="vehicle">{vehicle}</div>
  <div class="updated">{updated}</div>
  <a class="refresh" href="/">Refresh</a>
</header>
"#,
        vehicle = escape(&vehicle_id),
        updated = escape(&view.last_update_human),
    );

    if !view.has_data {
        body.push_str(
            r#"<main><section class="card"><h2>Waiting for data…</h2>
<p>The TUI hasn't received a vehicle-state response yet. This page will auto-refresh.</p>
</section></main>"#,
        );
        body.push_str(HTML_TAIL);
        return body;
    }

    body.push_str("<main class=\"grid\">");

    // Battery card
    let battery_pct = view.battery.percent_value.unwrap_or(0.0).clamp(0.0, 100.0);
    let battery_badge = if view.battery.is_charging {
        r#"<span class="badge active">⚡ charging</span>"#
    } else {
        ""
    };
    let bar_class = if view.battery.is_charging {
        "bar-fill charging"
    } else {
        "bar-fill"
    };
    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Battery {badge}</h2>
  <div class="big">{pct}</div>
  <div class="bar"><div class="{bar_class}" style="width:{pct_v:.0}%"></div></div>
  <div class="charging-line">{charge_line}</div>
  <dl>
    <dt>Range</dt><dd>{range}</dd>
    <dt>Time to full</dt><dd>{ttf}</dd>
    <dt>Limit</dt><dd>{limit}</dd>
    <dt>Capacity</dt><dd>{cap}</dd>
  </dl>
</section>
"#,
        badge = battery_badge,
        pct = escape(&view.battery.percent),
        bar_class = bar_class,
        pct_v = battery_pct,
        charge_line = escape(&view.battery.charging_label),
        range = escape(&view.battery.range_miles),
        ttf = escape(&view.battery.time_to_full),
        limit = escape(&view.battery.limit_percent),
        cap = escape(&view.battery.capacity_kwh),
    );

    // Charging card
    let charging_badge = if view.charging.is_active {
        r#"<span class="badge active">charging</span>"#
    } else {
        r#"<span class="badge idle">idle</span>"#
    };
    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Charging {badge}</h2>
  <dl>
    <dt>State</dt><dd>{state}</dd>
    <dt>Status</dt><dd>{status}</dd>
    <dt>Time to full</dt><dd>{ttf}</dd>
    <dt>Port</dt><dd>{port}</dd>
  </dl>
</section>
"#,
        badge = charging_badge,
        state = escape(&view.charging.state),
        status = escape(&view.charging.status),
        ttf = escape(&view.charging.time_to_full),
        port = escape(&view.charging.port_state),
    );

    // Climate card
    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Climate</h2>
  <div class="big">{cabin}</div>
  <dl>
    <dt>Driver set</dt><dd>{driver}</dd>
    <dt>Preconditioning</dt><dd>{pre}</dd>
    <dt>Defrost</dt><dd>{def}</dd>
  </dl>
</section>
"#,
        cabin = escape(&view.climate.cabin_temp_f),
        driver = escape(&view.climate.driver_set_temp_f),
        pre = escape(&view.climate.preconditioning),
        def = escape(&view.climate.defrost),
    );

    // Vehicle card
    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Vehicle</h2>
  <dl>
    <dt>Power</dt><dd>{power}</dd>
    <dt>Gear</dt><dd>{gear}</dd>
    <dt>Drive mode</dt><dd>{mode}</dd>
    <dt>Odometer</dt><dd>{odo}</dd>
    <dt>Speed</dt><dd>{speed}</dd>
    <dt>Doors</dt><dd>{doors}</dd>
    <dt>Closures</dt><dd>{closures}</dd>
  </dl>
</section>
"#,
        power = escape(&view.vehicle.power_state),
        gear = escape(&view.vehicle.gear),
        mode = escape(&view.vehicle.drive_mode),
        odo = escape(&view.vehicle.mileage),
        speed = escape(&view.vehicle.speed_mph),
        doors = escape(&view.vehicle.doors_locked),
        closures = escape(&view.vehicle.all_closed),
    );

    // Software card
    let sw = &view.software;
    let sw_badge = if sw.update_available {
        r#"<span class="badge warn">update available</span>"#
    } else {
        r#"<span class="badge idle">up to date</span>"#
    };

    let mut sw_rows = String::new();
    if sw.update_available {
        let _ = write!(
            sw_rows,
            "<dt>Available</dt><dd>{available} <span class=\"muted\">{date}</span></dd>",
            available = escape(&sw.available_version),
            date = escape(&sw.available_version_date),
        );
    }
    let _ = write!(
        sw_rows,
        "<dt>Status</dt><dd>{}</dd>",
        escape(&sw.status)
    );
    let _ = write!(
        sw_rows,
        "<dt>Install type</dt><dd>{}</dd>",
        escape(&sw.install_type)
    );
    let _ = write!(
        sw_rows,
        "<dt>Progress</dt><dd>{}</dd>",
        escape(&sw.progress_summary)
    );

    // Only surface the install-detail rows when there's actually a pending
    // install — otherwise they're all placeholders.
    let installing = sw.install_progress != "—" || sw.install_ready != "—";
    if installing {
        let _ = write!(
            sw_rows,
            "<dt>Install ready</dt><dd>{}</dd>",
            escape(&sw.install_ready)
        );
        let _ = write!(
            sw_rows,
            "<dt>Download</dt><dd>{}</dd>",
            escape(&sw.download_progress)
        );
        let _ = write!(
            sw_rows,
            "<dt>Install</dt><dd>{}</dd>",
            escape(&sw.install_progress)
        );
        let _ = write!(
            sw_rows,
            "<dt>Duration</dt><dd>{}</dd>",
            escape(&sw.install_duration)
        );
    }

    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Software {badge}</h2>
  <div class="big version">{current}</div>
  <div class="muted">{current_date}</div>
  <dl>
{rows}
  </dl>
</section>
"#,
        badge = sw_badge,
        current = escape(&sw.current_version),
        current_date = escape(&sw.current_version_date),
        rows = sw_rows,
    );

    // Location card
    let _ = write!(
        body,
        r#"<section class="card">
  <h2>Location</h2>
  <dl>
    <dt>Coords</dt><dd>{coords}</dd>
    <dt>Heading</dt><dd>{heading}</dd>
    <dt>Altitude</dt><dd>{alt}</dd>
    <dt>Last sync</dt><dd>{sync}</dd>
  </dl>
</section>
"#,
        coords = escape(&view.location.coordinates),
        heading = escape(&view.location.heading),
        alt = escape(&view.location.altitude_ft),
        sync = escape(&view.location.last_sync),
    );

    // Live charge card — only appears while a session is in progress.
    if let Some(live) = &view.live_charge {
        let _ = write!(
            body,
            r#"<section class="card">
  <h2>Live charge <span class="badge active">in progress</span></h2>
  <div class="big">{power}</div>
  <div class="muted">{charger} · {state}</div>
  <dl>
    <dt>SOC</dt><dd>{soc}</dd>
    <dt>Energy added</dt><dd>{energy}</dd>
    <dt>Range added</dt><dd>{range}</dd>
    <dt>Session mi/kWh</dt><dd>{eff}</dd>
    <dt>Time remaining</dt><dd>{rem}</dd>
    <dt>Started</dt><dd>{started}</dd>
  </dl>
</section>
"#,
            power = escape(&live.power_kw),
            charger = escape(&live.charger_id),
            state = escape(&live.charger_state),
            soc = escape(&live.soc_percent),
            energy = escape(&live.energy_delivered_kwh),
            range = escape(&live.range_added_miles),
            eff = escape(&live.session_efficiency),
            rem = escape(&live.time_remaining),
            started = escape(&live.started),
        );
    }

    // Charging stats card
    if let Some(stats) = &view.charging_stats {
        let _ = write!(
            body,
            r#"<section class="card">
  <h2>Charging stats</h2>
  <div class="big">{avg}</div>
  <div class="muted">lifetime average · {count} sessions</div>
  <dl>
    <dt>Total energy</dt><dd>{energy}</dd>
    <dt>Total range</dt><dd>{range}</dd>
    <dt>Best</dt><dd>{best}</dd>
    <dt>Worst</dt><dd>{worst}</dd>
    <dt>Home</dt><dd>{home}</dd>
    <dt>Public</dt><dd>{public}</dd>
  </dl>
</section>
"#,
            avg = escape(&stats.avg_mi_per_kwh),
            count = escape(&stats.session_count),
            energy = escape(&stats.total_energy_kwh),
            range = escape(&stats.total_range_miles),
            best = escape(&stats.best_mi_per_kwh),
            worst = escape(&stats.worst_mi_per_kwh),
            home = escape(&stats.home_summary),
            public = escape(&stats.public_summary),
        );
    }

    // Last charge card
    if let Some(ch) = &view.last_charge {
        let _ = write!(
            body,
            r#"<section class="card">
  <h2>Last charge</h2>
  <dl>
    <dt>When</dt><dd>{when}</dd>
    <dt>Energy</dt><dd>{energy}</dd>
    <dt>Range added</dt><dd>{range}</dd>
    <dt>Efficiency</dt><dd>{eff}</dd>
    <dt>Location</dt><dd>{loc}</dd>
    <dt>Charger</dt><dd>{kind}</dd>
  </dl>
</section>
"#,
            when = escape(&ch.when),
            energy = escape(&ch.energy_kwh),
            range = escape(&ch.range_added_miles),
            eff = escape(&ch.efficiency_mi_per_kwh),
            loc = escape(&ch.location),
            kind = escape(&ch.charger_type),
        );
    }

    // Trend card
    if !view.trend.is_empty() {
        body.push_str(r#"<section class="card wide"><h2>24h trend</h2>"#);
        body.push_str(&render_trend_svg(&view.trend));
        body.push_str("</section>\n");
    }

    body.push_str("</main>");
    body.push_str(HTML_TAIL);
    body
}

fn render_trend_svg(points: &[crate::view_model::TrendPointView]) -> String {
    let width = 600.0;
    let height = 140.0;
    let pad = 8.0;

    let battery: Vec<f64> = points.iter().filter_map(|p| p.battery_percent).collect();
    if battery.len() < 2 {
        return r#"<p class="muted">Not enough data yet.</p>"#.to_string();
    }

    // Battery axis is fixed 0–100 so the line stays comparable across sessions.
    let battery_path = build_path(&battery, 0.0, 100.0, width, height, pad);
    let observed_min = battery.iter().copied().fold(f64::INFINITY, f64::min);
    let observed_max = battery.iter().copied().fold(f64::NEG_INFINITY, f64::max);

    // Range trend uses an autoscaled axis so the curve shape is visible even
    // when the absolute range bouncs around inside a small band. We render
    // it on the same chart in a different color so the two metrics can be
    // compared at a glance.
    let range: Vec<f64> = points.iter().filter_map(|p| p.range_miles).collect();
    let range_path = if range.len() >= 2 {
        let r_min = range.iter().copied().fold(f64::INFINITY, f64::min);
        let r_max = range.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (lo, hi) = if (r_max - r_min).abs() < 1.0 {
            // Avoid a flat span collapsing to zero height.
            (r_min - 1.0, r_max + 1.0)
        } else {
            (r_min, r_max)
        };
        Some((build_path(&range, lo, hi, width, height, pad), r_min, r_max))
    } else {
        None
    };

    let mut svg = String::new();
    let _ = write!(
        svg,
        r##"<svg viewBox="0 0 {w} {h}" preserveAspectRatio="none" class="trend">
  <line x1="{pad}" y1="{pad}" x2="{xr}" y2="{pad}" stroke="#1f2a36" stroke-width="1"/>
  <line x1="{pad}" y1="{ymid:.1}" x2="{xr}" y2="{ymid:.1}" stroke="#1f2a36" stroke-width="1" stroke-dasharray="2,3"/>
  <line x1="{pad}" y1="{yb}" x2="{xr}" y2="{yb}" stroke="#1f2a36" stroke-width="1"/>
  <path d="{battery_path}" fill="none" stroke="#4ade80" stroke-width="2"/>"##,
        w = width,
        h = height,
        pad = pad,
        xr = width - pad,
        ymid = pad + (height - pad * 2.0) * 0.5,
        yb = height - pad,
    );

    if let Some((ref path, _, _)) = range_path {
        let _ = write!(
            svg,
            r##"
  <path d="{path}" fill="none" stroke="#facc15" stroke-width="1.5" stroke-dasharray="3,2" opacity="0.85"/>"##,
        );
    }
    svg.push_str("\n</svg>\n");

    let range_legend = match range_path {
        Some((_, lo, hi)) => format!(
            r#"<span class="range-swatch">range {lo:.0}–{hi:.0} mi</span>"#,
            lo = lo,
            hi = hi,
        ),
        None => String::new(),
    };

    format!(
        r#"{svg}<div class="trend-legend"><span>0%</span><span>SOC {observed_min:.0}–{observed_max:.0}%</span><span>100%</span>{range_legend}</div>"#,
    )
}

fn build_path(values: &[f64], min: f64, max: f64, width: f64, height: f64, pad: f64) -> String {
    let range = (max - min).max(f64::EPSILON);
    let step = (width - pad * 2.0) / (values.len() as f64 - 1.0).max(1.0);
    let mut path = String::from("M");
    for (i, value) in values.iter().enumerate() {
        let x = pad + step * i as f64;
        let y = pad + (height - pad * 2.0) * (1.0 - (value - min) / range);
        if i == 0 {
            let _ = write!(path, "{x:.1},{y:.1}");
        } else {
            let _ = write!(path, " L{x:.1},{y:.1}");
        }
    }
    path
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

const HTML_HEAD: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="refresh" content="30">
<title>rivian-tui dashboard</title>
<style>
  :root {
    --bg: #0b0f14;
    --card: #131922;
    --border: #1f2a36;
    --fg: #e6edf3;
    --muted: #8b95a1;
    --accent: #4ade80;
    --warn: #facc15;
  }
  * { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; background: var(--bg); color: var(--fg);
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
    font-size: 16px; }
  a { color: var(--accent); text-decoration: none; }
  .topbar {
    display: flex; align-items: center; gap: 12px;
    padding: 12px 16px; background: var(--card);
    border-bottom: 1px solid var(--border); flex-wrap: wrap;
  }
  .brand { font-weight: 700; letter-spacing: 0.5px; }
  .vehicle { color: var(--muted); font-family: ui-monospace, SFMono-Regular, monospace; font-size: 13px; }
  .updated { color: var(--muted); font-size: 13px; margin-left: auto; }
  .refresh { padding: 6px 12px; border: 1px solid var(--border); border-radius: 6px; }
  main { padding: 12px; }
  .grid { display: grid; gap: 12px; grid-template-columns: 1fr; }
  @media (min-width: 640px) { .grid { grid-template-columns: 1fr 1fr; } }
  @media (min-width: 1000px) { .grid { grid-template-columns: 1fr 1fr 1fr; } }
  .card {
    background: var(--card); border: 1px solid var(--border); border-radius: 10px;
    padding: 14px 16px;
  }
  .card.wide { grid-column: 1 / -1; }
  .card h2 { margin: 0 0 8px 0; font-size: 14px; text-transform: uppercase;
    letter-spacing: 0.08em; color: var(--muted); display: flex; align-items: center; gap: 8px; }
  .big { font-size: 32px; font-weight: 700; margin: 4px 0 10px 0; }
  dl { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; }
  dt { color: var(--muted); font-size: 13px; }
  dd { margin: 0; font-size: 14px; text-align: right; font-variant-numeric: tabular-nums; }
  .bar { height: 10px; background: #1f2a36; border-radius: 5px; overflow: hidden; margin-bottom: 8px; }
  .bar-fill { height: 100%; background: var(--accent); transition: width 400ms ease; }
  .bar-fill.charging {
    background: linear-gradient(90deg, var(--accent) 0%, #86efac 50%, var(--accent) 100%);
    background-size: 200% 100%;
    animation: charging-slide 1.6s linear infinite;
  }
  @keyframes charging-slide {
    0%   { background-position: 200% 0; }
    100% { background-position: 0 0; }
  }
  .charging-line { color: var(--accent); font-size: 13px; margin-bottom: 8px; min-height: 16px; }
  .version { font-size: 22px; font-family: ui-monospace, SFMono-Regular, monospace; }
  .badge { font-size: 11px; padding: 2px 8px; border-radius: 999px; text-transform: uppercase;
    letter-spacing: 0.05em; font-weight: 600; }
  .badge.active { background: rgba(74,222,128,0.15); color: var(--accent); }
  .badge.warn { background: rgba(250,204,21,0.15); color: var(--warn); }
  .badge.idle { background: #1f2a36; color: var(--muted); }
  .muted { color: var(--muted); font-size: 12px; }
  .trend { width: 100%; height: 140px; display: block; }
  .trend-legend { display: flex; justify-content: space-between; gap: 12px; color: var(--muted);
    font-size: 12px; margin-top: 4px; flex-wrap: wrap; }
  .trend-legend .range-swatch { color: var(--warn); }
</style>
</head>
<body>
"#;

const HTML_TAIL: &str = r#"
</body>
</html>
"#;
