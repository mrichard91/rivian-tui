use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::api::types::{ChargingSession, VehicleStateFields};

const DB_NAME: &str = "rivian.db";

pub struct Db {
    conn: Connection,
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
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vehicle_state (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                ts              TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
                vehicle_id      TEXT,

                -- power & drive
                power_state     TEXT,
                drive_mode      TEXT,
                gear_status     TEXT,
                vehicle_mileage_m REAL,

                -- battery & charging
                battery_level   REAL,
                battery_limit   REAL,
                battery_capacity REAL,
                distance_to_empty_km REAL,
                charger_status  TEXT,
                charger_state   TEXT,
                time_to_end_of_charge REAL,
                charge_port_state TEXT,
                charger_derate  TEXT,
                remote_charging_available REAL,
                battery_hv_thermal TEXT,

                -- climate
                cabin_temp_c    REAL,
                driver_temp_c   REAL,
                cabin_preconditioning TEXT,
                preconditioning_type TEXT,
                defrost_defog   TEXT,
                seat_heat_fl    TEXT,
                seat_heat_fr    TEXT,
                seat_heat_rl    TEXT,
                seat_heat_rr    TEXT,
                seat_vent_fl    TEXT,
                seat_vent_fr    TEXT,
                steering_wheel_heat TEXT,

                -- location
                latitude        REAL,
                longitude       REAL,
                speed           REAL,
                altitude        REAL,
                bearing         REAL,

                -- connectivity
                last_sync       TEXT,

                -- OTA
                ota_current     TEXT,
                ota_available   TEXT,
                ota_status      TEXT,
                ota_current_status TEXT,
                ota_download_progress REAL,
                ota_install_progress REAL,
                ota_install_ready TEXT,

                -- doors (closed + locked)
                door_fl_closed  TEXT,
                door_fr_closed  TEXT,
                door_rl_closed  TEXT,
                door_rr_closed  TEXT,
                door_fl_locked  TEXT,
                door_fr_locked  TEXT,
                door_rl_locked  TEXT,
                door_rr_locked  TEXT,
                frunk_closed    TEXT,
                frunk_locked    TEXT,
                liftgate_closed TEXT,
                liftgate_locked TEXT,
                tailgate_closed TEXT,
                tailgate_locked TEXT,
                side_bin_l      TEXT,
                side_bin_r      TEXT,

                -- windows
                window_fl       TEXT,
                window_fr       TEXT,
                window_rl       TEXT,
                window_rr       TEXT,

                -- tires
                tire_fl         TEXT,
                tire_fr         TEXT,
                tire_rl         TEXT,
                tire_rr         TEXT,

                -- security & misc
                pet_mode        TEXT,
                pet_mode_temp   TEXT,
                gear_guard      TEXT,
                gear_guard_video TEXT,
                gear_guard_video_mode TEXT,
                alarm_status    TEXT,
                wiper_fluid     TEXT,
                limited_accel_cold REAL,
                limited_regen_cold REAL,
                twelve_v_health TEXT,
                service_mode    TEXT,
                trailer_status  TEXT,
                car_wash_mode   TEXT
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
            CREATE INDEX IF NOT EXISTS idx_cs_txn ON charging_sessions(transaction_id);",
        )?;

        // Add columns that may not exist in older DBs
        let add_cols = [
            "charge_port_state TEXT", "charger_derate TEXT", "remote_charging_available REAL",
            "battery_hv_thermal TEXT", "driver_temp_c REAL", "preconditioning_type TEXT",
            "seat_heat_rl TEXT", "seat_heat_rr TEXT", "seat_vent_fl TEXT", "seat_vent_fr TEXT",
            "speed REAL", "altitude REAL", "bearing REAL",
            "ota_current_status TEXT", "ota_download_progress REAL",
            "ota_install_progress REAL", "ota_install_ready TEXT",
            "door_fl_locked TEXT", "door_fr_locked TEXT", "door_rl_locked TEXT", "door_rr_locked TEXT",
            "frunk_locked TEXT", "liftgate_locked TEXT", "tailgate_locked TEXT",
            "side_bin_l TEXT", "side_bin_r TEXT",
            "pet_mode_temp TEXT", "gear_guard_video TEXT", "gear_guard_video_mode TEXT",
            "alarm_status TEXT", "wiper_fluid TEXT",
            "service_mode TEXT", "trailer_status TEXT", "car_wash_mode TEXT",
        ];
        for col in &add_cols {
            let sql = format!("ALTER TABLE vehicle_state ADD COLUMN {col}");
            // Ignore "duplicate column" errors
            if let Err(e) = self.conn.execute_batch(&sql) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(e.into());
                }
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

    /// Upsert charging sessions (dedup by transaction_id). Returns count of new rows.
    pub fn upsert_charging_sessions(&self, sessions: &[ChargingSession]) -> Result<usize> {
        let mut new_count = 0;
        for s in sessions {
            let dedupe_key = charging_session_dedupe_key(s);
            let result = self.conn.execute(
                "INSERT OR IGNORE INTO charging_sessions (
                    transaction_id, dedupe_key, vehicle_id, vehicle_name, charger_type, vendor, city,
                    start_instant, end_instant, total_energy_kwh, range_added_km,
                    currency_code, paid_total, is_home_charger, is_public, is_roaming
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                rusqlite::params![
                    s.transaction_id, dedupe_key, s.vehicle_id, s.vehicle_name,
                    s.charger_type, s.vendor, s.city,
                    s.start_instant, s.end_instant, s.total_energy_kwh, s.range_added_km,
                    s.currency_code, s.paid_total,
                    s.is_home_charger, s.is_public, s.is_roaming_network,
                ],
            )?;
            if result > 0 {
                new_count += 1;
            }
        }
        Ok(new_count)
    }

    /// Get total charging session count
    pub fn charging_session_count(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM charging_sessions", [], |r| r.get(0),
        )?;
        Ok(count)
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
        };

        let inserted = db
            .upsert_charging_sessions(&[session.clone(), session])
            .unwrap();

        assert_eq!(inserted, 1);
        assert_eq!(db.charging_session_count().unwrap(), 1);
    }
}
