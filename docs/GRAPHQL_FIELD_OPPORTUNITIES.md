# GraphQL Field Opportunities

Survey of what open-source Rivian clients fetch versus what **rivian-tui** requests
today. Sections are marked **wired in** (already in `src/api/queries.rs`) or
**not yet** (still a menu item). Re-verified 2026-09-15 against the python
client's `const.py` / `rivian.py` on `main`.

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

## B. OTA / software-update fields — **wired in**

All four fields below and the `getOTAUpdateDetails` query are requested, rendered
(TUI System panel, web Software card) and persisted to `vehicle_state`.

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

## C. Other vehicleState / charging fields

### Wired in

`tirePressureStatusValid*` (×4), `chargingDisabledAll`, `batteryNeedsLfpCalibration`,
`batteryHvThermalEventPropagation`, `brakeFluidLow`, the six `btm*HardwareFailureStatus`
fields, `windowsNextAction`'s siblings `window*Calibrated` (×4), and
`closureTonneauClosed`. All feed `AlertsView` and are persisted.

### Not yet — polled `GetVehicleState` fields (cheap to add)

- `closureTonneauLocked`, `closureSideBinLeftLocked`, `closureSideBinRightLocked` — the
  Access panel has no tonneau / side-bin lock state today.
- `closure{Frunk,Liftgate,Tailgate,Tonneau,SideBinLeft,SideBinRight}NextAction`,
  `windowsNextAction` — what the next button press will do.
- `rangeThreshold` — low-range alert threshold.
- `batteryCellType` — LFP vs NMC; contextualises the calibration alert.
- `rearHitchStatus`, `seatThirdRowLeftHeat`, `seatThirdRowRightHeat` — model-dependent
  (R1S / hitch-equipped). Partial-response handling in `client.rs` means an unsupported
  field logs a warning instead of failing the poll.
- `activeDriverName`, `geoLocation`, `gnssError`, `gearGuardVideoTermsAccepted`.

### Not yet — subscription-only (websocket)

- Numeric tire PSI: `tirePressureFrontLeft` / `FrontRight` / `RearLeft` / `RearRight`.
- `chargingTripTargetSoc`, `chargingTripTargetMinsRemaining`, `chargingTimeEstimationValidity`.
- `chargingDisabledACFaultState`, `closureChargePortDoorNextAction`, `coldRangeNotification`.

### Not yet — additional queries

- **`getVehicleChargingSchedules`** (gateway, `$vehicleId`):
  `getVehicle(id: $vehicleId) { chargingSchedules { weekDays startTime duration location amperage enabled } }`.
  A read query for charge schedules *does* exist (earlier revisions of this doc said
  otherwise). Natural "next scheduled charge" row on the Charging card.
- **`getRegisteredWallboxes`** (charging endpoint, no variables): `wallboxId userId
  wifiId name linked latitude longitude chargingStatus power currentVoltage currentAmps
  softwareVersion model serialNumber maxAmps maxVoltage maxPower`.
- **`getVehicleImages`** (gateway): rendered vehicle image URLs for the web dashboard.
- `DriversAndKeys`, `getUserInfo { enrolledPhones registrationChannels }` — account PII;
  deliberately not planned.

---

## Caveat: subscription-only fields

Fields tagged *(subscription-only)* are documented by the python client as reliably
returned only over the `VehicleState` **websocket subscription**
(`wss://api.rivian.com/gql-consumer-subscriptions/graphql`), not the polled
`GetVehicleState` query — over plain polling they may come back `null`. We currently
poll, so adding these only pays off if/when we add the websocket path (or accept that
they may be empty).

---

## Suggested priority

1. Polled fields above (locks, next-actions, `rangeThreshold`, `batteryCellType`) — query
   string + struct fields + a `VEHICLE_STATE_DATA_COLUMNS` row each.
2. `getVehicleChargingSchedules` + a low-range alert from `rangeThreshold`.
3. `getRegisteredWallboxes` — if a Rivian wallbox is registered.
4. Websocket subscription — unlocks numeric tire PSI and the trip-charging fields, and
   replaces interval polling with push.

Validate per-model availability with `cargo run -- --stdout --query '...'` before wiring a
field into the UI; `--stdout` injects `$vehicleID` (gateway) or `$vehicleId` (charging).
