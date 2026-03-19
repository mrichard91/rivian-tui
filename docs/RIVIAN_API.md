# Rivian GraphQL API Reference

Reverse-engineered from community projects: [rivian-python-client](https://github.com/bretterer/rivian-python-client), [rivian-python-api](https://github.com/the-mace/rivian-python-api), [home-assistant-rivian](https://github.com/bretterer/home-assistant-rivian).

## Endpoints

| Name | URL | Purpose |
|------|-----|---------|
| Gateway | `https://rivian.com/api/gql/gateway/graphql` | Auth, vehicle state, commands, trip planning |
| Charging | `https://rivian.com/api/gql/chrg/user/graphql` | Charging sessions, live charging data |
| Orders | `https://rivian.com/api/gql/orders/graphql` | Order status |
| Content | `https://rivian.com/api/gql/content/graphql` | Content/articles |
| Transactions | `https://rivian.com/api/gql/t2d/graphql` | Transaction data |
| WebSocket | `wss://api.rivian.com/gql-consumer-subscriptions/graphql` | Live vehicle state subscriptions |

## Required Headers

All requests mimic the iOS Rivian app:

```
User-Agent: RivianApp/1304 CFNetwork/1404.0.5 Darwin/22.3.0
Apollographql-Client-Name: com.rivian.ios.consumer-apollo-ios
Content-Type: application/json
Accept: application/json
```

### Auth-specific headers

| Header | Value | When |
|--------|-------|------|
| `Csrf-Token` | From `CreateCSRFToken` | All requests after CSRF fetch |
| `A-Sess` | `appSessionToken` from CSRF | All requests |
| `U-Sess` | `userSessionToken` from login | Authenticated requests |
| `Dc-Cid` | `m-ios-{uuid}` | All requests |
| `Authorization` | `Bearer {accessToken}` | Authenticated requests |

## Auth Flow

1. `CreateCSRFToken` mutation → `csrfToken` + `appSessionToken`
2. `Login` mutation (email, password) → either:
   - `MobileLoginResponse`: `accessToken`, `refreshToken`, `userSessionToken`
   - `MobileMFALoginResponse`: `otpToken` (need step 3)
3. `LoginWithOTP` mutation (email, otpCode, otpToken) → tokens
4. `getUserInfo` query → vehicle IDs

## Vehicle State Query

**Endpoint:** Gateway
**Variable:** `$vehicleID: String!` (this is the VIN)
**Argument:** `vehicleState(id: $vehicleID)`

### Available Fields

All fields return `{ value }` (flexible type — string, number, or bool) unless noted.

#### Power & Drive
| Field | Example Value | Unit |
|-------|--------------|------|
| `powerState` | `"sleep"`, `"ready"`, `"go"` | — |
| `driveMode` | `"everyday"`, `"sport"`, `"conserve"` | — |
| `gearStatus` | `"park"`, `"drive"`, `"reverse"`, `"neutral"` | — |
| `vehicleMileage` | `10192690` | **meters** (divide by 1609.344 for miles) |

#### Battery & Charging
| Field | Example Value | Unit |
|-------|--------------|------|
| `batteryLevel` | `61.7` | percent |
| `batteryLimit` | `70` | percent |
| `batteryCapacity` | `136.3` | kWh |
| `distanceToEmpty` | `353` | **km** (divide by 1.60934 for miles) |
| `chargerStatus` | `"chrgr_sts_not_connected"` | — |
| `chargerState` | `"charging_ready"`, `"charging_active"` | — |
| `timeToEndOfCharge` | `0` | minutes |
| `chargePortState` | `"open"`, `"closed"` | — |
| `chargerDerateStatus` | `"NONE"` | — |
| `remoteChargingAvailable` | `0` | boolean-ish |
| `batteryHvThermalEvent` | `"off"` | — |

#### Climate
| Field | Example Value | Unit |
|-------|--------------|------|
| `cabinClimateInteriorTemperature` | `18` | **Celsius** |
| `cabinClimateDriverTemperature` | `20` | **Celsius** |
| `cabinPreconditioningStatus` | `"undefined"` | — |
| `cabinPreconditioningType` | `"NONE"` | — |
| `defrostDefogStatus` | `"Off"` | — |
| `seatFrontLeftHeat` | `"Off"`, `"Low"`, `"Med"`, `"High"` | — |
| `seatFrontRightHeat` | `"Off"` | — |
| `seatRearLeftHeat` | `"Off"` | — |
| `seatRearRightHeat` | `"Off"` | — |
| `seatFrontLeftVent` | `"Off"` | — |
| `seatFrontRightVent` | `"Off"` | — |
| `steeringWheelHeat` | `"Off"` | — |

#### Location (custom types, not `{ value }`)
| Field | Structure | Unit |
|-------|-----------|------|
| `gnssLocation` | `{ latitude, longitude, timeStamp }` | degrees, ISO8601 |
| `gnssSpeed` | `{ value }` | km/h (TBC) |
| `gnssAltitude` | `{ value }` (e.g., `56.9`) | meters |
| `gnssBearing` | `{ value }` (e.g., `30.321`) | degrees |

#### Connectivity (custom type)
| Field | Structure |
|-------|-----------|
| `cloudConnection` | `{ lastSync }` — ISO8601 timestamp |

#### OTA Updates
| Field | Example Value | Notes |
|-------|--------------|-------|
| `otaCurrentVersion` | `"2026.03.0"` | — |
| `otaAvailableVersion` | `"0.0.0"` | `0.0.0` = no update |
| `otaStatus` | `"Idle"` | `Idle`, `Downloading`, `Installing` |
| `otaCurrentStatus` | `"Install_Success"` | Last install result |
| `otaCurrentVersionWeek` | `3` | — |
| `otaCurrentVersionYear` | `2026` | — |
| `otaAvailableVersionWeek` | `0` | — |
| `otaAvailableVersionYear` | `0` | — |
| `otaDownloadProgress` | `0` | percent |
| `otaInstallProgress` | `0` | percent |
| `otaInstallReady` | `"ota_not_available"` | — |
| `otaInstallDuration` | `0` | minutes (TBC) |
| `otaInstallTime` | `0` | — |
| `otaInstallType` | `"Convenience"` | — |

#### Doors & Closures
Each has `Closed`/`Locked` variants: `doorFrontLeftClosed`, `doorFrontLeftLocked`, etc.

| Field Pattern | Example Values |
|---------------|---------------|
| `door{Front,Rear}{Left,Right}Closed` | `"closed"`, `"open"` |
| `door{Front,Rear}{Left,Right}Locked` | `"locked"`, `"unlocked"` |
| `closureFrunk{Closed,Locked}` | `"closed"`, `"locked"` |
| `closureLiftgate{Closed,Locked}` | `"closed"`, `"signal_not_available"` |
| `closureTailgate{Closed,Locked}` | `"closed"` |
| `closureSideBin{Left,Right}Closed` | `"closed"` |
| `closureTonneau{Closed,Locked}` | `"signal_not_available"` |

#### Windows
| Field | Values |
|-------|--------|
| `window{Front,Rear}{Left,Right}Closed` | `"closed"`, `"open"` |
| `window{Front,Rear}{Left,Right}Calibrated` | calibration status |
| `windowsNextAction` | — |

#### Tires (status only, no PSI values)
| Field | Values |
|-------|--------|
| `tirePressureStatus{Front,Rear}{Left,Right}` | `"OK"`, `"Low"` |
| `tirePressureStatusValid{Front,Rear}{Left,Right}` | validation |

#### Security & Misc
| Field | Example Value |
|-------|--------------|
| `petModeStatus` | `"Disabled"` |
| `petModeTemperatureStatus` | `"Default"` |
| `gearGuardLocked` | `"locked"` |
| `gearGuardVideoStatus` | `"Enabled"` |
| `gearGuardVideoMode` | `"Away_From_Home"` |
| `alarmSoundStatus` | `"false"` |
| `wiperFluidState` | `"normal"` |
| `limitedAccelCold` | `1` (boolean-ish) |
| `limitedRegenCold` | `1` |
| `twelveVoltBatteryHealth` | `"NORMAL_OPERATION"` |
| `serviceMode` | `"off"` |
| `trailerStatus` | `"TRAILER_NOT_PRESENT"` |
| `carWashMode` | `"off"` |

#### Fields that return null (vehicle-dependent)
- `brakeFluidLow` — may not be exposed on all models
- `rearHitchStatus` — only on vehicles with hitch

## Charging Sessions

**Endpoint:** Charging (`/chrg/user/graphql`)
**Operation:** `getCompletedSessionSummaries`
**Variables:** None (returns all sessions for authenticated user)

### Response Fields
| Field | Type | Example |
|-------|------|---------|
| `transactionId` | String | `"USCPI4902136551"` |
| `startInstant` | String (ISO8601) | `"2026-02-28T14:56:22.000Z"` |
| `endInstant` | String (ISO8601) | `"2026-02-28T15:34:30.000Z"` |
| `totalEnergyKwh` | Float | `5.9012` |
| `rangeAddedKm` | Float | `61` (km) |
| `chargerType` | String? | `"RAN15_DISPENSER"`, null |
| `vendor` | String? | `"ChargePoint"`, `"Rivian"` |
| `city` | String? | `"West Hartford"` |
| `currencyCode` | String? | `"USD"` |
| `paidTotal` | Float? | `0`, null |
| `vehicleId` | String? | VIN |
| `vehicleName` | String? | — |
| `isHomeCharger` | Bool? | `false` |
| `isPublic` | Bool? | — |
| `isRoamingNetwork` | Bool? | — |

## Live Charging Session

**Endpoint:** Charging
**Operation:** `getLiveSessionData`
**Variables:** `$vehicleId: ID`

Returns real-time charging data: power, SOC, time remaining, energy delivered. Only available during active charging.

## Trip Planning

**Endpoint:** Gateway
**Operation:** `planTrip`
**Variables:** origin/destination coordinates, vehicleId, startingSoc, startingRangeMeters

Returns routes with charging waypoints. Not trip history (no driving history API exists).

## Testing with --stdout

```bash
# Default: dump vehicle state
cargo run -- --stdout

# Custom query
cargo run -- --stdout --query "query getUserInfo { currentUser { vehicles { id } } }"

# Charging endpoint
cargo run -- --stdout --endpoint charging --query "query getCompletedSessionSummaries { getCompletedSessionSummaries { startInstant totalEnergyKwh vendor city } }"

# Vehicle state with specific fields
cargo run -- --stdout --query 'query GetVehicleState($vehicleID: String!) { vehicleState(id: $vehicleID) { batteryLevel { value } otaCurrentVersion { value } } }'
```

## Notes

- Introspection queries are **disabled** on all endpoints
- `vehicleMileage` is in **meters**, `distanceToEmpty` is in **km**
- `cabinClimateInteriorTemperature` is in **Celsius**
- No trip/driving history API exists
- No actual tire pressure PSI values — only status (`OK`/`Low`)
- WebSocket subscriptions available for real-time vehicle state updates
