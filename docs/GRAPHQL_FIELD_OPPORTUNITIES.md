# GraphQL Field Opportunities (proposal)

Research pass surveying what open-source Rivian clients fetch that **rivian-tui does
not yet**, so we can decide what to add in a future pass. Nothing here is wired in —
this is a menu, not a changelog.

**Sources mined (canonical reverse-engineered clients):**
- [`bretterer/rivian-python-client`](https://github.com/bretterer/rivian-python-client) — `src/rivian/rivian.py`, `src/rivian/const.py`. The authoritative community client; the Home Assistant integration inherits its exact field set.
- [`bttnns/rivflux`](https://github.com/bttnns/rivflux) (Go) — our project's reference repo.
- [`kaedenbrinkman/rivian-api`](https://github.com/kaedenbrinkman/rivian-api) (JS) — README/API docs.

All three expose a strict subset of the python client's field set, so the python
client is the superset we mined against. Field names below are **verbatim** from the
GraphQL schema and exclude anything we already request in `src/api/queries.rs`.

---

## A. Trip history query — DOES NOT EXIST ⚠️

**No open-source Rivian client exposes a trip / driving-history / energy-history query.**

The complete set of GraphQL operations in `rivian-python-client` is:
`CreateCSRFToken`, `Login`, `LoginWithOTP`, `EnrollPhone`, `DisenrollPhone`,
`DriversAndKeys`, `getUserInfo`, `getRegisteredWallboxes`, `getVehicleCommand`,
`getVehicleImages`, `GetVehicleState`, `getOTAUpdateDetails`, `getLiveSessionData`,
`sendVehicleCommand`, and the `VehicleState` subscription. An exhaustive scan for
`history` / `trip` / `energy` operations turned up nothing; the closest historical
data is `getCompletedSessionSummaries` (charging sessions — which we already use).

**Consequence:** our "last 5 trips" feature is derived locally from stored
`vehicle_state` snapshots (`Db::recent_trips` → `segment_trips`), and that is the
**only** viable source today. The `Trip` type is deliberately source-agnostic, so if
an undocumented `getTrips`/energy-history operation is ever discovered it can populate
`Trip` directly without touching the renderers. There is no existing client to use as
a template for discovering one.

---

## B. OTA / software-update fields (high value)

Already in the `GetVehicleState` query we call — just add to the selection:

| Field | Why |
|-------|-----|
| `otaCurrentVersionNumber` | Clean monotonic build number for diffing/sorting |
| `otaAvailableVersionNumber` | Same, for the pending build |
| `otaCurrentVersionGitHash` | Exact build identity |
| `otaAvailableVersionGitHash` | Exact pending-build identity |

Plus a **separate query we don't use at all** — `getOTAUpdateDetails` (gateway
endpoint, variable `vehicleId`), returning release/download metadata absent from
vehicle state:

```graphql
availableOTAUpdateDetails { url version locale }
currentOTAUpdateDetails   { url version locale }
```

`url` typically points at release-notes / update-detail content (`locale` localizes
it) — this is the "what's in this update" payload, a natural companion to the OTA
work just shipped on the web dashboard.

---

## C. Other high-value vehicleState / charging fields

All from the same queries we already issue (unless noted). Excludes fields we already
fetch (e.g. `limitedAccelCold`, `limitedRegenCold`, `carWashMode`, `trailerStatus`,
`wiperFluidState`, `cabinPreconditioningType`, `gearGuardVideoMode/Status`,
`batteryHvThermalEvent`, and the full live-session set — all already in our queries).

**Tire pressure — actual numeric PSI** (we only fetch the `tirePressureStatus*` enums):
- `tirePressureFrontLeft`, `tirePressureFrontRight`, `tirePressureRearLeft`, `tirePressureRearRight` *(subscription-only — see caveat)*
- `tirePressureStatusValidFrontLeft` / `…FrontRight` / `…RearLeft` / `…RearRight` (validity flags, main query)

**Charging / trip-charging:**
- `chargingTripTargetSoc` — target SoC for trip charging *(subscription-only)*
- `chargingTripTargetMinsRemaining` — minutes to reach the trip target *(subscription-only)*
- `chargingTimeEstimationValidity` — whether the time-remaining estimate is trustworthy *(subscription-only)*
- `chargingDisabledAll` — global charging-disabled flag (main query)
- `chargingDisabledACFaultState` — AC charging fault *(subscription-only)*
- `closureChargePortDoorNextAction` — pending charge-port-door action *(subscription-only)*

**Range / battery health:**
- `rangeThreshold` — low-range threshold
- `batteryNeedsLfpCalibration` — LFP pack needs a 100% calibration charge (very useful for LFP owners)
- `batteryCellType` — LFP vs NMC chemistry
- `coldRangeNotification` — cold-weather range warning *(subscription-only)*
- `batteryHvThermalEventPropagation` — HV thermal-event propagation flag

**Safety / fluids:**
- `brakeFluidLow`
- `rearHitchStatus`

**Driver identity / Gear Guard cam:**
- `activeDriverName` — which driver profile/key is active *(also in subscription set)*
- `gearGuardVideoTermsAccepted`

**Geo:**
- `geoLocation` — geofence / named-location resolution
- `gnssError` — GNSS error/accuracy (complements raw `gnssLocation`)

**New query — home charger telemetry:** `getRegisteredWallboxes` (charging endpoint,
no variables): `wallboxId userId wifiId name linked latitude longitude chargingStatus
power currentVoltage currentAmps softwareVersion model serialNumber maxAmps maxVoltage
maxPower`.

---

## Caveat: subscription-only fields

Fields tagged *(subscription-only)* are documented by the python client as reliably
returned only over the `VehicleState` **websocket subscription**
(`wss://api.rivian.com/gql-consumer-subscriptions/graphql`), not the polled
`GetVehicleState` query — over plain polling they may come back `null`. We currently
poll, so adding these only pays off if/when we add the websocket path (or accept that
they may be empty). No charging-**schedule** read query exists in any client
(schedules are set via commands, not queried), so `chargingTripTargetSoc` above is the
only trip-charging data readable today.

---

## Suggested priority

1. `getOTAUpdateDetails` (release-notes URL) + `ota*VersionNumber` / `*GitHash` — pairs with the OTA work just done.
2. Numeric tire PSI (`tirePressureFrontLeft`…) — concrete data behind the status enums.
3. `chargingTripTargetSoc` / `chargingTripTargetMinsRemaining`.
4. `batteryNeedsLfpCalibration`, `rangeThreshold`, `batteryCellType`, `brakeFluidLow`.
5. `getRegisteredWallboxes` — if home-charger telemetry is wanted.
