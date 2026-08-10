use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

use crate::api::types::{
    ChargingSession, LiveChargingSession, VehicleStateFields, KM_PER_MI, METERS_PER_MI,
};

const DB_NAME: &str = "rivian.db";

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct VehicleTrendPoint {
    pub battery_level: Option<f64>,
    pub range_km: Option<f64>,
    pub vehicle_mileage_m: Option<f64>,
    pub speed_kmh: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChargeSessionSummary {
    pub start_instant: Option<String>,
    pub end_instant: Option<String>,
    pub total_energy_kwh: Option<f64>,
    pub range_added_km: Option<f64>,
    pub vendor: Option<String>,
    pub city: Option<String>,
    pub charger_type: Option<String>,
    pub is_public: Option<bool>,
    pub is_home_charger: Option<bool>,
}

/// A single derived driving trip. Currently reconstructed from stored
/// `vehicle_state` snapshots (odometer + state-of-charge deltas across a
/// contiguous moving segment), but the struct is intentionally source-agnostic
/// so a future Rivian trip/energy-history GraphQL query can populate the same
/// shape without touching the renderers. `energy_kwh` / `efficiency_mi_per_kwh`
/// are `None` when the snapshots lack the SOC or pack-capacity data needed to
/// compute them (or when net SOC rose, e.g. heavy regen downhill).
#[derive(Debug, Clone, Serialize)]
pub struct Trip {
    pub start_ts: Option<String>,
    pub end_ts: Option<String>,
    pub distance_mi: f64,
    pub start_soc: Option<f64>,
    pub end_soc: Option<f64>,
    pub energy_kwh: Option<f64>,
    pub efficiency_mi_per_kwh: Option<f64>,
}

impl Trip {
    /// The timestamp a renderer should display for this trip: the end time,
    /// falling back to the start. Lives on the type so the TUI and web views
    /// can't drift on the fallback policy.
    pub fn when_ts(&self) -> Option<&str> {
        self.end_ts.as_deref().or(self.start_ts.as_deref())
    }
}

/// Raw per-snapshot fields the trip segmenter consumes. Kept separate from the
/// public `Trip` so the segmentation logic is a pure function over plain data
/// and can be unit-tested without a database.
#[derive(Debug, Clone)]
struct TripPoint {
    ts: Option<String>,
    odometer_m: Option<f64>,
    soc: Option<f64>,
    capacity_kwh: Option<f64>,
}

/// Parse a stored snapshot timestamp into Unix seconds. Snapshots use the
/// SQLite `strftime('%Y-%m-%dT%H:%M:%SZ')` format, which is valid RFC3339.
fn ts_secs(ts: &Option<String>) -> Option<i64> {
    ts.as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|dt| dt.timestamp())
}

/// Reconstruct trips from an ascending-by-time slice of snapshots.
///
/// A trip is a maximal run of consecutive snapshots whose odometer advanced
/// faster than a walking-pace floor between samples; a sample with no movement
/// ends the current trip. The movement test is speed-based (odometer delta /
/// time delta) so it holds at any poll cadence; when timestamps are missing or
/// non-increasing it falls back to a fixed distance floor calibrated to the
/// default 5-minute cadence. A gap longer than `MAX_GAP_SECS` between samples
/// also ends the current trip — whatever happened while we weren't sampling
/// (driving, charging) cannot be attributed, and merging across it produces
/// trips with wildly wrong efficiency. Trips shorter than `MIN_TRIP_MI` are
/// dropped as odometer noise. Returned oldest-first.
fn segment_trips(points: &[TripPoint]) -> Vec<Trip> {
    // ~1.1 mph: below this average speed between samples the car is parked.
    const MOVE_SPEED_MPS: f64 = 0.5;
    // Fallback distance floor when timestamps are unusable (≈0.1 mi, the
    // historical threshold for 5-minute samples).
    const MOVE_THRESHOLD_M: f64 = 161.0;
    // Samples further apart than this are different observation sessions —
    // never bridge them into one trip.
    const MAX_GAP_SECS: i64 = 1800;
    // Ignore sub-quarter-mile blips so a creep in a parking lot isn't a trip.
    const MIN_TRIP_MI: f64 = 0.25;
    // Efficiency needs a real amount of energy to be meaningful.
    const MIN_ENERGY_KWH: f64 = 0.2;

    let pts: Vec<&TripPoint> = points.iter().filter(|p| p.odometer_m.is_some()).collect();
    if pts.len() < 2 {
        return Vec::new();
    }

    let make_trip = |start: &TripPoint, end: &TripPoint| -> Option<Trip> {
        let distance_mi = (end.odometer_m? - start.odometer_m?) / METERS_PER_MI;
        if distance_mi < MIN_TRIP_MI {
            return None;
        }

        // Net energy used = SOC drop × usable pack capacity. Prefer the
        // capacity reported at trip end, falling back to the start sample.
        let capacity = end.capacity_kwh.or(start.capacity_kwh);
        let (energy_kwh, efficiency) = match (start.soc, end.soc, capacity) {
            (Some(s0), Some(s1), Some(cap)) if s0 - s1 > 0.0 && cap > 0.0 => {
                let energy = (s0 - s1) / 100.0 * cap;
                if energy >= MIN_ENERGY_KWH {
                    (Some(energy), Some(distance_mi / energy))
                } else {
                    (Some(energy), None)
                }
            }
            _ => (None, None),
        };

        Some(Trip {
            start_ts: start.ts.clone(),
            end_ts: end.ts.clone(),
            distance_mi,
            start_soc: start.soc,
            end_soc: end.soc,
            energy_kwh,
            efficiency_mi_per_kwh: efficiency,
        })
    };

    let mut trips = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    for j in 1..pts.len() {
        let odo_delta = pts[j].odometer_m.unwrap() - pts[j - 1].odometer_m.unwrap();
        let gap_secs = match (ts_secs(&pts[j - 1].ts), ts_secs(&pts[j].ts)) {
            (Some(a), Some(b)) if b > a => Some(b - a),
            _ => None,
        };
        let gap_too_large = gap_secs.is_some_and(|g| g > MAX_GAP_SECS);
        let moved = !gap_too_large
            && match gap_secs {
                Some(g) => odo_delta / g as f64 > MOVE_SPEED_MPS,
                None => odo_delta > MOVE_THRESHOLD_M,
            };

        if moved {
            if start.is_none() {
                start = Some(j - 1);
            }
            end = j;
        } else if let Some(s) = start.take() {
            if let Some(trip) = make_trip(pts[s], pts[end]) {
                trips.push(trip);
            }
        }
    }
    if let Some(s) = start.take() {
        if let Some(trip) = make_trip(pts[s], pts[end]) {
            trips.push(trip);
        }
    }

    trips
}

/// Lifetime / aggregate charging statistics derived from `charging_sessions`.
/// Sessions with implausibly small range or energy are filtered out before
/// the mi/kWh ratios are computed so a single bogus row doesn't dominate
/// "best" / "worst".
#[derive(Debug, Clone, Default, Serialize)]
pub struct ChargingStats {
    pub session_count: i64,
    pub total_energy_kwh: f64,
    pub total_range_km: f64,
    pub avg_mi_per_kwh: Option<f64>,
    pub best_mi_per_kwh: Option<f64>,
    pub worst_mi_per_kwh: Option<f64>,
    pub home_session_count: i64,
    pub home_avg_mi_per_kwh: Option<f64>,
    pub public_session_count: i64,
    pub public_avg_mi_per_kwh: Option<f64>,
}

