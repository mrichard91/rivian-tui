use serde::{Deserialize, Serialize};

/// Authentication tokens stored in the OS keychain
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub user_session_token: String,
    pub csrf_token: String,
    pub app_session_token: String,
    pub vehicle_id: String,
    /// Stable per-install client id sent in the `Dc-Cid` header. Optional
    /// so older saved-token payloads still deserialize; backfilled to a
    /// fresh UUID on the next `load_tokens`.
    #[serde(default)]
    pub device_id: Option<String>,
}

/// Temporary MFA state during login
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaState {
    pub email: String,
    pub csrf_token: String,
    pub app_session_token: String,
    pub otp_token: String,
    /// Client id chosen at the start of the login flow; carried through the
    /// OTP exchange so every request in this login uses the same `Dc-Cid`.
    #[serde(default)]
    pub device_id: String,
    pub timestamp: i64,
}

// --- GraphQL response types ---

#[derive(Debug, Deserialize)]
pub struct GraphQlResponse<T> {
    pub data: Option<T>,
    pub errors: Option<Vec<GraphQlError>>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    pub message: String,
    pub extensions: Option<serde_json::Value>,
}

impl GraphQlError {
    fn code(&self) -> Option<&str> {
        self.extensions
            .as_ref()
            .and_then(|ext| ext.get("code"))
            .and_then(|code| code.as_str())
    }

    /// The gateway's "your session is no longer valid" signal.
    pub fn is_unauthenticated(&self) -> bool {
        self.code() == Some("UNAUTHENTICATED")
    }

    pub fn display_message(&self) -> String {
        match self.code() {
            Some(code) => format!("{} ({code})", self.message),
            None => self.message.clone(),
        }
    }
}

// --- CSRF ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsrfData {
    pub create_csrf_token: CsrfToken,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CsrfToken {
    pub csrf_token: String,
    pub app_session_token: String,
}

// --- Login ---

#[derive(Debug, Deserialize)]
pub struct LoginData {
    pub login: LoginResult,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    #[serde(rename = "__typename")]
    pub typename: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub user_session_token: Option<String>,
    #[serde(default)]
    pub otp_token: Option<String>,
}

// --- OTP Login ---

#[derive(Debug, Deserialize)]
pub struct OtpLoginData {
    #[serde(rename = "loginWithOTP")]
    pub login_with_otp: OtpLoginResult,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtpLoginResult {
    pub access_token: String,
    pub refresh_token: String,
    pub user_session_token: String,
}

// --- Vehicles ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfoData {
    pub current_user: CurrentUser,
}

