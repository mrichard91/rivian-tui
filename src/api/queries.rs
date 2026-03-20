//! GraphQL query/mutation strings for the Rivian API.
//!
//! Query format verified against rivian-python-client and rivian-python-api.
//! Variable naming: $vehicleID (capital D), type String!, argument `id:`.

pub const CREATE_CSRF_TOKEN: &str = "mutation CreateCSRFToken { createCsrfToken { __typename csrfToken appSessionToken } }";

pub const LOGIN: &str = "mutation Login($email: String!, $password: String!) { login(email: $email, password: $password) { __typename ... on MobileLoginResponse { accessToken refreshToken userSessionToken } ... on MobileMFALoginResponse { otpToken } } }";

pub const LOGIN_WITH_OTP: &str = "mutation LoginWithOTP($email: String!, $otpCode: String!, $otpToken: String!) { loginWithOTP(email: $email, otpCode: $otpCode, otpToken: $otpToken) { __typename accessToken refreshToken userSessionToken } }";

pub const GET_USER_INFO: &str = "query getUserInfo { currentUser { vehicles { id } } }";

pub const GET_VEHICLE_STATE: &str = "\
query GetVehicleState($vehicleID: String!) { vehicleState(id: $vehicleID) { \
powerState { value } driveMode { value } gearStatus { value } vehicleMileage { value } \
batteryLevel { value } distanceToEmpty { value } chargerStatus { value } chargerState { value } \
batteryLimit { value } timeToEndOfCharge { value } batteryCapacity { value } \
chargePortState { value } chargerDerateStatus { value } remoteChargingAvailable { value } \
cabinClimateInteriorTemperature { value } cabinClimateDriverTemperature { value } \
cabinPreconditioningStatus { value } cabinPreconditioningType { value } \
defrostDefogStatus { value } \
seatFrontLeftHeat { value } seatFrontRightHeat { value } \
seatRearLeftHeat { value } seatRearRightHeat { value } \
seatFrontLeftVent { value } seatFrontRightVent { value } \
steeringWheelHeat { value } \
cloudConnection { lastSync } gnssLocation { latitude longitude timeStamp } \
gnssSpeed { value } gnssAltitude { value } gnssBearing { value } \
otaCurrentVersion { value } otaAvailableVersion { value } otaStatus { value } \
otaCurrentStatus { value } otaCurrentVersionWeek { value } otaCurrentVersionYear { value } \
otaAvailableVersionWeek { value } otaAvailableVersionYear { value } \
otaDownloadProgress { value } otaInstallProgress { value } otaInstallReady { value } \
otaInstallDuration { value } otaInstallTime { value } otaInstallType { value } \
doorFrontLeftClosed { value } doorFrontRightClosed { value } \
doorRearLeftClosed { value } doorRearRightClosed { value } \
doorFrontLeftLocked { value } doorFrontRightLocked { value } \
doorRearLeftLocked { value } doorRearRightLocked { value } \
closureFrunkClosed { value } closureFrunkLocked { value } \
closureLiftgateClosed { value } closureLiftgateLocked { value } \
closureTailgateClosed { value } closureTailgateLocked { value } \
closureSideBinLeftClosed { value } closureSideBinRightClosed { value } \
windowFrontLeftClosed { value } windowFrontRightClosed { value } \
windowRearLeftClosed { value } windowRearRightClosed { value } \
tirePressureStatusFrontLeft { value } tirePressureStatusFrontRight { value } \
tirePressureStatusRearLeft { value } tirePressureStatusRearRight { value } \
petModeStatus { value } petModeTemperatureStatus { value } \
gearGuardLocked { value } gearGuardVideoStatus { value } gearGuardVideoMode { value } \
alarmSoundStatus { value } wiperFluidState { value } \
limitedAccelCold { value } limitedRegenCold { value } \
twelveVoltBatteryHealth { value } batteryHvThermalEvent { value } \
serviceMode { value } trailerStatus { value } carWashMode { value } \
} }";

/// Charging endpoint — completed session summaries
pub const GET_CHARGING_SESSIONS: &str = "query getCompletedSessionSummaries { getCompletedSessionSummaries { chargerType currencyCode paidTotal startInstant endInstant totalEnergyKwh rangeAddedKm city transactionId vehicleId vehicleName vendor isRoamingNetwork isPublic isHomeCharger } }";