fn charging_session_dedupe_key(session: &ChargingSession) -> String {
    if let Some(transaction_id) = session.transaction_id.as_deref() {
        if !transaction_id.is_empty() {
            return format!("txn:{transaction_id}");
        }
    }

    format!(
        "fallback:{}|{}|{}|{}|{}|{}|{}|{}",
        session.vehicle_id.as_deref().unwrap_or(""),
        session.start_instant.as_deref().unwrap_or(""),
        session.end_instant.as_deref().unwrap_or(""),
        session.charger_type.as_deref().unwrap_or(""),
        session.vendor.as_deref().unwrap_or(""),
        session.city.as_deref().unwrap_or(""),
        session.total_energy_kwh.unwrap_or_default(),
        session.range_added_km.unwrap_or_default(),
    )
}

/// Canonical schema for the `vehicle_state` data columns (everything except
/// the fixed id/ts/vehicle_id prefix). This single list drives BOTH the
/// CREATE TABLE for new databases AND the migration backfill for existing
/// ones — the previous design kept a separate hand-maintained ALTER list,
/// which drifted: the door/closure "closed" columns were never added to it,
/// so legacy databases rejected every snapshot insert ("no column named
/// door_fl_closed") and silently stopped recording history.
const VEHICLE_STATE_DATA_COLUMNS: &[(&str, &str)] = &[
    // power & drive
    ("power_state", "TEXT"),
    ("drive_mode", "TEXT"),
    ("gear_status", "TEXT"),
    ("vehicle_mileage_m", "REAL"),
    // battery & charging
    ("battery_level", "REAL"),
    ("battery_limit", "REAL"),
    ("battery_capacity", "REAL"),
    ("distance_to_empty_km", "REAL"),
    ("charger_status", "TEXT"),
    ("charger_state", "TEXT"),
    ("time_to_end_of_charge", "REAL"),
    ("charge_port_state", "TEXT"),
    ("charger_derate", "TEXT"),
    ("remote_charging_available", "REAL"),
    ("battery_hv_thermal", "TEXT"),
    // climate
    ("cabin_temp_c", "REAL"),
    ("driver_temp_c", "REAL"),
    ("cabin_preconditioning", "TEXT"),
    ("preconditioning_type", "TEXT"),
    ("defrost_defog", "TEXT"),
    ("seat_heat_fl", "TEXT"),
    ("seat_heat_fr", "TEXT"),
    ("seat_heat_rl", "TEXT"),
    ("seat_heat_rr", "TEXT"),
    ("seat_vent_fl", "TEXT"),
    ("seat_vent_fr", "TEXT"),
    ("steering_wheel_heat", "TEXT"),
    // location
    ("latitude", "REAL"),
    ("longitude", "REAL"),
    ("speed", "REAL"),
    ("altitude", "REAL"),
    ("bearing", "REAL"),
    // connectivity
    ("last_sync", "TEXT"),
    // OTA
    ("ota_current", "TEXT"),
    ("ota_available", "TEXT"),
    ("ota_status", "TEXT"),
    ("ota_current_status", "TEXT"),
    ("ota_download_progress", "REAL"),
    ("ota_install_progress", "REAL"),
    ("ota_install_ready", "TEXT"),
    // doors (closed + locked)
    ("door_fl_closed", "TEXT"),
    ("door_fr_closed", "TEXT"),
    ("door_rl_closed", "TEXT"),
    ("door_rr_closed", "TEXT"),
    ("door_fl_locked", "TEXT"),
    ("door_fr_locked", "TEXT"),
    ("door_rl_locked", "TEXT"),
    ("door_rr_locked", "TEXT"),
    ("frunk_closed", "TEXT"),
    ("frunk_locked", "TEXT"),
    ("liftgate_closed", "TEXT"),
    ("liftgate_locked", "TEXT"),
    ("tailgate_closed", "TEXT"),
    ("tailgate_locked", "TEXT"),
    ("side_bin_l", "TEXT"),
    ("side_bin_r", "TEXT"),
    // windows
    ("window_fl", "TEXT"),
    ("window_fr", "TEXT"),
    ("window_rl", "TEXT"),
    ("window_rr", "TEXT"),
    // tires
    ("tire_fl", "TEXT"),
    ("tire_fr", "TEXT"),
    ("tire_rl", "TEXT"),
    ("tire_rr", "TEXT"),
    // security & misc
    ("pet_mode", "TEXT"),
    ("pet_mode_temp", "TEXT"),
    ("gear_guard", "TEXT"),
    ("gear_guard_video", "TEXT"),
    ("gear_guard_video_mode", "TEXT"),
    ("alarm_status", "TEXT"),
    ("wiper_fluid", "TEXT"),
    ("limited_accel_cold", "REAL"),
    ("limited_regen_cold", "REAL"),
    ("twelve_v_health", "TEXT"),
    ("service_mode", "TEXT"),
    ("trailer_status", "TEXT"),
    ("car_wash_mode", "TEXT"),
];

impl Db {
    pub fn open() -> Result<Self> {
        let path = Self::db_path()?;
        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open database: {}", path.display()))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;

        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn db_path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .context("no config dir")?
            .join("rivian-tui");
        fs::create_dir_all(&dir)?;
        Ok(dir.join(DB_NAME))
    }

    fn migrate(&self) -> Result<()> {
        let vehicle_state_cols = VEHICLE_STATE_DATA_COLUMNS
            .iter()
            .map(|(name, ty)| format!("                {name} {ty}"))
            .collect::<Vec<_>>()
            .join(",\n");

        self.conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS vehicle_state (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                ts              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                vehicle_id      TEXT,
{vehicle_state_cols}
            );

            CREATE INDEX IF NOT EXISTS idx_vs_ts ON vehicle_state(ts);
            CREATE INDEX IF NOT EXISTS idx_vs_vehicle ON vehicle_state(vehicle_id);

            CREATE TABLE IF NOT EXISTS charging_sessions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                fetched_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                transaction_id  TEXT UNIQUE,
                dedupe_key      TEXT,
                vehicle_id      TEXT,
                vehicle_name    TEXT,
                charger_type    TEXT,
                vendor          TEXT,
                city            TEXT,
                start_instant   TEXT,
                end_instant     TEXT,
                total_energy_kwh REAL,
                range_added_km  REAL,
                currency_code   TEXT,
                paid_total      REAL,
                is_home_charger INTEGER,
                is_public       INTEGER,
                is_roaming      INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_cs_start ON charging_sessions(start_instant);
            CREATE INDEX IF NOT EXISTS idx_cs_txn ON charging_sessions(transaction_id);

            CREATE TABLE IF NOT EXISTS live_charging_snapshots (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                ts              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                vehicle_id      TEXT,
                charger_id      TEXT,
                session_start   TEXT,
                vehicle_charger_state TEXT,
                soc             REAL,
                power_kw        REAL,
                current_a       REAL,
                total_energy_kwh REAL,
                range_added_km  REAL,
                kilometers_per_hour REAL,
                time_remaining_min REAL,
                time_elapsed_sec INTEGER,
                current_price   REAL,
                is_rivian_charger INTEGER,
                is_free_session INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_lcs_vehicle_ts
                ON live_charging_snapshots(vehicle_id, ts);
            CREATE INDEX IF NOT EXISTS idx_lcs_session
                ON live_charging_snapshots(vehicle_id, session_start);",
        ))?;