#[derive(Debug, Deserialize)]
pub struct CurrentUser {
    pub vehicles: Vec<Vehicle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Vehicle {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub vin: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub vehicle: Option<VehicleDetails>,
    #[serde(default)]
    pub settings: Option<UserVehicleSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VehicleDetails {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub vin: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub model_year: Option<i64>,
    #[serde(default)]
    pub make: Option<String>,
    #[serde(default)]
    pub mobile_configuration: Option<MobileConfiguration>,
    #[serde(default)]
    pub vehicle_state: Option<VehicleStateSummary>,
    #[serde(default)]
    pub ota_early_access_status: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MobileConfiguration {
    #[serde(default)]
    pub trim_option: Option<ConfigOption>,
    #[serde(default)]
    pub exterior_color_option: Option<ConfigOption>,
    #[serde(default)]
    pub interior_color_option: Option<ConfigOption>,
    #[serde(default)]
    pub drive_system_option: Option<ConfigOption>,
    #[serde(default)]
    pub tonneau_option: Option<ConfigOption>,
    #[serde(default)]
    pub wheel_option: Option<ConfigOption>,
    #[serde(default)]
    pub charge_port: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOption {
    #[serde(default)]
    pub option_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VehicleStateSummary {
    #[serde(default)]
    pub supported_features: Vec<SupportedFeature>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SupportedFeature {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserVehicleSettings {
    #[serde(default)]
    pub name: Option<SettingsValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SettingsValue {
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VehicleMetadata {
    pub id: String,
    pub display_name: String,
    pub vin: Option<String>,
    pub model: Option<String>,
    pub model_year: Option<i64>,
    pub trim: Option<String>,
    pub exterior_color: Option<String>,
    pub interior_color: Option<String>,
    pub drive_system: Option<String>,
    pub wheel: Option<String>,
    pub charge_port: Option<String>,
    pub roles: Vec<String>,
    pub state: Option<String>,
    pub supported_features: Vec<SupportedFeature>,
    pub ota_early_access_status: Option<bool>,
}

impl Vehicle {
    pub fn display_name(&self) -> String {
        self.settings
            .as_ref()
            .and_then(|settings| settings.name.as_ref())
            .and_then(|name| name.value.as_deref())
            .filter(|name| !name.is_empty())
            .or(self.name.as_deref().filter(|name| !name.is_empty()))
            .or_else(|| {
                self.vehicle
                    .as_ref()
                    .and_then(|vehicle| vehicle.model.as_deref())
                    .filter(|model| !model.is_empty())
            })
            .unwrap_or(&self.id)
            .to_string()
    }

    pub fn metadata(&self) -> VehicleMetadata {
        let details = self.vehicle.as_ref();
        let config = details.and_then(|vehicle| vehicle.mobile_configuration.as_ref());
        let option = |field: Option<&ConfigOption>| {
            field
                .and_then(|option| option.option_name.clone())
                .filter(|value| !value.is_empty())
        };

        VehicleMetadata {
            id: self.id.clone(),
            display_name: self.display_name(),
            vin: details
                .and_then(|vehicle| vehicle.vin.clone())
                .or_else(|| self.vin.clone()),
            model: details.and_then(|vehicle| vehicle.model.clone()),
            model_year: details.and_then(|vehicle| vehicle.model_year),
            trim: option(config.and_then(|config| config.trim_option.as_ref())),
            exterior_color: option(config.and_then(|config| config.exterior_color_option.as_ref())),
            interior_color: option(config.and_then(|config| config.interior_color_option.as_ref())),
            drive_system: option(config.and_then(|config| config.drive_system_option.as_ref())),
            wheel: option(config.and_then(|config| config.wheel_option.as_ref())),
            charge_port: config.and_then(|config| config.charge_port.clone()),
            roles: self.roles.clone(),
            state: self.state.clone(),
            supported_features: details
                .and_then(|vehicle| vehicle.vehicle_state.as_ref())
                .map(|state| state.supported_features.clone())
                .unwrap_or_default(),
            ota_early_access_status: details.and_then(|vehicle| vehicle.ota_early_access_status),
        }
    }
}

// --- Charging Sessions ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingSessionsData {
    pub get_completed_session_summaries: Vec<ChargingSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChargingSession {
    pub charger_type: Option<String>,
    pub currency_code: Option<String>,
    pub paid_total: Option<f64>,
    pub start_instant: Option<String>,
    pub end_instant: Option<String>,
    pub total_energy_kwh: Option<f64>,
    pub range_added_km: Option<f64>,
    pub city: Option<String>,
    pub transaction_id: Option<String>,
    pub vehicle_id: Option<String>,
    pub vehicle_name: Option<String>,
    pub vendor: Option<String>,
    pub is_roaming_network: Option<bool>,
    pub is_public: Option<bool>,
    pub is_home_charger: Option<bool>,
    #[serde(default)]
    pub meta: Option<ChargingSessionMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChargingSessionMeta {
    #[serde(default)]
    pub transaction_id_grouping_key: Option<String>,
    #[serde(default)]
    pub data_sources: Vec<String>,
}

// --- Live Charging Session ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionData {
    pub get_live_session_data: Option<LiveChargingSession>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionHistoryData {
    pub get_live_session_history: Option<LiveSessionHistory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LiveSessionHistory {
    #[serde(default)]
    pub chart_data: Vec<LiveSessionHistoryPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveSessionHistoryPoint {
    #[serde(default)]
    pub kw: Option<f64>,
    #[serde(default)]
    pub time: Option<String>,
}

/// A single live charging session record. Fields with the `TsValue` shape
/// come back from the server as `{ value, updatedAt }` records; plain
/// scalars come back inline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LiveChargingSession {
    // Plain scalars
    pub charger_id: Option<String>,
    pub start_time: Option<String>,
    pub time_elapsed: Option<i64>,
    pub location_id: Option<String>,
    pub current_price: Option<f64>,
    pub current_currency: Option<String>,
    pub is_rivian_charger: Option<bool>,
    pub is_free_session: Option<bool>,

    // TimeStamped value records
    pub soc: Option<TsValue>,
    pub power: Option<TsValue>,
    pub current: Option<TsValue>,
    pub current_miles: Option<TsValue>,
    pub kilometers_charged_per_hour: Option<TsValue>,
    pub range_added_this_session: Option<TsValue>,
    pub time_remaining: Option<TsValue>,
    pub total_charged_energy: Option<TsValue>,
    pub vehicle_charger_state: Option<TsValue>,
}

/// Wire shape for Rivian's `TimeStamped*` scalars: `{ value, updatedAt }`.
/// The `value` is intentionally `serde_json::Value` because the API mixes
/// numeric and string representations across fields — same trick we use for
/// `StateValue` on the vehicle-state side.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TsValue {
    #[serde(default)]
    pub value: serde_json::Value,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl TsValue {
    pub fn as_f64(&self) -> Option<f64> {
        match &self.value {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }
}

impl LiveChargingSession {
    pub fn power_kw(&self) -> Option<f64> {
        self.power.as_ref().and_then(|v| v.as_f64())
    }

    pub fn soc_percent(&self) -> Option<f64> {
        self.soc.as_ref().and_then(|v| v.as_f64())
    }

    /// Total energy delivered this session, in kWh.
    pub fn total_energy_kwh(&self) -> Option<f64> {
        self.total_charged_energy.as_ref().and_then(|v| v.as_f64())
    }

    /// Range added this session, in km.
    pub fn range_added_km(&self) -> Option<f64> {
        self.range_added_this_session
            .as_ref()
            .and_then(|v| v.as_f64())
    }

    pub fn current_amps(&self) -> Option<f64> {
        self.current.as_ref().and_then(|v| v.as_f64())
    }

    pub fn time_remaining_min(&self) -> Option<f64> {
        self.time_remaining.as_ref().and_then(|v| v.as_f64())
    }

    pub fn vehicle_charger_state_str(&self) -> Option<&str> {
        self.vehicle_charger_state.as_ref().and_then(|v| v.as_str())
    }

    pub fn range_added_miles(&self) -> Option<f64> {
        self.range_added_km().map(|km| km / KM_PER_MI)
    }

    /// mi/kWh observed so far in this session. Returns None until both range
    /// and energy have crossed a small noise floor — early in a session
    /// either denominator is near zero and the ratio explodes.
    pub fn efficiency_mi_per_kwh(&self) -> Option<f64> {
        let mi = self.range_added_miles()?;
        let kwh = self.total_energy_kwh()?;
        if kwh > 0.5 && mi > 0.5 {
            Some(mi / kwh)
        } else {
            None
        }
    }
}

// --- Vehicle State ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VehicleStateData {
    pub vehicle_state: Option<VehicleStateFields>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtaDetailsData {
    #[serde(rename = "getVehicle")]
    pub get_vehicle: Option<OtaVehicleDetails>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OtaVehicleDetails {
    #[serde(default)]
    pub available_ota_update_details: Option<OtaUpdateDetail>,
    #[serde(default)]
    pub current_ota_update_details: Option<OtaUpdateDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OtaUpdateDetails {
    pub available: Option<OtaUpdateDetail>,
    pub current: Option<OtaUpdateDetail>,
}

impl From<OtaVehicleDetails> for OtaUpdateDetails {
    fn from(details: OtaVehicleDetails) -> Self {
        Self {
            available: details.available_ota_update_details,
            current: details.current_ota_update_details,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OtaUpdateDetail {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VehicleStateFields {
    // Power & drive
    pub power_state: Option<StateValue>,
    pub drive_mode: Option<StateValue>,
    pub gear_status: Option<StateValue>,
    pub vehicle_mileage: Option<StateValue>,

    // Battery & charging
    pub battery_level: Option<StateValue>,
    pub battery_limit: Option<StateValue>,
    pub battery_capacity: Option<StateValue>,
    pub distance_to_empty: Option<StateValue>,
    pub charger_status: Option<StateValue>,
    pub charger_state: Option<StateValue>,
    pub time_to_end_of_charge: Option<StateValue>,
    pub charge_port_state: Option<StateValue>,
    pub charger_derate_status: Option<StateValue>,
    pub remote_charging_available: Option<StateValue>,
    pub battery_needs_lfp_calibration: Option<StateValue>,
    pub charging_disabled_all: Option<StateValue>,

    // Climate
    pub cabin_climate_interior_temperature: Option<StateValue>,
    pub cabin_climate_driver_temperature: Option<StateValue>,
    pub cabin_preconditioning_status: Option<StateValue>,
    pub cabin_preconditioning_type: Option<StateValue>,
    pub defrost_defog_status: Option<StateValue>,
    pub seat_front_left_heat: Option<StateValue>,
    pub seat_front_right_heat: Option<StateValue>,
    pub seat_rear_left_heat: Option<StateValue>,
    pub seat_rear_right_heat: Option<StateValue>,
    pub seat_front_left_vent: Option<StateValue>,
    pub seat_front_right_vent: Option<StateValue>,
    pub steering_wheel_heat: Option<StateValue>,

    // Location
    pub gnss_location: Option<GnssLocation>,
    pub gnss_speed: Option<StateValue>,
    pub gnss_altitude: Option<StateValue>,
    pub gnss_bearing: Option<StateValue>,

    // Connectivity
    pub cloud_connection: Option<CloudConnection>,

    // OTA
    pub ota_current_version: Option<StateValue>,
    pub ota_available_version: Option<StateValue>,
    pub ota_current_version_number: Option<StateValue>,
    pub ota_available_version_number: Option<StateValue>,
    pub ota_current_version_git_hash: Option<StateValue>,
    pub ota_available_version_git_hash: Option<StateValue>,
    pub ota_status: Option<StateValue>,
    pub ota_current_status: Option<StateValue>,
    pub ota_current_version_week: Option<StateValue>,
    pub ota_current_version_year: Option<StateValue>,
    pub ota_available_version_week: Option<StateValue>,
    pub ota_available_version_year: Option<StateValue>,
    pub ota_download_progress: Option<StateValue>,
    pub ota_install_progress: Option<StateValue>,
    pub ota_install_ready: Option<StateValue>,
    pub ota_install_duration: Option<StateValue>,
    pub ota_install_time: Option<StateValue>,
    pub ota_install_type: Option<StateValue>,

    // Doors & closures
    pub door_front_left_closed: Option<StateValue>,
    pub door_front_right_closed: Option<StateValue>,
    pub door_rear_left_closed: Option<StateValue>,
    pub door_rear_right_closed: Option<StateValue>,
    pub door_front_left_locked: Option<StateValue>,
    pub door_front_right_locked: Option<StateValue>,
    pub door_rear_left_locked: Option<StateValue>,
    pub door_rear_right_locked: Option<StateValue>,
    pub closure_frunk_closed: Option<StateValue>,
    pub closure_frunk_locked: Option<StateValue>,
    pub closure_liftgate_closed: Option<StateValue>,
    pub closure_liftgate_locked: Option<StateValue>,
    pub closure_tailgate_closed: Option<StateValue>,
    pub closure_tailgate_locked: Option<StateValue>,
    pub closure_side_bin_left_closed: Option<StateValue>,
    pub closure_side_bin_right_closed: Option<StateValue>,
    pub closure_tonneau_closed: Option<StateValue>,

    // Windows
    pub window_front_left_closed: Option<StateValue>,
    pub window_front_right_closed: Option<StateValue>,
    pub window_rear_left_closed: Option<StateValue>,
    pub window_rear_right_closed: Option<StateValue>,
    pub window_front_left_calibrated: Option<StateValue>,
    pub window_front_right_calibrated: Option<StateValue>,
    pub window_rear_left_calibrated: Option<StateValue>,
    pub window_rear_right_calibrated: Option<StateValue>,

    // Tires
    pub tire_pressure_status_front_left: Option<StateValue>,
    pub tire_pressure_status_front_right: Option<StateValue>,
    pub tire_pressure_status_rear_left: Option<StateValue>,
    pub tire_pressure_status_rear_right: Option<StateValue>,
    pub tire_pressure_status_valid_front_left: Option<StateValue>,
    pub tire_pressure_status_valid_front_right: Option<StateValue>,
    pub tire_pressure_status_valid_rear_left: Option<StateValue>,
    pub tire_pressure_status_valid_rear_right: Option<StateValue>,

    // Security & misc
    pub pet_mode_status: Option<StateValue>,
    pub pet_mode_temperature_status: Option<StateValue>,
    pub gear_guard_locked: Option<StateValue>,
    pub gear_guard_video_status: Option<StateValue>,
    pub gear_guard_video_mode: Option<StateValue>,
    pub alarm_sound_status: Option<StateValue>,
    pub wiper_fluid_state: Option<StateValue>,
    pub limited_accel_cold: Option<StateValue>,
    pub limited_regen_cold: Option<StateValue>,
    pub twelve_volt_battery_health: Option<StateValue>,
    pub battery_hv_thermal_event: Option<StateValue>,
    pub battery_hv_thermal_event_propagation: Option<StateValue>,
    pub brake_fluid_low: Option<StateValue>,
    pub btm_ff_hardware_failure_status: Option<StateValue>,
    pub btm_ic_hardware_failure_status: Option<StateValue>,
    pub btm_lfd_hardware_failure_status: Option<StateValue>,
    pub btm_oc_hardware_failure_status: Option<StateValue>,
    pub btm_rfd_hardware_failure_status: Option<StateValue>,
    pub btm_rf_hardware_failure_status: Option<StateValue>,
    pub service_mode: Option<StateValue>,
    pub trailer_status: Option<StateValue>,
    pub car_wash_mode: Option<StateValue>,
}

/// Flexible value type — Rivian API returns mixed types (strings, numbers, bools)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateValue {
    pub value: serde_json::Value,
}

impl StateValue {
    /// Try to get the value as a string
    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }

    /// Try to get the value as f64 (handles both number and string representations)
    pub fn as_f64(&self) -> Option<f64> {
        match &self.value {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Get value as a display string regardless of underlying type
    pub fn to_display(&self) -> String {
        match &self.value {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_string(),
            other => other.to_string(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GnssLocation {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub time_stamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConnection {
    pub last_sync: Option<String>,
}

/// Convert a Celsius temperature to Fahrenheit.
pub fn celsius_to_fahrenheit(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Kilometers per statute mile. Single source of truth — the TUI, web view,
/// trip segmenter, and SQL aggregates must all divide by the same value or
/// their distances/efficiencies silently disagree.
pub const KM_PER_MI: f64 = 1.60934;
/// Meters per statute mile.
pub const METERS_PER_MI: f64 = 1609.344;

impl VehicleStateFields {
    pub fn get_f64(&self, field: &Option<StateValue>) -> Option<f64> {
        field.as_ref().and_then(|v| v.as_f64())
    }

    pub fn get_str<'a>(&'a self, field: &'a Option<StateValue>) -> &'a str {
        field.as_ref().and_then(|v| v.as_str()).unwrap_or("unknown")
    }

    pub fn get_boolish(&self, field: &Option<StateValue>) -> Option<bool> {
        let value = field.as_ref()?;

        match &value.value {
            serde_json::Value::Bool(flag) => Some(*flag),
            serde_json::Value::Number(num) => num.as_f64().map(|v| v != 0.0),
            serde_json::Value::String(text) => match text.to_ascii_lowercase().as_str() {
                "true" | "1" | "on" | "enabled" | "yes" | "closed" | "locked" => Some(true),
                "false" | "0" | "off" | "disabled" | "no" | "open" | "unlocked" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn battery_percent(&self) -> Option<f64> {
        self.get_f64(&self.battery_level)
    }

    /// distanceToEmpty is in km from the API
    pub fn range_miles(&self) -> Option<f64> {
        self.get_f64(&self.distance_to_empty)
            .map(|km| km / KM_PER_MI)
    }

    /// vehicleMileage is in meters from the API
    pub fn mileage(&self) -> Option<f64> {
        self.get_f64(&self.vehicle_mileage)
            .map(|m| m / METERS_PER_MI)
    }

    pub fn cabin_temp_f(&self) -> Option<f64> {
        self.get_f64(&self.cabin_climate_interior_temperature)
            .map(celsius_to_fahrenheit)
    }

    pub fn driver_temp_f(&self) -> Option<f64> {
        self.get_f64(&self.cabin_climate_driver_temperature)
            .map(celsius_to_fahrenheit)
    }

    pub fn speed_mph(&self) -> Option<f64> {
        self.get_f64(&self.gnss_speed).map(|kmh| kmh / KM_PER_MI)
    }

    pub fn altitude_ft(&self) -> Option<f64> {
        self.get_f64(&self.gnss_altitude)
            .map(|meters| meters * 3.28084)
    }

    pub fn power_state_str(&self) -> &str {
        self.get_str(&self.power_state)
    }

    pub fn gear_str(&self) -> &str {
        self.get_str(&self.gear_status)
    }

    pub fn drive_mode_str(&self) -> &str {
        self.get_str(&self.drive_mode)
    }

    pub fn charger_state_str(&self) -> &str {
        self.get_str(&self.charger_state)
    }

    pub fn charger_status_str(&self) -> &str {
        self.get_str(&self.charger_status)
    }

    pub fn battery_limit_percent(&self) -> Option<f64> {
        self.get_f64(&self.battery_limit)
    }

    pub fn battery_capacity_kwh(&self) -> Option<f64> {
        self.get_f64(&self.battery_capacity)
    }

    /// Remaining charge time, only while a session is active — the API keeps
    /// reporting a stale `timeToEndOfCharge` after the car is unplugged.
    pub fn time_to_full(&self) -> Option<String> {
        if !self.is_actively_charging() {
            return None;
        }
        self.get_f64(&self.time_to_end_of_charge).map(|v| {
            let mins = v as u64;
            let h = mins / 60;
            let m = mins % 60;
            if h > 0 {
                format!("{h}h {m}m")
            } else {
                format!("{m}m")
            }
        })
    }

    pub fn last_sync(&self) -> Option<&str> {
        self.cloud_connection
            .as_ref()
            .and_then(|c| c.last_sync.as_deref())
    }

    pub fn location_summary(&self) -> Option<String> {
        let location = self.gnss_location.as_ref()?;
        let lat = location.latitude?;
        let lon = location.longitude?;
        Some(format!("{lat:.3}, {lon:.3}"))
    }

    pub fn heading_summary(&self) -> Option<String> {
        let degrees = self.get_f64(&self.gnss_bearing)?;
        let cardinal = match (((degrees.rem_euclid(360.0) + 22.5) / 45.0).floor() as usize) % 8 {
            0 => "N",
            1 => "NE",
            2 => "E",
            3 => "SE",
            4 => "S",
            5 => "SW",
            6 => "W",
            _ => "NW",
        };
        Some(format!("{degrees:.0}° {cardinal}"))
    }

    pub fn ota_progress_summary(&self) -> Option<String> {
        let install = self
            .get_f64(&self.ota_install_progress)
            .filter(|v| *v > 0.0);
        let download = self
            .get_f64(&self.ota_download_progress)
            .filter(|v| *v > 0.0);

        if let Some(progress) = install {
            return Some(format!("install {progress:.0}%"));
        }

        if let Some(progress) = download {
            return Some(format!("download {progress:.0}%"));
        }

        let install_type = self.get_str(&self.ota_install_type);
        if install_type != "unknown" {
            return Some(install_type.to_string());
        }

        None
    }

    /// True when an OTA update is available — i.e. the API reports a
    /// distinct, non-sentinel `otaAvailableVersion`. Centralised so the TUI
    /// and web view can't drift apart on which sentinel values mean
    /// "nothing to install".
    pub fn update_available(&self) -> bool {
        let avail = self.get_str(&self.ota_available_version);
        let current = self.get_str(&self.ota_current_version);
        !matches!(avail, "" | "—" | "0.0.0" | "unknown") && avail != current
    }

    pub fn is_actively_charging(&self) -> bool {
        // chargerState values are tokens like "charging_active", "charging_ready",
        // "charging_inactive". A naive substring("active") match treats
        // "charging_inactive" as active because "inactive" contains "active".
        // Split into tokens and look for an exact match instead.
        let state = self.charger_state_str().to_ascii_lowercase();
        if state.split('_').any(|tok| tok == "active") {
            return true;
        }

        // chargerStatus uses "chrgr_sts_*" tokens — e.g. "chrgr_sts_charging".
        // "chrgr_sts_not_charging" must not match, so look for a "charging"
        // token that isn't preceded by "not".
        let status = self.charger_status_str().to_ascii_lowercase();
        let toks: Vec<&str> = status.split('_').collect();
        for (i, tok) in toks.iter().enumerate() {
            if *tok == "charging" {
                let preceded_by_not =
                    i.checked_sub(1).and_then(|j| toks.get(j)).copied() == Some("not");
                if !preceded_by_not {
                    return true;
                }
            }
        }
        false
    }

    #[cfg(test)]
    pub fn is_door_open(&self, door: &str) -> Option<bool> {
        let val = match door {
            "front_left" => &self.door_front_left_closed,
            "front_right" => &self.door_front_right_closed,
            "rear_left" => &self.door_rear_left_closed,
            "rear_right" => &self.door_rear_right_closed,
            _ => return None,
        };
        val.as_ref().map(|v| v.to_display() != "closed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_csrf_response() {
        let json = r#"{
            "data": {
                "createCsrfToken": {
                    "__typename": "CreateCsrfTokenResponse",
                    "csrfToken": "csrf-abc-123",
                    "appSessionToken": "app-sess-xyz"
                }
            }
        }"#;

        let resp: GraphQlResponse<CsrfData> = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.create_csrf_token.csrf_token, "csrf-abc-123");
        assert_eq!(data.create_csrf_token.app_session_token, "app-sess-xyz");
    }

    #[test]
    fn deserialize_login_mfa_response() {
        let json = r#"{
            "data": {
                "login": {
                    "__typename": "MobileMFALoginResponse",
                    "otpToken": "otp-token-999"
                }
            }
        }"#;

        let resp: GraphQlResponse<LoginData> = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.login.typename, "MobileMFALoginResponse");
        assert_eq!(data.login.otp_token.as_deref(), Some("otp-token-999"));
        assert!(data.login.access_token.is_none());
    }

    #[test]
    fn deserialize_login_direct_response() {
        let json = r#"{
            "data": {
                "login": {
                    "__typename": "MobileLoginResponse",
                    "accessToken": "access-tok",
                    "refreshToken": "refresh-tok",
                    "userSessionToken": "user-sess-tok"
                }
            }
        }"#;

        let resp: GraphQlResponse<LoginData> = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.login.typename, "MobileLoginResponse");
        assert_eq!(data.login.access_token.as_deref(), Some("access-tok"));
        assert_eq!(data.login.refresh_token.as_deref(), Some("refresh-tok"));
        assert_eq!(
            data.login.user_session_token.as_deref(),
            Some("user-sess-tok")
        );
        assert!(data.login.otp_token.is_none());
    }

    #[test]
    fn deserialize_otp_login_response() {
        let json = r#"{
            "data": {
                "loginWithOTP": {
                    "__typename": "MobileLoginResponse",
                    "accessToken": "otp-access",
                    "refreshToken": "otp-refresh",
                    "userSessionToken": "otp-user-sess"
                }
            }
        }"#;

        let resp: GraphQlResponse<OtpLoginData> = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.login_with_otp.access_token, "otp-access");
        assert_eq!(data.login_with_otp.refresh_token, "otp-refresh");
        assert_eq!(data.login_with_otp.user_session_token, "otp-user-sess");
    }

    #[test]
    fn deserialize_user_info_response() {
        let json = r#"{
            "data": {
                "currentUser": {
                    "vehicles": [
                        { "id": "vehicle-001", "name": "My R1T" },
                        { "id": "vehicle-002" }
                    ]
                }
            }
        }"#;

        let resp: GraphQlResponse<UserInfoData> = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert_eq!(data.current_user.vehicles.len(), 2);
        assert_eq!(data.current_user.vehicles[0].id, "vehicle-001");
        assert_eq!(
            data.current_user.vehicles[0].name.as_deref(),
            Some("My R1T")
        );
        assert!(data.current_user.vehicles[1].name.is_none());
    }

    #[test]
    fn deserialize_vehicle_state_full() {
        let json = r#"{
            "data": {
                "vehicleState": {
                    "cabinClimateInteriorTemperature": { "value": 22.5 },
                    "powerState": { "value": "ready" },
                    "driveMode": { "value": "everyday" },
                    "gearStatus": { "value": "park" },
                    "vehicleMileage": { "value": 12500.3 },
                    "batteryLevel": { "value": 72.0 },
                    "distanceToEmpty": { "value": 198.5 },
                    "chargerStatus": { "value": "chrgr_sts_not_connected" },
                    "chargerState": { "value": "charging_inactive" },
                    "batteryLimit": { "value": 80.0 },
                    "timeToEndOfCharge": { "value": 90.0 },
                    "batteryCapacity": { "value": 135.0 },
                    "cloudConnection": { "lastSync": "2026-03-19T06:00:00Z" },
                    "gnssLocation": { "latitude": 37.7749, "longitude": -122.4194, "timeStamp": "2026-03-19T06:00:00Z" },
                    "doorFrontLeftClosed": { "value": "closed" },
                    "doorFrontRightClosed": { "value": "open" }
                }
            }
        }"#;

        let resp: GraphQlResponse<VehicleStateData> = serde_json::from_str(json).unwrap();
        let vs = resp.data.unwrap().vehicle_state.unwrap();

        assert!((vs.battery_percent().unwrap() - 72.0).abs() < f64::EPSILON);
        // 198.5 km → miles
        assert!((vs.range_miles().unwrap() - 198.5 / 1.60934).abs() < 0.1);
        // 12500.3 meters → miles
        assert!((vs.mileage().unwrap() - 12500.3 / 1609.344).abs() < 0.01);
        assert_eq!(vs.power_state_str(), "ready");
        assert_eq!(vs.gear_str(), "park");
        assert_eq!(vs.drive_mode_str(), "everyday");
        assert_eq!(vs.charger_state_str(), "charging_inactive");
        assert_eq!(vs.charger_status_str(), "chrgr_sts_not_connected");
        assert!((vs.battery_limit_percent().unwrap() - 80.0).abs() < 0.01);
        assert!((vs.battery_capacity_kwh().unwrap() - 135.0).abs() < 0.01);
        // Unplugged: the stale timeToEndOfCharge is not surfaced (see
        // time_to_full_is_only_reported_while_charging for the live case).
        assert_eq!(vs.time_to_full(), None);
        assert_eq!(vs.last_sync().unwrap(), "2026-03-19T06:00:00Z");
        assert_eq!(vs.location_summary().as_deref(), Some("37.775, -122.419"));

        // cabin temp: 22.5 C → 72.5 F
        let temp_f = vs.cabin_temp_f().unwrap();
        assert!((temp_f - 72.5).abs() < 0.01);

        // doors
        assert_eq!(vs.is_door_open("front_left"), Some(false));
        assert_eq!(vs.is_door_open("front_right"), Some(true));

        // gnss
        let loc = vs.gnss_location.as_ref().unwrap();
        assert!((loc.latitude.unwrap() - 37.7749).abs() < 0.001);
    }

    #[test]
    fn deserialize_vehicle_state_partial() {
        let json = r#"{
            "data": {
                "vehicleState": {
                    "batteryLevel": { "value": 55.0 },
                    "distanceToEmpty": { "value": 150.0 }
                }
            }
        }"#;

        let resp: GraphQlResponse<VehicleStateData> = serde_json::from_str(json).unwrap();
        let vs = resp.data.unwrap().vehicle_state.unwrap();

        assert!((vs.battery_percent().unwrap() - 55.0).abs() < f64::EPSILON);
        // 150.0 km → miles
        assert!((vs.range_miles().unwrap() - 150.0 / 1.60934).abs() < 0.1);
        assert!(vs.mileage().is_none());
        assert_eq!(vs.power_state_str(), "unknown");
        assert!(vs.cabin_temp_f().is_none());
    }

    #[test]
    fn deserialize_vehicle_state_string_numbers() {
        // Rivian API sometimes returns numbers as strings
        let json = r#"{
            "data": {
                "vehicleState": {
                    "batteryLevel": { "value": "72" },
                    "vehicleMileage": { "value": "12500.3" },
                    "distanceToEmpty": { "value": "198.5" },
                    "powerState": { "value": "sleep" }
                }
            }
        }"#;

        let resp: GraphQlResponse<VehicleStateData> = serde_json::from_str(json).unwrap();
        let vs = resp.data.unwrap().vehicle_state.unwrap();

        assert!((vs.battery_percent().unwrap() - 72.0).abs() < 0.01);
        assert!((vs.mileage().unwrap() - 12500.3 / 1609.344).abs() < 0.01);
        // distanceToEmpty is in km
        assert!((vs.range_miles().unwrap() - 198.5 / 1.60934).abs() < 0.1);
        assert_eq!(vs.power_state_str(), "sleep");
    }

    #[test]
    fn deserialize_live_charging_session() {
        // Shape mirrors what the charging endpoint returns while a session
        // is in progress. Plain scalars sit alongside TimeStamped {value,
        // updatedAt} records for fields like power, soc, totalChargedEnergy.
        let json = r#"{
            "data": {
                "getLiveSessionData": {
                    "__typename": "LiveSessionData",
                    "chargerId": "RAN-CHRG-42",
                    "startTime": "2026-05-06T10:00:00.000Z",
                    "timeElapsed": 1820,
                    "locationId": "loc-1",
                    "currentPrice": 0.18,
                    "currentCurrency": "USD",
                    "isRivianCharger": true,
                    "isFreeSession": false,
                    "soc": { "value": 64.5, "updatedAt": "2026-05-06T10:30:00Z" },
                    "power": { "value": 142.7, "updatedAt": "2026-05-06T10:30:00Z" },
                    "current": { "value": 320.0, "updatedAt": "2026-05-06T10:30:00Z" },
                    "currentMiles": { "value": 213.0, "updatedAt": "2026-05-06T10:30:00Z" },
                    "kilometersChargedPerHour": { "value": 480.0, "updatedAt": "2026-05-06T10:30:00Z" },
                    "rangeAddedThisSession": { "value": 161.0, "updatedAt": "2026-05-06T10:30:00Z" },
                    "timeRemaining": { "value": 22.0, "updatedAt": "2026-05-06T10:30:00Z" },
                    "totalChargedEnergy": { "value": 31.4, "updatedAt": "2026-05-06T10:30:00Z" },
                    "vehicleChargerState": { "value": "charging_active", "updatedAt": "2026-05-06T10:30:00Z" }
                }
            }
        }"#;

        let resp: GraphQlResponse<LiveSessionData> = serde_json::from_str(json).unwrap();
        let session = resp.data.unwrap().get_live_session_data.unwrap();

        assert_eq!(session.charger_id.as_deref(), Some("RAN-CHRG-42"));
        assert_eq!(session.is_rivian_charger, Some(true));
        assert!((session.power_kw().unwrap() - 142.7).abs() < 0.01);
        assert!((session.soc_percent().unwrap() - 64.5).abs() < 0.01);
        assert!((session.total_energy_kwh().unwrap() - 31.4).abs() < 0.01);
        assert!((session.range_added_km().unwrap() - 161.0).abs() < 0.01);
        // 161 km / 1.60934 / 31.4 kWh ≈ 3.18 mi/kWh
        assert!((session.efficiency_mi_per_kwh().unwrap() - 3.186).abs() < 0.05);
        assert_eq!(session.vehicle_charger_state_str(), Some("charging_active"));
    }

    #[test]
    fn live_session_efficiency_filters_out_noise_floor() {
        let mut session = LiveChargingSession {
            range_added_this_session: Some(TsValue {
                value: serde_json::json!(0.1),
                updated_at: None,
            }),
            total_charged_energy: Some(TsValue {
                value: serde_json::json!(0.1),
                updated_at: None,
            }),
            ..Default::default()
        };
        assert!(
            session.efficiency_mi_per_kwh().is_none(),
            "tiny numerator/denominator should be filtered to None"
        );

        // After enough has flowed, efficiency becomes meaningful.
        session.total_charged_energy = Some(TsValue {
            value: serde_json::json!(10.0),
            updated_at: None,
        });
        session.range_added_this_session = Some(TsValue {
            value: serde_json::json!(50.0),
            updated_at: None,
        });
        assert!(session.efficiency_mi_per_kwh().is_some());
    }

    #[test]
    fn live_session_data_is_null_when_not_charging() {
        let json = r#"{ "data": { "getLiveSessionData": null } }"#;
        let resp: GraphQlResponse<LiveSessionData> = serde_json::from_str(json).unwrap();
        assert!(resp.data.unwrap().get_live_session_data.is_none());
    }

    #[test]
    fn is_actively_charging_distinguishes_active_from_inactive() {
        let active = VehicleStateFields {
            charger_state: Some(StateValue {
                value: serde_json::json!("charging_active"),
            }),
            ..Default::default()
        };
        assert!(active.is_actively_charging());

        let inactive = VehicleStateFields {
            charger_state: Some(StateValue {
                value: serde_json::json!("charging_inactive"),
            }),
            ..Default::default()
        };
        assert!(
            !inactive.is_actively_charging(),
            "charging_inactive must not be reported as actively charging"
        );

        let ready = VehicleStateFields {
            charger_state: Some(StateValue {
                value: serde_json::json!("charging_ready"),
            }),
            ..Default::default()
        };
        assert!(!ready.is_actively_charging());

        let by_status = VehicleStateFields {
            charger_status: Some(StateValue {
                value: serde_json::json!("chrgr_sts_charging"),
            }),
            ..Default::default()
        };
        assert!(by_status.is_actively_charging());

        let not_charging_status = VehicleStateFields {
            charger_status: Some(StateValue {
                value: serde_json::json!("chrgr_sts_not_charging"),
            }),
            ..Default::default()
        };
        assert!(
            !not_charging_status.is_actively_charging(),
            "chrgr_sts_not_charging must not be reported as actively charging"
        );
    }

    #[test]
    fn time_to_full_is_only_reported_while_charging() {
        // The wire keeps a stale timeToEndOfCharge after unplugging (up to
        // 6365 min observed with chargerState = charging_ready).
        let unplugged: VehicleStateFields = serde_json::from_str(
            r#"{ "chargerState": { "value": "charging_ready" },
                 "timeToEndOfCharge": { "value": 6365 } }"#,
        )
        .unwrap();
        assert_eq!(unplugged.time_to_full(), None);

        let charging: VehicleStateFields = serde_json::from_str(
            r#"{ "chargerState": { "value": "charging_active" },
                 "timeToEndOfCharge": { "value": 95 } }"#,
        )
        .unwrap();
        assert_eq!(charging.time_to_full().as_deref(), Some("1h 35m"));
    }

    #[test]
    fn deserialize_graphql_error() {
        let json = r#"{
            "data": null,
            "errors": [
                { "message": "Not authorized" },
                { "message": "Token expired", "extensions": { "code": "UNAUTHENTICATED" } }
            ]
        }"#;

        let resp: GraphQlResponse<VehicleStateData> = serde_json::from_str(json).unwrap();
        assert!(resp.data.is_none());
        let errors = resp.errors.unwrap();
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].message, "Not authorized");
        assert_eq!(errors[1].message, "Token expired");
    }

    #[test]
    fn auth_tokens_roundtrip() {
        let tokens = AuthTokens {
            access_token: "at".into(),
            refresh_token: "rt".into(),
            user_session_token: "ust".into(),
            csrf_token: "csrf".into(),
            app_session_token: "ast".into(),
            vehicle_id: "vid".into(),
            device_id: Some("device-1".into()),
        };
        let json = serde_json::to_string(&tokens).unwrap();
        let parsed: AuthTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_token, "at");
        assert_eq!(parsed.vehicle_id, "vid");
        assert_eq!(parsed.device_id.as_deref(), Some("device-1"));
    }

    #[test]
    fn auth_tokens_deserialize_legacy_payload_without_device_id() {
        // Older saved payloads predate `device_id`; they must still parse.
        let legacy = r#"{
            "access_token": "at",
            "refresh_token": "rt",
            "user_session_token": "ust",
            "csrf_token": "csrf",
            "app_session_token": "ast",
            "vehicle_id": "vid"
        }"#;
        let parsed: AuthTokens = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.vehicle_id, "vid");
        assert!(parsed.device_id.is_none());
    }

    #[test]
    fn mfa_state_roundtrip() {
        let mfa = MfaState {
            email: "test@example.com".into(),
            csrf_token: "csrf".into(),
            app_session_token: "ast".into(),
            otp_token: "otp".into(),
            device_id: "device-1".into(),
            timestamp: 1710000000,
        };
        let json = serde_json::to_string(&mfa).unwrap();
        let parsed: MfaState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.email, "test@example.com");
        assert_eq!(parsed.otp_token, "otp");
        assert_eq!(parsed.timestamp, 1710000000);
    }
}