        // Backfill any canonical column missing from an existing database.
        // Introspection (not a hand-maintained list) so a column added to
        // VEHICLE_STATE_DATA_COLUMNS can never be forgotten here.
        let existing: std::collections::HashSet<String> = self
            .conn
            .prepare("SELECT name FROM pragma_table_info('vehicle_state')")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (name, ty) in VEHICLE_STATE_DATA_COLUMNS {
            if !existing.contains(*name) {
                self.conn
                    .execute_batch(&format!("ALTER TABLE vehicle_state ADD COLUMN {name} {ty}"))?;
            }
        }

        if let Err(e) = self
            .conn
            .execute_batch("ALTER TABLE charging_sessions ADD COLUMN dedupe_key TEXT")
        {
            let msg = e.to_string();
            if !msg.contains("duplicate column") {
                return Err(e.into());
            }
        }

        self.conn.execute_batch(
            "UPDATE charging_sessions
             SET dedupe_key = CASE
                 WHEN transaction_id IS NOT NULL AND transaction_id != '' THEN 'txn:' || transaction_id
                 ELSE 'fallback:'
                     || COALESCE(vehicle_id, '') || '|'
                     || COALESCE(start_instant, '') || '|'
                     || COALESCE(end_instant, '') || '|'
                     || COALESCE(charger_type, '') || '|'
                     || COALESCE(vendor, '') || '|'
                     || COALESCE(city, '') || '|'
                     || COALESCE(CAST(total_energy_kwh AS TEXT), '') || '|'
                     || COALESCE(CAST(range_added_km AS TEXT), '')
             END
             WHERE dedupe_key IS NULL OR dedupe_key = '';

             DELETE FROM charging_sessions
             WHERE id NOT IN (
                 SELECT MIN(id)
                 FROM charging_sessions
                 WHERE dedupe_key IS NOT NULL AND dedupe_key != ''
                 GROUP BY dedupe_key
             )
             AND dedupe_key IS NOT NULL
             AND dedupe_key != '';

             CREATE UNIQUE INDEX IF NOT EXISTS idx_cs_dedupe ON charging_sessions(dedupe_key);",
        )?;

        Ok(())
    }

    /// Insert a vehicle state snapshot. Returns the row id.
    pub fn insert_state(&self, vehicle_id: &str, vs: &VehicleStateFields) -> Result<i64> {
        let sv = |f: &Option<crate::api::types::StateValue>| -> Option<String> {
            f.as_ref().map(|v| v.to_display())
        };
        let fv = |f: &Option<crate::api::types::StateValue>| -> Option<f64> {
            f.as_ref().and_then(|v| v.as_f64())
        };

        let (lat, lon) = vs
            .gnss_location
            .as_ref()
            .map(|g| (g.latitude, g.longitude))
            .unwrap_or((None, None));

        let last_sync = vs
            .cloud_connection
            .as_ref()
            .and_then(|c| c.last_sync.clone());

        self.conn.execute(
            "INSERT INTO vehicle_state (
                vehicle_id,
                power_state, drive_mode, gear_status, vehicle_mileage_m,
                battery_level, battery_limit, battery_capacity, distance_to_empty_km,
                charger_status, charger_state, time_to_end_of_charge,
                charge_port_state, charger_derate, remote_charging_available, battery_hv_thermal,
                cabin_temp_c, driver_temp_c, cabin_preconditioning, preconditioning_type, defrost_defog,
                seat_heat_fl, seat_heat_fr, seat_heat_rl, seat_heat_rr,
                seat_vent_fl, seat_vent_fr, steering_wheel_heat,
                latitude, longitude, speed, altitude, bearing, last_sync,
                ota_current, ota_available, ota_status, ota_current_status,
                ota_download_progress, ota_install_progress, ota_install_ready,
                door_fl_closed, door_fr_closed, door_rl_closed, door_rr_closed,
                door_fl_locked, door_fr_locked, door_rl_locked, door_rr_locked,
                frunk_closed, frunk_locked, liftgate_closed, liftgate_locked,
                tailgate_closed, tailgate_locked, side_bin_l, side_bin_r,
                window_fl, window_fr, window_rl, window_rr,
                tire_fl, tire_fr, tire_rl, tire_rr,
                pet_mode, pet_mode_temp, gear_guard, gear_guard_video, gear_guard_video_mode,
                alarm_status, wiper_fluid,
                limited_accel_cold, limited_regen_cold, twelve_v_health,
                service_mode, trailer_status, car_wash_mode
            ) VALUES (
                ?1,
                ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12,
                ?13, ?14, ?15, ?16,
                ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25,
                ?26, ?27, ?28,
                ?29, ?30, ?31, ?32, ?33, ?34,
                ?35, ?36, ?37, ?38,
                ?39, ?40, ?41,
                ?42, ?43, ?44, ?45,
                ?46, ?47, ?48, ?49,
                ?50, ?51, ?52, ?53,
                ?54, ?55, ?56, ?57,
                ?58, ?59, ?60, ?61,
                ?62, ?63, ?64, ?65,
                ?66, ?67, ?68, ?69, ?70,
                ?71, ?72,
                ?73, ?74, ?75,
                ?76, ?77, ?78
            )",
            rusqlite::params![
                vehicle_id,
                sv(&vs.power_state), sv(&vs.drive_mode), sv(&vs.gear_status), fv(&vs.vehicle_mileage),
                fv(&vs.battery_level), fv(&vs.battery_limit), fv(&vs.battery_capacity), fv(&vs.distance_to_empty),
                sv(&vs.charger_status), sv(&vs.charger_state), fv(&vs.time_to_end_of_charge),
                sv(&vs.charge_port_state), sv(&vs.charger_derate_status), fv(&vs.remote_charging_available), sv(&vs.battery_hv_thermal_event),
                fv(&vs.cabin_climate_interior_temperature), fv(&vs.cabin_climate_driver_temperature),
                sv(&vs.cabin_preconditioning_status), sv(&vs.cabin_preconditioning_type), sv(&vs.defrost_defog_status),
                sv(&vs.seat_front_left_heat), sv(&vs.seat_front_right_heat), sv(&vs.seat_rear_left_heat), sv(&vs.seat_rear_right_heat),
                sv(&vs.seat_front_left_vent), sv(&vs.seat_front_right_vent), sv(&vs.steering_wheel_heat),
                lat, lon, fv(&vs.gnss_speed), fv(&vs.gnss_altitude), fv(&vs.gnss_bearing), last_sync,
                sv(&vs.ota_current_version), sv(&vs.ota_available_version), sv(&vs.ota_status), sv(&vs.ota_current_status),
                fv(&vs.ota_download_progress), fv(&vs.ota_install_progress), sv(&vs.ota_install_ready),
                sv(&vs.door_front_left_closed), sv(&vs.door_front_right_closed), sv(&vs.door_rear_left_closed), sv(&vs.door_rear_right_closed),
                sv(&vs.door_front_left_locked), sv(&vs.door_front_right_locked), sv(&vs.door_rear_left_locked), sv(&vs.door_rear_right_locked),
                sv(&vs.closure_frunk_closed), sv(&vs.closure_frunk_locked), sv(&vs.closure_liftgate_closed), sv(&vs.closure_liftgate_locked),
                sv(&vs.closure_tailgate_closed), sv(&vs.closure_tailgate_locked), sv(&vs.closure_side_bin_left_closed), sv(&vs.closure_side_bin_right_closed),
                sv(&vs.window_front_left_closed), sv(&vs.window_front_right_closed), sv(&vs.window_rear_left_closed), sv(&vs.window_rear_right_closed),
                sv(&vs.tire_pressure_status_front_left), sv(&vs.tire_pressure_status_front_right), sv(&vs.tire_pressure_status_rear_left), sv(&vs.tire_pressure_status_rear_right),
                sv(&vs.pet_mode_status), sv(&vs.pet_mode_temperature_status), sv(&vs.gear_guard_locked), sv(&vs.gear_guard_video_status), sv(&vs.gear_guard_video_mode),
                sv(&vs.alarm_sound_status), sv(&vs.wiper_fluid_state),
                fv(&vs.limited_accel_cold), fv(&vs.limited_regen_cold), sv(&vs.twelve_volt_battery_health),
                sv(&vs.service_mode), sv(&vs.trailer_status), sv(&vs.car_wash_mode),
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Get the total number of recorded snapshots
    pub fn snapshot_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM vehicle_state", [], |r| r.get(0))?;
        Ok(count)
    }

    /// Upsert charging sessions (dedup by transaction_id). Returns the new rows.
    ///
    /// `default_vehicle_id` is used to fill in `vehicle_id` when the API
    /// response omits it (older payloads do this). On dedupe hits we also
    /// backfill any existing rows that previously stored NULL, so the
    /// strict-match queries below pick them up after a single refresh.
    pub fn upsert_charging_sessions(
        &self,
        sessions: &[ChargingSession],
        default_vehicle_id: &str,
    ) -> Result<Vec<ChargingSession>> {
        let mut new_sessions = Vec::new();
        for s in sessions {
            let dedupe_key = charging_session_dedupe_key(s);
            let effective_vehicle_id = s
                .vehicle_id
                .as_deref()
                .filter(|v| !v.is_empty())
                .unwrap_or(default_vehicle_id);
            let inserted = self.conn.execute(
                "INSERT OR IGNORE INTO charging_sessions (
                    transaction_id, dedupe_key, vehicle_id, vehicle_name, charger_type, vendor, city,
                    start_instant, end_instant, total_energy_kwh, range_added_km,
                    currency_code, paid_total, is_home_charger, is_public, is_roaming
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                rusqlite::params![
                    s.transaction_id, dedupe_key, effective_vehicle_id, s.vehicle_name,
                    s.charger_type, s.vendor, s.city,
                    s.start_instant, s.end_instant, s.total_energy_kwh, s.range_added_km,
                    s.currency_code, s.paid_total,
                    s.is_home_charger, s.is_public, s.is_roaming_network,
                ],
            )?;
            if inserted > 0 {
                new_sessions.push(s.clone());
            } else {
                // Existing row — opportunistically backfill vehicle_id so
                // legacy rows that were inserted before this change become
                // visible to the strict-match queries.
                self.conn.execute(
                    "UPDATE charging_sessions
                     SET vehicle_id = ?1
                     WHERE dedupe_key = ?2
                       AND (vehicle_id IS NULL OR vehicle_id = '')",
                    rusqlite::params![effective_vehicle_id, dedupe_key],
                )?;
            }
        }
        Ok(new_sessions)
    }

    /// Get total charging session count
    pub fn charging_session_count(&self) -> Result<i64> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM charging_sessions", [], |r| r.get(0))?;
        Ok(count)
    }

    /// Return vehicle trend samples fetched within the last `hours` hours,
    /// ordered oldest to newest. Limited to a hard cap to avoid runaway
    /// result sizes if someone lowers the poll interval dramatically.
    pub fn recent_vehicle_trend(
        &self,
        vehicle_id: &str,
        hours: u32,
    ) -> Result<Vec<VehicleTrendPoint>> {
        const HARD_CAP: i64 = 2000;

        // `hours` is a trusted u32 — safe to interpolate into the datetime
        // modifier. SQLite does not accept bound parameters inside the
        // strftime modifier list.
        let sql = format!(
            "SELECT battery_level, distance_to_empty_km, vehicle_mileage_m, speed
             FROM (
                 SELECT id, battery_level, distance_to_empty_km, vehicle_mileage_m, speed
                 FROM vehicle_state
                 WHERE vehicle_id = ?1
                   AND ts >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-{hours} hours')
                 ORDER BY id DESC
                 LIMIT ?2
             )
             ORDER BY id ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let points: Vec<VehicleTrendPoint> = stmt
            .query_map((vehicle_id, HARD_CAP), |row| {
                Ok(VehicleTrendPoint {
                    battery_level: row.get(0)?,
                    range_km: row.get(1)?,
                    vehicle_mileage_m: row.get(2)?,
                    speed_kmh: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(points)
    }

    /// Reconstruct the most recent driving trips for a vehicle from stored
    /// snapshots, newest-first, capped at `limit`. Scans a bounded window of
    /// recent snapshots so an idle car (few movements) still surfaces several
    /// trips without an unbounded table scan.
    ///
    /// No Rivian GraphQL trip-history query is known to exist in any
    /// open-source client, so this snapshot-derived view is the trip source.
    /// If such a query is ever discovered, it can populate `Trip` directly and
    /// this method becomes one of several sources behind the same type.
    pub fn recent_trips(&self, vehicle_id: &str, limit: usize) -> Result<Vec<Trip>> {
        // Bound the scan to the newest N snapshots to keep segmentation
        // cheap. How much wall-clock history this covers scales with the poll
        // cadence: ~3 weeks at the 5-minute default, ~2 days at the 30s
        // minimum — acceptable, since faster polling implies an attended
        // session where recent trips are the interesting ones.
        const SCAN_CAP: i64 = 6000;

        let mut stmt = self.conn.prepare(
            "SELECT ts, vehicle_mileage_m, battery_level, battery_capacity
             FROM (
                 SELECT id, ts, vehicle_mileage_m, battery_level, battery_capacity
                 FROM vehicle_state
                 WHERE vehicle_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2
             )
             ORDER BY id ASC",
        )?;

        let points: Vec<TripPoint> = stmt
            .query_map((vehicle_id, SCAN_CAP), |row| {
                Ok(TripPoint {
                    ts: row.get(0)?,
                    odometer_m: row.get(1)?,
                    soc: row.get(2)?,
                    capacity_kwh: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut trips = segment_trips(&points);
        // segment_trips returns oldest-first; we want newest-first, capped.
        trips.reverse();
        trips.truncate(limit);
        Ok(trips)
    }

    pub fn latest_charging_session(
        &self,
        vehicle_id: &str,
    ) -> Result<Option<ChargeSessionSummary>> {
        // Same adaptive filter as charging_session_stats: rows inserted
        // before vehicle_id was recorded have NULL/'' and are only backfilled
        // when the API re-returns the same session, so until at least one
        // vehicle-specific row exists, fall back to matching the legacy rows.
        // Keeping the two queries on one policy prevents the Last-charge
        // panel and the stats panel from disagreeing about the same table.
        let vehicle_specific_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM charging_sessions WHERE vehicle_id = ?1",
            [vehicle_id],
            |r| r.get(0),
        )?;
        let vehicle_filter = if vehicle_specific_count > 0 {
            "vehicle_id = ?1"
        } else {
            "vehicle_id = ?1 OR vehicle_id IS NULL OR vehicle_id = ''"
        };

        let sql = format!(
            "SELECT start_instant, end_instant, total_energy_kwh, range_added_km,
                    vendor, city, charger_type, is_public, is_home_charger
             FROM charging_sessions
             WHERE {vehicle_filter}
             ORDER BY COALESCE(end_instant, start_instant, fetched_at) DESC
             LIMIT 1"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let result = stmt.query_row([vehicle_id], |row| {
            Ok(ChargeSessionSummary {
                start_instant: row.get(0)?,
                end_instant: row.get(1)?,
                total_energy_kwh: row.get(2)?,
                range_added_km: row.get(3)?,
                vendor: row.get(4)?,
                city: row.get(5)?,
                charger_type: row.get(6)?,
                is_public: row.get(7)?,
                is_home_charger: row.get(8)?,
            })
        });

        match result {
            Ok(summary) => Ok(Some(summary)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Insert a single live-charging snapshot. Caller decides cadence — we
    /// never deduplicate here because two consecutive samples with the same
    /// values are still useful for time-series plotting.
    pub fn insert_live_charging_snapshot(
        &self,
        vehicle_id: &str,
        snap: &LiveChargingSession,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO live_charging_snapshots (
                vehicle_id, charger_id, session_start, vehicle_charger_state,
                soc, power_kw, current_a, total_energy_kwh, range_added_km,
                kilometers_per_hour, time_remaining_min, time_elapsed_sec,
                current_price, is_rivian_charger, is_free_session
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                vehicle_id,
                snap.charger_id,
                snap.start_time,
                snap.vehicle_charger_state_str(),
                snap.soc_percent(),
                snap.power_kw(),
                snap.current_amps(),
                snap.total_energy_kwh(),
                snap.range_added_km(),
                snap.kilometers_charged_per_hour
                    .as_ref()
                    .and_then(|v| v.as_f64()),
                snap.time_remaining_min(),
                snap.time_elapsed,
                snap.current_price,
                snap.is_rivian_charger.map(|b| b as i64),
                snap.is_free_session.map(|b| b as i64),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Aggregate completed-charging stats for a vehicle. Only sessions with
    /// realistic energy (>0.5 kWh) and range (>0.5 km) values contribute to
    /// the mi/kWh ratios; cancelled or sub-minute sessions otherwise produce
    /// junk efficiency numbers.
    pub fn charging_session_stats(&self, vehicle_id: &str) -> Result<Option<ChargingStats>> {
        let vehicle_specific_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM charging_sessions WHERE vehicle_id = ?1",
            [vehicle_id],
            |r| r.get(0),
        )?;
        let vehicle_filter = if vehicle_specific_count > 0 {
            "vehicle_id = ?1"
        } else {
            "vehicle_id = ?1 OR vehicle_id IS NULL OR vehicle_id = ''"
        };

        // 1. totals across the whole history
        let totals_sql = format!(
            "SELECT
                COUNT(*),
                COALESCE(SUM(total_energy_kwh), 0),
                COALESCE(SUM(range_added_km), 0)
             FROM charging_sessions
             WHERE {vehicle_filter}"
        );
        let (count, total_energy, total_range): (i64, f64, f64) =
            self.conn.query_row(&totals_sql, [vehicle_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;

        if count == 0 {
            return Ok(None);
        }

        // 2. mi/kWh aggregates filtered to realistic sessions
        let efficiency_sql = format!(
            "SELECT
                AVG(range_km / {KM_PER_MI} / energy_kwh),
                MAX(range_km / {KM_PER_MI} / energy_kwh),
                MIN(range_km / {KM_PER_MI} / energy_kwh)
             FROM (
                SELECT total_energy_kwh AS energy_kwh, range_added_km AS range_km
                FROM charging_sessions
                WHERE ({vehicle_filter})
                  AND total_energy_kwh > 0.5
                  AND range_added_km > 0.5
             )"
        );
        let (avg_eff, best_eff, worst_eff): (Option<f64>, Option<f64>, Option<f64>) =
            self.conn.query_row(&efficiency_sql, [vehicle_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;

        // 3. home vs public bucket averages
        let bucket_avg = |home: bool| -> Result<(i64, Option<f64>)> {
            let where_clause = if home {
                "is_home_charger = 1"
            } else {
                "is_home_charger = 0 OR is_home_charger IS NULL"
            };
            let sql = format!(
                "SELECT
                    COUNT(*),
                    AVG(CASE WHEN total_energy_kwh > 0.5 AND range_added_km > 0.5
                              THEN range_added_km / {KM_PER_MI} / total_energy_kwh
                              ELSE NULL END)
                 FROM charging_sessions
                 WHERE ({vehicle_filter})
                   AND ({where_clause})"
            );
            let row: (i64, Option<f64>) = self
                .conn
                .query_row(&sql, [vehicle_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(row)
        };

        let (home_count, home_avg) = bucket_avg(true)?;
        let (public_count, public_avg) = bucket_avg(false)?;

        Ok(Some(ChargingStats {
            session_count: count,
            total_energy_kwh: total_energy,
            total_range_km: total_range,
            avg_mi_per_kwh: avg_eff,
            best_mi_per_kwh: best_eff,
            worst_mi_per_kwh: worst_eff,
            home_session_count: home_count,
            home_avg_mi_per_kwh: home_avg,
            public_session_count: public_count,
            public_avg_mi_per_kwh: public_avg,
        }))
    }

    /// Get a reference to the underlying connection (for future chat/query use)
    #[cfg(test)]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_db() -> Db {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        let db = Db { conn };
        db.migrate().unwrap();
        db
    }

    #[test]
    fn insert_and_count() {
        let db = make_test_db();
        let vs = VehicleStateFields::default();
        db.insert_state("test-vehicle", &vs).unwrap();
        db.insert_state("test-vehicle", &vs).unwrap();
        assert_eq!(db.snapshot_count().unwrap(), 2);
    }

    #[test]
    fn insert_with_data() {
        let db = make_test_db();
        let json = r#"{
            "powerState": { "value": "sleep" },
            "batteryLevel": { "value": 72.0 },
            "vehicleMileage": { "value": 10192690 },
            "distanceToEmpty": { "value": 320 },
            "gnssLocation": { "latitude": 37.77, "longitude": -122.42 },
            "gnssSpeed": { "value": 65 },
            "gnssAltitude": { "value": 100.5 }
        }"#;
        let vs: VehicleStateFields = serde_json::from_str(json).unwrap();
        let id = db.insert_state("VIN123", &vs).unwrap();
        assert!(id > 0);

        let row: (String, f64, f64, Option<f64>) = db
            .conn()
            .query_row(
                "SELECT power_state, battery_level, vehicle_mileage_m, speed FROM vehicle_state WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();

        assert_eq!(row.0, "sleep");
        assert!((row.1 - 72.0).abs() < 0.01);
        assert!((row.2 - 10192690.0).abs() < 0.01);
        assert!((row.3.unwrap() - 65.0).abs() < 0.01);
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = make_test_db();
        db.migrate().unwrap(); // run again — should not error
    }

    #[test]
    fn charging_sessions_without_transaction_id_are_deduped() {
        let db = make_test_db();
        let session = ChargingSession {
            charger_type: Some("home".into()),
            currency_code: Some("USD".into()),
            paid_total: Some(0.0),
            start_instant: Some("2026-03-19T10:00:00Z".into()),
            end_instant: Some("2026-03-19T11:00:00Z".into()),
            total_energy_kwh: Some(22.5),
            range_added_km: Some(120.0),
            city: Some("Irvine".into()),
            transaction_id: None,
            vehicle_id: Some("vehicle-1".into()),
            vehicle_name: Some("R1T".into()),
            vendor: Some("Home".into()),
            is_roaming_network: Some(false),
            is_public: Some(false),
            is_home_charger: Some(true),
            meta: None,
        };

        let inserted = db
            .upsert_charging_sessions(&[session.clone(), session], "vehicle-1")
            .unwrap();

        assert_eq!(inserted.len(), 1);
        assert_eq!(db.charging_session_count().unwrap(), 1);
    }

    #[test]
    fn recent_vehicle_trend_returns_oldest_to_newest() {
        let db = make_test_db();

        for (battery, range, mileage) in [
            (75.0, 320.0, 1000.0),
            (74.0, 315.0, 1010.0),
            (73.5, 312.0, 1020.0),
        ] {
            let json = format!(
                r#"{{
                    "batteryLevel": {{ "value": {battery} }},
                    "distanceToEmpty": {{ "value": {range} }},
                    "vehicleMileage": {{ "value": {mileage} }}
                }}"#
            );
            let vs: VehicleStateFields = serde_json::from_str(&json).unwrap();
            db.insert_state("VIN123", &vs).unwrap();
        }

        let trend = db.recent_vehicle_trend("VIN123", 3).unwrap();
        assert_eq!(trend.len(), 3);
        assert_eq!(trend.first().and_then(|p| p.battery_level), Some(75.0));
        assert_eq!(trend.last().and_then(|p| p.battery_level), Some(73.5));
    }

    #[test]
    fn recent_vehicle_trend_keeps_latest_samples_when_over_cap() {
        let db = make_test_db();

        for idx in 0..2005 {
            let json = format!(
                r#"{{
                    "batteryLevel": {{ "value": {idx} }},
                    "distanceToEmpty": {{ "value": 320 }}
                }}"#
            );
            let vs: VehicleStateFields = serde_json::from_str(&json).unwrap();
            db.insert_state("VIN123", &vs).unwrap();
        }

        let trend = db.recent_vehicle_trend("VIN123", 24).unwrap();
        assert_eq!(trend.len(), 2000);
        assert_eq!(trend.first().and_then(|p| p.battery_level), Some(5.0));
        assert_eq!(trend.last().and_then(|p| p.battery_level), Some(2004.0));
    }

    #[test]
    fn latest_charging_session_returns_most_recent_row() {
        let db = make_test_db();

        let older = ChargingSession {
            charger_type: Some("AC".into()),
            currency_code: Some("USD".into()),
            paid_total: Some(0.0),
            start_instant: Some("2026-03-18T10:00:00Z".into()),
            end_instant: Some("2026-03-18T11:00:00Z".into()),
            total_energy_kwh: Some(12.0),
            range_added_km: Some(55.0),
            city: Some("Austin".into()),
            transaction_id: Some("txn-1".into()),
            vehicle_id: Some("vehicle-1".into()),
            vehicle_name: Some("R1T".into()),
            vendor: Some("Home".into()),
            is_roaming_network: Some(false),
            is_public: Some(false),
            is_home_charger: Some(true),
            meta: None,
        };
        let newer = ChargingSession {
            transaction_id: Some("txn-2".into()),
            end_instant: Some("2026-03-19T11:00:00Z".into()),
            start_instant: Some("2026-03-19T10:00:00Z".into()),
            total_energy_kwh: Some(18.0),
            range_added_km: Some(80.0),
            city: Some("Denver".into()),
            vendor: Some("Rivian".into()),
            charger_type: Some("RAN".into()),
            vehicle_id: Some("vehicle-1".into()),
            vehicle_name: Some("R1T".into()),
            currency_code: Some("USD".into()),
            paid_total: Some(5.0),
            is_roaming_network: Some(false),
            is_public: Some(true),
            is_home_charger: Some(false),
            meta: None,
        };

        db.upsert_charging_sessions(&[older, newer], "vehicle-1")
            .unwrap();

        let latest = db.latest_charging_session("vehicle-1").unwrap().unwrap();
        assert_eq!(latest.vendor.as_deref(), Some("Rivian"));
        assert_eq!(latest.city.as_deref(), Some("Denver"));
    }

    #[test]
    fn live_snapshot_round_trip() {
        let db = make_test_db();

        let json = r#"{
            "chargerId": "RAN-42",
            "startTime": "2026-05-06T10:00:00Z",
            "timeElapsed": 1820,
            "currentPrice": 0.18,
            "isRivianCharger": true,
            "isFreeSession": false,
            "soc": { "value": 64.5, "updatedAt": "2026-05-06T10:30:00Z" },
            "power": { "value": 142.7, "updatedAt": "2026-05-06T10:30:00Z" },
            "current": { "value": 320.0, "updatedAt": "2026-05-06T10:30:00Z" },
            "totalChargedEnergy": { "value": 31.4, "updatedAt": "2026-05-06T10:30:00Z" },
            "rangeAddedThisSession": { "value": 161.0, "updatedAt": "2026-05-06T10:30:00Z" },
            "vehicleChargerState": { "value": "charging_active", "updatedAt": "2026-05-06T10:30:00Z" }
        }"#;
        let snap: LiveChargingSession = serde_json::from_str(json).unwrap();
        let id = db.insert_live_charging_snapshot("VIN-1", &snap).unwrap();
        assert!(id > 0);

        let row: (String, Option<String>, f64, f64, f64) = db
            .conn()
            .query_row(
                "SELECT vehicle_id, charger_id, soc, power_kw, total_energy_kwh
                 FROM live_charging_snapshots WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(row.0, "VIN-1");
        assert_eq!(row.1.as_deref(), Some("RAN-42"));
        assert!((row.2 - 64.5).abs() < 0.01);
        assert!((row.3 - 142.7).abs() < 0.01);
        assert!((row.4 - 31.4).abs() < 0.01);
    }

    #[test]
    fn charging_session_stats_separates_home_and_public() {
        let db = make_test_db();

        // Home: 50 kWh -> 200 km -> 124 mi -> 2.49 mi/kWh
        // Public DC fast: 30 kWh -> 90 km -> 56 mi -> 1.86 mi/kWh
        // Public AC: 20 kWh -> 100 km -> 62 mi -> 3.11 mi/kWh
        let sessions = vec![
            ChargingSession {
                transaction_id: Some("home-1".into()),
                vehicle_id: Some("VIN-1".into()),
                total_energy_kwh: Some(50.0),
                range_added_km: Some(200.0),
                is_home_charger: Some(true),
                charger_type: Some("home".into()),
                ..test_charging_session()
            },
            ChargingSession {
                transaction_id: Some("public-dc-1".into()),
                vehicle_id: Some("VIN-1".into()),
                total_energy_kwh: Some(30.0),
                range_added_km: Some(90.0),
                is_home_charger: Some(false),
                is_public: Some(true),
                charger_type: Some("RAN".into()),
                ..test_charging_session()
            },
            ChargingSession {
                transaction_id: Some("public-ac-1".into()),
                vehicle_id: Some("VIN-1".into()),
                total_energy_kwh: Some(20.0),
                range_added_km: Some(100.0),
                is_home_charger: Some(false),
                is_public: Some(true),
                charger_type: Some("ChargePoint".into()),
                ..test_charging_session()
            },
            // Junk session — should be filtered out of efficiency math.
            ChargingSession {
                transaction_id: Some("junk".into()),
                vehicle_id: Some("VIN-1".into()),
                total_energy_kwh: Some(0.1),
                range_added_km: Some(50.0),
                is_home_charger: Some(false),
                charger_type: Some("Unknown".into()),
                ..test_charging_session()
            },
        ];
        db.upsert_charging_sessions(&sessions, "VIN-1").unwrap();

        let stats = db.charging_session_stats("VIN-1").unwrap().unwrap();
        assert_eq!(stats.session_count, 4);
        assert!((stats.total_energy_kwh - 100.1).abs() < 0.01);
        assert!((stats.total_range_km - 440.0).abs() < 0.01);

        // Best across realistic sessions = public-ac (3.11 mi/kWh).
        let best = stats.best_mi_per_kwh.unwrap();
        assert!((best - 100.0 / 1.60934 / 20.0).abs() < 0.01, "got {best}");
        // Worst = public-dc (1.86 mi/kWh).
        let worst = stats.worst_mi_per_kwh.unwrap();
        assert!((worst - 90.0 / 1.60934 / 30.0).abs() < 0.01);

        assert_eq!(stats.home_session_count, 1);
        assert_eq!(stats.public_session_count, 3);
        let home = stats.home_avg_mi_per_kwh.unwrap();
        assert!((home - 200.0 / 1.60934 / 50.0).abs() < 0.01);
    }

    #[test]
    fn charging_session_stats_returns_none_when_empty() {
        let db = make_test_db();
        assert!(db.charging_session_stats("VIN-1").unwrap().is_none());
    }

    #[test]
    fn charging_session_stats_prefers_vehicle_specific_rows_over_legacy_blanks() {
        let db = make_test_db();

        let sessions = vec![
            ChargingSession {
                transaction_id: Some("vin-1".into()),
                vehicle_id: Some("VIN-1".into()),
                total_energy_kwh: Some(10.0),
                range_added_km: Some(32.0),
                ..test_charging_session()
            },
            ChargingSession {
                transaction_id: Some("legacy".into()),
                vehicle_id: None,
                total_energy_kwh: Some(90.0),
                range_added_km: Some(900.0),
                ..test_charging_session()
            },
        ];
        db.upsert_charging_sessions(&sessions, "").unwrap();

        let stats = db.charging_session_stats("VIN-1").unwrap().unwrap();
        assert_eq!(stats.session_count, 1);
        assert!((stats.total_energy_kwh - 10.0).abs() < 0.01);
        assert!((stats.total_range_km - 32.0).abs() < 0.01);

        let legacy_stats = db.charging_session_stats("VIN-2").unwrap().unwrap();
        assert_eq!(legacy_stats.session_count, 1);
        assert!((legacy_stats.total_energy_kwh - 90.0).abs() < 0.01);
    }

    #[test]
    fn recent_trips_segments_driving_from_parked_periods() {
        let db = make_test_db();

        // Two trips separated by a parked period. Odometer in meters, SOC in %,
        // capacity 135 kWh.
        //   Trip 1: 0 -> 16093 m (10 mi), SOC 80 -> 76 (4% of 135 = 5.4 kWh)
        //           => 10 / 5.4 ≈ 1.85 mi/kWh
        //   parked: odometer flat
        //   Trip 2: 16093 -> 48279 m (+20 mi), SOC 76 -> 68 (8% = 10.8 kWh)
        //           => 20 / 10.8 ≈ 1.85 mi/kWh
        let rows = [
            (0.0, 80.0),     // start trip 1
            (8046.0, 78.0),  // moving
            (16093.0, 76.0), // end trip 1
            (16093.0, 76.0), // parked
            (16093.0, 76.0), // parked
            (24139.0, 73.0), // start trip 2 (movement resumes)
            (48279.0, 68.0), // end trip 2
            (48279.0, 68.0), // parked
        ];
        for (odo, soc) in rows {
            let json = format!(
                r#"{{
                    "vehicleMileage": {{ "value": {odo} }},
                    "batteryLevel": {{ "value": {soc} }},
                    "batteryCapacity": {{ "value": 135.0 }}
                }}"#
            );
            let vs: VehicleStateFields = serde_json::from_str(&json).unwrap();
            db.insert_state("VIN-1", &vs).unwrap();
        }

        let trips = db.recent_trips("VIN-1", 5).unwrap();
        assert_eq!(trips.len(), 2, "expected two distinct trips");

        // Newest-first: trip 2 (20 mi) comes before trip 1 (10 mi).
        let newest = &trips[0];
        assert!((newest.distance_mi - 20.0).abs() < 0.05, "{newest:?}");
        assert!((newest.energy_kwh.unwrap() - 10.8).abs() < 0.05);
        assert!((newest.efficiency_mi_per_kwh.unwrap() - 20.0 / 10.8).abs() < 0.05);

        let oldest = &trips[1];
        assert!((oldest.distance_mi - 10.0).abs() < 0.05);
        assert!((oldest.efficiency_mi_per_kwh.unwrap() - 10.0 / 5.4).abs() < 0.05);
    }

    #[test]
    fn recent_trips_skips_noise_and_handles_missing_energy() {
        let db = make_test_db();

        // A sub-quarter-mile creep (200 m) must not register as a trip.
        // A real trip with no SOC data yields distance but no efficiency.
        let rows: [(f64, Option<f64>); 6] = [
            (0.0, None),    // park
            (200.0, None),  // tiny creep -> noise, below MIN_TRIP_MI
            (200.0, None),  // park again
            (200.0, None),  // last parked sample -> becomes trip start (odo 200 m)
            (3219.0, None), // movement resumes
            (6437.0, None), // trip end (no SOC -> no efficiency)
        ];
        for (odo, soc) in rows {
            let soc_json = match soc {
                Some(v) => format!(r#", "batteryLevel": {{ "value": {v} }}"#),
                None => String::new(),
            };
            let json = format!(r#"{{ "vehicleMileage": {{ "value": {odo} }}{soc_json} }}"#);
            let vs: VehicleStateFields = serde_json::from_str(&json).unwrap();
            db.insert_state("VIN-1", &vs).unwrap();
        }

        let trips = db.recent_trips("VIN-1", 5).unwrap();
        assert_eq!(trips.len(), 1, "noise creep must not be a trip: {trips:?}");
        let trip = &trips[0];
        // Trip spans the last parked sample (200 m) through the end (6437 m).
        assert!((trip.distance_mi - (6437.0 - 200.0) / 1609.344).abs() < 0.05);
        assert!(trip.energy_kwh.is_none());
        assert!(trip.efficiency_mi_per_kwh.is_none());
    }

    #[test]
    fn recent_trips_empty_when_no_snapshots() {
        let db = make_test_db();
        assert!(db.recent_trips("VIN-1", 5).unwrap().is_empty());
    }

    fn trip_point(ts: &str, odometer_m: f64, soc: Option<f64>) -> TripPoint {
        TripPoint {
            ts: Some(ts.into()),
            odometer_m: Some(odometer_m),
            soc,
            capacity_kwh: soc.map(|_| 135.0),
        }
    }

    #[test]
    fn segment_trips_splits_on_large_time_gaps() {
        // App offline for a day while the car drove 120 mi and charged: the
        // two samples bridging the gap must NOT merge into one bogus trip
        // (which would read as e.g. 120 mi on 5% SOC ≈ 18 mi/kWh).
        let points = vec![
            trip_point("2026-06-08T08:00:00Z", 0.0, Some(60.0)),
            trip_point("2026-06-08T08:05:00Z", 8000.0, Some(58.0)),
            trip_point("2026-06-08T08:10:00Z", 16000.0, Some(56.0)),
            // 24h offline gap with 193 km driven and a charge in between.
            trip_point("2026-06-09T08:10:00Z", 209000.0, Some(55.0)),
            trip_point("2026-06-09T08:15:00Z", 209000.0, Some(55.0)),
        ];

        let trips = segment_trips(&points);
        assert_eq!(trips.len(), 1, "gap must not be bridged: {trips:?}");
        // Only the contiguous 16 km morning segment survives.
        assert!((trips[0].distance_mi - 16000.0 / METERS_PER_MI).abs() < 0.05);
    }

    #[test]
    fn segment_trips_movement_test_scales_with_cadence() {
        // 30-second samples crawling at ~8 mph (≈107 m per sample): well under
        // the old fixed 161 m floor, but clearly moving by speed. The whole
        // crawl must come out as one trip, not be dropped as parked.
        let mut points = Vec::new();
        let mut odo = 0.0;
        for i in 0..40 {
            let ts = format!("2026-06-08T08:{:02}:{:02}Z", i / 2, (i % 2) * 30);
            points.push(trip_point(&ts, odo, None));
            odo += 107.0;
        }
        // Then parked.
        points.push(trip_point("2026-06-08T08:25:00Z", odo, None));
        points.push(trip_point("2026-06-08T08:30:00Z", odo, None));

        let trips = segment_trips(&points);
        assert_eq!(trips.len(), 1, "slow crawl must register: {trips:?}");
        assert!((trips[0].distance_mi - 39.0 * 107.0 / METERS_PER_MI).abs() < 0.1);
    }

    #[test]
    fn latest_charging_session_falls_back_to_legacy_rows() {
        let db = make_test_db();

        // A legacy row inserted before vehicle_id was recorded.
        db.conn()
            .execute(
                "INSERT INTO charging_sessions
                     (transaction_id, dedupe_key, vehicle_id, end_instant, total_energy_kwh)
                 VALUES ('legacy-1', 'txn:legacy-1', NULL, '2026-05-01T10:00:00Z', 33.0)",
                [],
            )
            .unwrap();

        let session = db.latest_charging_session("VIN-1").unwrap();
        assert!(
            session.is_some(),
            "legacy NULL-vehicle_id row must stay visible until backfilled"
        );
        assert_eq!(session.unwrap().total_energy_kwh, Some(33.0));

        // Once a vehicle-specific row exists, strict matching takes over.
        db.upsert_charging_sessions(
            &[ChargingSession {
                transaction_id: Some("new-1".into()),
                vehicle_id: Some("VIN-1".into()),
                end_instant: Some("2026-06-01T10:00:00Z".into()),
                total_energy_kwh: Some(20.0),
                ..test_charging_session()
            }],
            "VIN-1",
        )
        .unwrap();
        let session = db.latest_charging_session("VIN-1").unwrap().unwrap();
        assert_eq!(session.total_energy_kwh, Some(20.0));
    }

    fn test_charging_session() -> ChargingSession {
        ChargingSession {
            charger_type: None,
            currency_code: Some("USD".into()),
            paid_total: Some(0.0),
            start_instant: Some("2026-05-01T10:00:00Z".into()),
            end_instant: Some("2026-05-01T11:00:00Z".into()),
            total_energy_kwh: None,
            range_added_km: None,
            city: Some("Seattle".into()),
            transaction_id: None,
            vehicle_id: None,
            vehicle_name: Some("R1T".into()),
            vendor: Some("Rivian".into()),
            is_roaming_network: Some(false),
            is_public: Some(false),
            is_home_charger: Some(false),
            meta: None,
        }
    }

    #[test]
    fn migrate_backfills_door_closed_columns_on_legacy_db() {
        // Reproduces the schema observed in a real pre-upgrade database: the
        // door/closure "closed" columns were in CREATE TABLE but never in the
        // old hand-maintained ALTER list, so legacy DBs lacked them and every
        // insert_state failed — silently halting all snapshot recording
        // (and with it, trip derivation) for months.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE vehicle_state (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                vehicle_id TEXT,
                power_state TEXT,
                drive_mode TEXT,
                gear_status TEXT,
                vehicle_mileage_m REAL,
                battery_level REAL,
                battery_limit REAL,
                battery_capacity REAL,
                charger_status TEXT,
                charger_state TEXT,
                time_to_end_of_charge REAL,
                cabin_temp_c REAL,
                cabin_preconditioning TEXT,
                defrost_defog TEXT,
                seat_heat_fl TEXT,
                seat_heat_fr TEXT,
                steering_wheel_heat TEXT,
                latitude REAL,
                longitude REAL,
                last_sync TEXT,
                ota_current TEXT,
                ota_available TEXT,
                ota_status TEXT,
                window_fl TEXT,
                window_fr TEXT,
                window_rl TEXT,
                window_rr TEXT,
                tire_fl TEXT,
                tire_fr TEXT,
                tire_rl TEXT,
                tire_rr TEXT,
                pet_mode TEXT,
                gear_guard TEXT,
                limited_accel_cold REAL,
                limited_regen_cold REAL,
                twelve_v_health TEXT
            );",
        )
        .unwrap();

        let db = Db { conn };
        db.migrate().unwrap();

        // The legacy DB previously rejected this insert with
        // "no column named door_fl_closed".
        let vs: VehicleStateFields = serde_json::from_str(
            r#"{
                "batteryLevel": { "value": 70.0 },
                "doorFrontLeftClosed": { "value": "closed" },
                "closureFrunkClosed": { "value": "closed" }
            }"#,
        )
        .unwrap();
        let id = db.insert_state("VIN-LEGACY", &vs).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn migrate_adds_distance_to_empty_km_to_legacy_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE vehicle_state (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                vehicle_id TEXT,
                power_state TEXT,
                drive_mode TEXT,
                gear_status TEXT,
                vehicle_mileage_m REAL,
                battery_level REAL,
                battery_limit REAL,
                battery_capacity REAL,
                charger_status TEXT,
                charger_state TEXT,
                time_to_end_of_charge REAL
            );",
        )
        .unwrap();

        let db = Db { conn };
        db.migrate().unwrap();
        assert!(db
            .conn()
            .prepare("SELECT distance_to_empty_km FROM vehicle_state LIMIT 1")
            .is_ok());
    }
}
