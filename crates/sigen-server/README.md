# Sigen server

Axum web dashboard and Prometheus exporter for one Sigenergy hybrid inverter
and its attached battery cluster. Uses `sigen-modbus` and its V2.7 register map.
Enable the inverter's Modbus TCP server first.

## Container

Build from the repository root with Podman:

```sh
podman build -f Containerfile -t sigen-server .
podman run --rm --name sigen-server \
  -p 127.0.0.1:3000:3000 \
  -v sigen-data:/data \
  -e SIGEN_ADDRESS=192.168.1.50:502 \
  -e SIGEN_UNIT_ID=1 \
  sigen-server
```

Docker works with the same arguments (`docker build -f Containerfile ...`).
The multi-stage build uses a locked dependency graph and a stripped static musl
binary. The final `scratch` image contains only the executable, CA certificates,
and a writable `/data` directory. It runs as UID/GID `65532:65532`, with no shell
or package manager. Dashboard assets are embedded in the executable.

The container listens on `0.0.0.0:3000`; the example publishes it on host localhost.
Alert settings and the append-only write audit log are stored in `/data/alerts.json`
and `/data/writes.jsonl`; the event journal is `/data/sigen-events.sqlite3`.
Use a named volume as above to retain them across container
replacement. Bind mounts must be writable by UID/GID 65532. Pass existing `SIGEN_*`
options through environment variables or append CLI options after the image name.
For example, `podman run --rm sigen-server --help` prints the available options.
Mount a private MQTT CA certificate read-only and set `SIGEN_MQTT_CA_FILE` to its
path inside the container when needed. SIGTERM requests a graceful shutdown.

## Run

Command-line options use Clap. Run `cargo run -p sigen-server -- --help` for
all options, or `--version` for the version. For example:

```sh
cargo run -p sigen-server -- --address 192.168.1.50:502 --unit-id 1 --bind 127.0.0.1:3000
cargo run -p sigen-server -- --address 192.168.1.50:502 --mqtt-host 192.168.1.10 --mqtt-id house_battery --mqtt-controls
```

All existing `SIGEN_*` environment variables remain supported. Explicit options
override environment values, which override defaults. Option names match their
environment variable suffixes with hyphens: `SIGEN_POLL_SECONDS` becomes
`--poll-seconds`, `SIGEN_ALERTS_FILE` becomes `--alerts-file`, and so on.
`--address` (or `SIGEN_ADDRESS`) is required. `--mqtt-tls` and `--mqtt-controls`
turn those options on; `--mqtt-tls=false` and `--mqtt-controls=false` override
an enabled environment setting. MQTT credentials are hidden from help output;
prefer the password environment variable over a command-line password.
The HTTP URL is logged at startup.

Environment-only startup also works:

```sh
SIGEN_ADDRESS=192.168.1.50:502 SIGEN_UNIT_ID=1 cargo run -p sigen-server
```

Open **http://127.0.0.1:3000**. The dashboard shows solar/grid/battery/inverter
power, battery charge and health, temperatures, grid frequency and alarm words.
**Today’s energy** adds inverter solar generation, whole-plant load consumption,
and this inverter’s battery charge/discharge totals. Each value has its own sample
age and device-local date; previous-day samples are hidden after the device reports
a new local day. **Grid energy counters** shows plant import/export totals using
the new statistics interface, not daily consumption. These counters begin at zero
when firmware first introduces that interface; they do not inherit earlier totals.
**Electrical details** expands to show inverter phase voltages/currents, grid
sensor phase power with import/export direction, output wiring and power factor.
Absent or ambiguously defined phases are shown as — rather than interpreted as
zero; grid phase power is unavailable when the grid sensor is disconnected.
The dashboard automatically follows your system's light or dark appearance.
Select **Pause updates** to stop automatic refreshes while you read the dashboard
or inspect metrics. **Resume updates** refreshes the display and restarts polling.
Control submissions and scenario changes still refresh the display while paused.
To preview the dashboard, open **http://127.0.0.1:3000/?demo=1** or select
**Demo mode** in the footer. A persistent banner identifies simulated data.
Choose solar generation, nighttime discharge, a battery alarm, or an offline
device; controls and alert toggles operate only on browser-local demo state.
The demo makes no status API requests, journal requests, hardware writes, or notification requests.
Reloading resets demo changes; **Exit demo** returns to the live dashboard.
Select **Demo Prometheus metrics** to view simulated exporter output that follows
the current scenario, control settings, and alert toggles. Offline scenarios omit
stale sensor metrics, just like the live exporter. This preview is browser-only;
the server's `/metrics` endpoint continues to expose real server status.
This UI flag does not stop the server's existing background hardware polling.

For a live demo that exercises actual Modbus TCP, start the bundled simulator
in another terminal:

```sh
cargo run -p sigen-modbus --bin sigen-simulator -- --scenario cycle
```

Then run this server with `--address 127.0.0.1:1502 --unit-id 1` and open `/`
without `?demo=1`. Use separate `--alerts-file` and `--write-audit-file` paths
for demo state. The simulator identifies itself as `SigenStor SIMULATOR`, supports
live controls, and cycles through solar, night, battery alarm and off-grid states.
Real server integrations remain active: leave Pushover/MQTT credentials unset
unless you want simulated events delivered to those services. See the
[simulator instructions](../sigen-modbus/README.md#live-modbus-tcp-simulator)
for all scenarios and supported registers.

The live dashboard provides **whole-plant** controls for remote EMS enable, its operating mode,
and backup SOC. These write plant unit 247, regardless of `--unit-id`, and can
affect other inverters in the same plant. `--unit-id` selects monitored hardware.
When enabling previously disabled remote EMS, the server first writes and
confirms standby, then enables EMS and confirms both settings. It does not
resume a previously stored forced charge/discharge or PCS dispatch mode.
An enable request while EMS is already enabled preserves the current mode.
Mode changes require remote EMS to be enabled. Disabling it returns control to
the device's energy management. Charging/discharging modes use existing device
limits; this interface does not set charging or discharging power limits.
PCS mode activates existing dispatch settings. Verify these device settings
before selecting a mode. The server must be the only external Modbus controller;
read/write/readback sequences cannot prevent another controller changing them.

| Environment variable | Default | Purpose |
| --- | --- | --- |
| `SIGEN_ADDRESS` | required | Modbus IP:port, e.g. `192.168.1.50:502` |
| `SIGEN_UNIT_ID` | `1` | Configured inverter ID, 1–246 |
| `SIGEN_BIND` | `127.0.0.1:3000` | HTTP listening address |
| `SIGEN_POLL_SECONDS` | `15` | Delay after each complete poll, 1–3600 seconds |
| `SIGEN_READ_FUNCTION` | `4` | Telemetry read function, 3 or 4; see the Modbus crate's function-code notes |

Each poll takes about 12–13 seconds because Modbus requests must be spaced one
second apart. The dashboard refreshes the cached state every three seconds.
Scrapes and status requests don't trigger hardware traffic. The first complete
reading appears after about 13 seconds. Polls are not atomic snapshots.

Optional dashboard groups are read one request at a time during the gap between
core polls. Energy groups refresh at most once every five minutes; electrical
groups at most once every minute. They share the existing paced connection and
only run when there is enough time before the next core poll for request pacing
and a complete response. Very short `--poll-seconds` settings can leave no time
for optional reads. Queued commands are checked before starting another optional
read; an already-started read completes before a new command is processed.
Unsupported groups (Modbus exceptions 1–3) are marked unavailable until restart
without failing the core poll. Other device exceptions are retried on the next
optional interval; transport/protocol failures invalidate controls and reconnect.
An offline, failed or old optional sample is labeled **Last known**, independently
of the core sample age. Daily energy older than ten minutes and electrical data
older than two minutes use that label even while the core connection is healthy.
The `/api/status` response exposes these groups in `details`, each with `value`,
`updated_unix`, `device_day`, `last_error` and `unsupported`. Grid counters are
exact decimal strings in 0.01 kWh units to preserve U64 precision in browsers.
These groups also appear in MQTT status and Prometheus metrics, using their own
freshness limits. Home Assistant discovery retains its existing entities.

A dedicated thread owns one paced Modbus connection. Polling and writes are
serialized; HTTP stays responsive while hardware requests run. Disconnections
are retried on the next poll. Control requests use a bounded queue and are never
automatically retried. Success means the write's setting matched readback;
errors or a 60-second HTTP timeout may mean a write already took effect. Check
readback before issuing another command. Invalid enable/mode/SOC readbacks fail
closed, and mode confirmation also requires remote EMS to remain enabled.
Requests have a monotonic deadline, checked after Modbus pacing immediately
before each write. Expired/cancelled requests, stale hardware, and shutdown
prevent further writes. Poll/control failures invalidate requests already
queued, even if the hardware later reconnects. A write already being transmitted
cannot be cancelled or undone. There is no automatic rollback or shutdown write.
On reconnect, the inverter serial number must match the initially observed
identity; a mismatch blocks polling and controls until the operator verifies
the target and restarts the server.

Cached readings remain visible with an offline/stale label after polling fails.
Hardware measurements are omitted from metrics when offline or stale;
`sigen_up` becomes zero. HTTP, MQTT and control checks also use a monotonic
freshness deadline (polling delay + 30 seconds), so moving the system clock
backward cannot keep old data available indefinitely.
Controls require an initial successful complete poll. Older firmware lacking
any required register may fail the complete poll; check the Modbus crate's
supported register ranges. PV-only inverters are not supported by this dashboard.

The server defaults to localhost and has no user authentication. For remote use,
place it behind an authenticated HTTPS reverse proxy or use a trusted private
network. Mutations require JSON and `X-Sigen-Control: 1`; browser cross-origin
writes are rejected. No CORS is enabled. The proxy should preserve the browser's
Host header. To prevent DNS rebinding, mutations only accept IP-literal or
`localhost` Host names by default. For a trusted local hostname or reverse
proxy, allow its exact Host authority, including any non-default port:
`--allowed-host sigen.example:8443`. Repeat the option or set
`SIGEN_ALLOWED_HOSTS=sigen.example:8443,sigen.home:3000`. Do not allow untrusted
domains. This check is not authentication. Hardware data is rendered as text
and all assets are served locally.

This is monitoring and supervisory control software, not a safety system.
Tests use simulated hardware; register readback does not prove physical output
or protection behavior. Manufacturer protections, qualified installation,
and controlled commissioning on the exact model/firmware remain necessary.
See [the code safety review](../../SAFETY_REVIEW.md) for changes and limitations.

## HTTP API

- `GET /`: dashboard, with `/app.js` and `/style.css` assets.
- `GET /api/status`: cached readings, control readback, connectivity and timestamps.
- `GET /api/events`: persistent event history, with optional category and cursor filters.
- `POST /api/control`: one of the following JSON commands. Returns confirmed
  control readback, or an error; never redirects or retries.
- `GET /metrics`: Prometheus text exposition, version 0.0.4.

```sh
curl http://127.0.0.1:3000/api/status
curl -X POST http://127.0.0.1:3000/api/control \
  -H 'Content-Type: application/json' -H 'X-Sigen-Control: 1' \
  -d '{"action":"remote_ems","enabled":true}'
curl -X POST http://127.0.0.1:3000/api/control \
  -H 'Content-Type: application/json' -H 'X-Sigen-Control: 1' \
  -d '{"action":"ems_mode","mode":2}'
curl -X POST http://127.0.0.1:3000/api/control \
  -H 'Content-Type: application/json' -H 'X-Sigen-Control: 1' \
  -d '{"action":"backup_soc","percent":20.0}'
```

EMS modes: 0 PCS remote control, 1 standby, 2 maximum self-consumption,
3 charge/grid first, 4 charge/PV first, 5 discharge/PV first, 6 discharge/ESS first.
Backup reserve accepts 0–100%, rounded to 0.1%.

## Prometheus

```yaml
scrape_configs:
  - job_name: sigen
    scrape_interval: 15s
    static_configs:
      - targets: ['127.0.0.1:3000']
```

Use the server's reachable address if Prometheus runs elsewhere. Metrics include
`sigen_up`, `sigen_poll_errors_total`, `sigen_control_errors_total`,
`sigen_last_success_timestamp_seconds`, `sigen_sample_age_seconds`,
`sigen_grid_power_watts`, `sigen_pv_power_watts`, `sigen_battery_power_watts`,
`sigen_battery_soc_percent`, `sigen_battery_soh_percent`, battery and inverter
temperatures, cell voltages, power limits, available energy, grid frequency,
EMS settings and `sigen_alarm_bits{group="1"}` through group 5. Power is in watts,
available energy in Wh, temperature in Celsius, frequency in hertz, and SOC/SOH
in percent (0–100). Positive grid power means import; positive plant battery power
means charging. Metrics carry HELP and TYPE metadata.

Optional telemetry adds the following metrics when each sample is current:

| Metric | Type / units |
| --- | --- |
| `sigen_pv_daily_energy_kwh`, `sigen_load_daily_energy_kwh` | Gauges, kWh for the device-local day; inverter PV versus whole-plant load |
| `sigen_battery_daily_charge_energy_kwh`, `sigen_battery_daily_discharge_energy_kwh` | Gauges, this inverter’s battery daily kWh |
| `sigen_grid_import_energy_kwh_total`, `sigen_grid_export_energy_kwh_total` | Counters, whole-plant cumulative kWh using the new statistics interface |
| `sigen_inverter_phase_voltage_volts{phase="a"}`, `sigen_inverter_phase_current_amps{phase="a"}` | Gauges, volts/amps; phases `a`, `b`, `c` where inverter wiring makes the readings meaningful |
| `sigen_grid_phase_power_watts{phase="a"}` | Gauge, watts; phases `a`, `b`, `c`, positive import and negative export, from the plant grid sensor |
| `sigen_inverter_output_type`, `sigen_inverter_power_factor` | Gauges, raw wiring type and dimensionless power factor |

Daily totals are gauges because they reset at local midnight. Cumulative counters
can reset when firmware introduces the new statistics interface. Grid energy is
formatted from the exact integer register value; Prometheus stores floating-point
samples, so very large totals can lose precision there. Optional values are omitted
when unsupported, failed, stale, from the previous day, or when core hardware is
offline/stale. Energy sample freshness is ten minutes; electrical freshness is
two minutes. Grid phase metrics require a connected grid sensor independently
of the inverter's phase count. Scrapes never trigger additional Modbus traffic.

## Verification

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Tests cover HTTP/assets, validation, cross-origin rejection, offline behavior,
queue overload, worker confirmation/errors, actual Modbus write/readback,
telemetry unit conversion, and Prometheus output using a local mock inverter.
No live hardware is required.

## MQTT and Home Assistant

MQTT is optional. Set `SIGEN_MQTT_HOST` to enable it:

```sh
SIGEN_ADDRESS=192.168.1.50:502 \
SIGEN_MQTT_HOST=192.168.1.10 \
SIGEN_MQTT_ID=house_battery \
SIGEN_MQTT_USERNAME=sigen \
SIGEN_MQTT_PASSWORD='your-broker-password' \
cargo run -p sigen-server
```

Add Home Assistant's **MQTT integration** using the same broker. With MQTT
discovery enabled (the default), a **Sigen house_battery** device appears with
34 sensors and binary sensors for power, charge, battery health, temperature,
voltage, available energy, grid connectivity, running states, alarm groups and alerts.
Device model, serial number and firmware are added once read from the inverter.
No Home Assistant YAML sensor definitions are needed. This uses Home Assistant's
[device discovery format](https://www.home-assistant.io/integrations/mqtt/#mqtt-discovery).

Set `SIGEN_MQTT_CONTROLS=true` to additionally discover a remote EMS switch,
EMS mode selector, and backup reserve number. Otherwise no MQTT command topic
is subscribed to, and existing control entities are removed from discovery.
Commands use the same bounded, serialized hardware queue as the web interface.
State changes are published from confirmed hardware readback, never optimistically.
The mode selector is unavailable until remote EMS is enabled.

| Environment variable | Default | Purpose |
| --- | --- | --- |
| `SIGEN_MQTT_HOST` | unset (disabled) | Broker hostname or IP, without a scheme or port |
| `SIGEN_MQTT_PORT` | `1883`, or `8883` with TLS | Broker TCP port |
| `SIGEN_MQTT_ID` | Derived from Modbus endpoint and unit ID | Stable MQTT client, device and entity identifier; letters, digits, `_`, `-` |
| `SIGEN_MQTT_USERNAME` | unset | Broker username |
| `SIGEN_MQTT_PASSWORD` | unset | Broker password; requires a username |
| `SIGEN_MQTT_TLS` | `false` | `true` enables certificate-verified TLS |
| `SIGEN_MQTT_CA_FILE` | system trust roots | PEM CA file for a private TLS broker; requires TLS |
| `SIGEN_MQTT_TOPIC_PREFIX` | `sigen/<id>` | Root topic for this server/device |
| `SIGEN_MQTT_DISCOVERY_PREFIX` | `homeassistant` | Match Home Assistant's discovery prefix |
| `SIGEN_MQTT_HA_STATUS_TOPIC` | `homeassistant/status` | Match Home Assistant's birth/status topic |
| `SIGEN_MQTT_CONTROLS` | `false` | Publish and accept Home Assistant controls |

Use a fixed `SIGEN_MQTT_ID` before first discovery, so DHCP/address changes do
not create a second device. Each server instance needs a distinct ID **and**
topic prefix. If you change an ID, remove its old device and retained discovery
configuration at `homeassistant/device/<old-id>/config` (publish an empty retained
payload) to avoid leaving duplicate entities.

For a TLS broker using a private CA:

```sh
SIGEN_MQTT_HOST=mqtt.example.net \
SIGEN_MQTT_TLS=true SIGEN_MQTT_CA_FILE=/path/to/ca.pem \
SIGEN_ADDRESS=192.168.1.50:502 cargo run -p sigen-server
```

The certificate must match the configured broker hostname. Configure broker
credentials and ACLs for the selected topics, especially if MQTT controls are
enabled. The broker account needs write access to this device's topic prefix
and its discovery topic; read access to the HA status topic and, when controls
are enabled, `<prefix>/command/+`.

The bridge republishes cached state every five seconds, without additional
Modbus reads. Discovery and state are retained; discovery is resent after broker
reconnects and Home Assistant's `online` birth message. The broker connection
retries every five seconds without interrupting HTTP or Modbus. Dashboard status,
`GET /api/status` and the `sigen_mqtt_connected` Prometheus gauge expose MQTT
connectivity. Credentials are excluded from status and logs.

The retained `<prefix>/availability` topic is `online` while the bridge is
connected. A retained `offline` last will covers crashes/lost connections; normal
shutdown publishes `offline` before disconnecting. Entity availability also
requires a successful hardware poll that is no more than the configured polling
delay plus 30 seconds old. Offline/stale state publishes null readings and
controls and `details`. While core hardware is fresh, `details` contains the same
seven optional groups as `/api/status`, with each group's `value` set to null if
it is unsupported, failed, older than its freshness limit, or (for daily totals)
from a previous device-local day. Sample timestamps and diagnostic fields remain
available. Grid phase power is null when the grid sensor is disconnected. Grid
energy values remain exact decimal strings in 0.01 kWh units. `device_day` is
included at the top level for interpreting daily samples. Numeric discovery sensors additionally expire after 90 seconds without a state
message, as described in the [MQTT sensor documentation](https://www.home-assistant.io/integrations/sensor.mqtt/).

Topics with the default prefix and `SIGEN_MQTT_ID=house_battery`:

| Topic | Payload | Retained |
| --- | --- | --- |
| `homeassistant/device/house_battery/config` | Home Assistant device discovery JSON | yes |
| `sigen/house_battery/state` | JSON with `online`, `last_success_unix`, `readings`, `details`, `device_day`, `controls`, `alerts`, `target`, `unit` | yes |
| `sigen/house_battery/availability` | `online` / `offline` | yes |
| `sigen/house_battery/command/remote_ems` | `ON` / `OFF` | **never** |
| `sigen/house_battery/command/ems_mode` | An exact option name from the discovery document | **never** |
| `sigen/house_battery/command/backup_soc` | A number from 0 to 100 | **never** |
| `sigen/house_battery/command_result` | JSON with `confirmed` and `controls` or `error` | no |

Only one MQTT control runs at a time. MQTT commands use QoS 0 and clean sessions
to avoid queued command delivery after reconnects. Retained replays, DUP-flagged
commands, invalid values, and commands while offline/stale are rejected. Never
retain control messages. A live retained publish can still be delivered as a
normal message by MQTT brokers; its retained replay on a later subscription is
rejected. Controls are never automatically retried. Failed confirmation or a
60-second timeout can mean the write took effect; check readback before retrying.

Available energy sensors are instantaneous stored/remaining energy amounts,
not lifetime energy totals for Home Assistant's Energy dashboard. Power sensors
can be integrated over time in Home Assistant if cumulative energy is needed.
Tests exercise an MQTT protocol mock broker (discovery, state, commands,
Home Assistant birth, reconnect and shutdown). A live broker/Home Assistant
installation has not been used for validation.

## Alerts

The web UI has five independent on/off toggles, enabled by default:

| Alert | Trigger | Clears when |
| --- | --- | --- |
| Hardware offline or stale | Failed poll, or no successful reading within polling delay + 30 seconds | A fresh poll succeeds |
| Inverter fault | Plant/inverter reports Fault or Environmental abnormality, or either PCS alarm word is nonzero | Fresh readings report recovery and clear PCS alarms |
| Battery alarm | ESS alarm group 3 has any bit set | Fresh readings report no ESS alarms |
| Grid disconnected | Plant reports automatic or manual off-grid operation | Fresh readings report on-grid operation |
| Control failed | A hardware command fails or readback does not match | A later hardware command succeeds |

No thresholds or notification destinations need configuring. Toggle off the grid
alert for an intentionally off-grid installation. During initial startup the
connection alert waits for the first failed poll or the normal stale timeout.
An active hardware fault stays marked as last-known when readings become stale;
it only clears on fresh recovery data or when its toggle is disabled.

Toggle choices are saved atomically to `sigen-alerts.json` in the working
directory. `SIGEN_ALERTS_FILE` can select another file in an existing writable
directory. A malformed settings file fails startup rather than resetting choices.
Failed saves leave the previous settings in effect. Current alert state and
activation timestamps are available in `GET /api/status`. Toggle via
`POST /api/alerts` with `{"id":"grid_disconnected","enabled":false}` and the
same JSON and `X-Sigen-Control: 1` headers as other mutations; hardware may be
offline when changing alert settings.

Five Home Assistant MQTT problem binary sensors mirror these alerts, including
when MQTT hardware controls are disabled. Disabling an alert makes its sensor
inactive. The offline alert remains available while hardware is unreachable;
other hardware-specific alerts become unavailable when their state is unknown.
Prometheus exposes `sigen_alert_enabled`, `sigen_alert_active`, and
`sigen_alert_known`, labelled by `alert` ID. Alerts are shown locally and shared
with Home Assistant/Prometheus. Optional Pushover notifications are supported
as described below; other phone/email delivery uses Home Assistant automations
or Alertmanager receivers.

## Logging

The server initializes `tracing_subscriber` once at startup. Structured tracing
logs go to stderr; the default level is `info`. Dependencies using the `log`
facade are bridged through the subscriber as well. Configure filtering with
`RUST_LOG`, for example:

```sh
RUST_LOG=sigen_server=debug,sigen_modbus=trace \
SIGEN_ADDRESS=192.168.1.50:502 cargo run -p sigen-server
```

Info events cover startup/shutdown, connection recovery, confirmed controls and
saved alert settings. Warnings cover hardware/MQTT failures and active alerts.
Repeated identical hardware failures move to debug to limit noise. Debug HTTP
spans contain method, path, status and duration; Modbus trace spans include unit,
transaction and function IDs. HTTP headers, query strings, bodies, MQTT
credentials and raw MQTT packets are not recorded by server instrumentation.
Use crate-specific filters when diagnosing MQTT to avoid enabling dependency
packet dumps. The Modbus library uses the application's subscriber rather than
installing its own.

## Pushover notifications

Set your Pushover application token and user/group key to send notifications for
the existing enabled alerts:

```sh
SIGEN_PUSHOVER_TOKEN=your_application_token \
SIGEN_PUSHOVER_USER=your_user_or_group_key \
cargo run -p sigen-server -- --address 192.168.1.50:502
```

Register an application and get your user/group key from your Pushover account.
Both must be provided; without them notifications are disabled. The integration
uses Pushover's [Message API](https://pushover.net/api) with verified HTTPS,
severity-based priority and the default notification sound. It never sends an automatic
test message at startup. The existing alert toggles control which alerts send
notifications; no thresholds or extra per-alert settings are needed.

All active alerts use at least **high priority (`1`)**, including hardware
offline/stale, inverter faults, battery alarms, grid disconnection and failed
controls. Explicit physical hazard alarm bits use **critical priority**, mapped
to Pushover's **Emergency (`2`)**:

| Alert | Critical conditions (Sigenergy Modbus V2.7) |
| --- | --- |
| Inverter fault | PCS group 1 bits 1, 2, 5, 6, 7, 8, 15: insulation failure, overheating, PV overvoltage/reversal/back-feed, AFCI fault, excessive DC output current; PCS group 2 bits 0, 8: excessive leakage current, short circuit to PE |
| Battery alarm | ESS group 3 bits 2, 6: overheating, thermal runaway |

This physical-hazard classification uses the alarm descriptions in
[V2.7 appendices 2–4](https://www.sigenergy.com/uploads/us_download/1755488219226583.pdf).
The manufacturer's “Critical” severity also covers operational faults such as
software version mismatch; those remain high priority here. Unknown bits also
use high priority. Gateway and charger words do not trigger these existing alerts.

Emergency notifications repeat every 60 seconds until acknowledged, for up to
30 minutes. Pushover handles these repeats independently of server delivery
retries. Recovery messages use **normal priority (`0`)**. Recovery or disabling
an alert does not cancel an emergency already accepted by Pushover; acknowledge
it in the Pushover app, or it expires after 30 minutes.

| Option | Environment variable | Default |
| --- | --- | --- |
| `--pushover-token` | `SIGEN_PUSHOVER_TOKEN` | unset |
| `--pushover-user` | `SIGEN_PUSHOVER_USER` | unset |
| `--pushover-device` | `SIGEN_PUSHOVER_DEVICE` | all user devices |
| `--pushover-recovery=false` | `SIGEN_PUSHOVER_RECOVERY=false` | recovery messages enabled |

Prefer environment variables for credentials. Tokens and keys are hidden from
help, status, and application logs. The optional device selects one named device;
Pushover's group delivery rules still apply. A notification includes the alert
name, activation/recovery, inverter ID, Modbus endpoint and event timestamp.

A notification is queued once on activation, once on escalation from high to
critical, and once on confirmed recovery. The highest severity stays latched
until confirmed recovery, preventing repeated emergency messages if bits fluctuate.
Repeated polls don't resend it, and stale readings don't generate recovery.
Disabling an alert suppresses queued deliveries and does not send a misleading
recovery message. Active conditions are evaluated again after server restart
and may notify again. Brief transitions are captured by the alert evaluator,
including control failures, rather than sampled by the delivery worker.

Delivery runs asynchronously, sequentially, using a bounded queue of 32 messages.
Requests have a 5-second connection timeout and 10-second total timeout. Temporary
network/API failures retry up to three total attempts, at least five seconds
apart. Rejections (HTTP 4xx, including quota errors, or API status other than 1)
are not retried. A lost response can cause a duplicate notification on retry.
The queue is in memory: overflow, shutdown, or process failure can drop pending
notifications. A toggle suppresses delivery when the worker next checks it;
an HTTP request already in flight may still complete.

The dashboard and `/api/status` show Pushover delivery status and the most recent
error. Prometheus exports `sigen_pushover_sent_total`,
`sigen_pushover_failed_total`, and
`sigen_pushover_last_success_timestamp_seconds`. Sent means Pushover accepted the
message, not proof that a phone displayed it. These metrics do not depend on
hardware connectivity. Delivery failures do not stop hardware polling or HTTP.

Tests use a local mock API for request encoding, acceptance/rejection, retry
limits, recovery suppression, and state/credential handling. No real Pushover
notifications are sent by tests.

## Event journal

The **Event journal** panel keeps a persistent timeline of server starts and
graceful shutdowns, hardware connection loss/recovery, alert activation/escalation/
recovery, alert toggle changes, and accepted control requests and their outcomes.
Repeated poll failures and unchanged alerts do not add rows. Disabling an active
alert is recorded as disabling, not hardware recovery. Outage durations cover
observations within the running process; no events can be observed while it is down.

Events live in `sigen-events.sqlite3` in the working directory. Override with
`--journal-file /path/to/events.sqlite3` or `SIGEN_JOURNAL_FILE`. The parent directory
must exist. New databases have mode 0600 on Unix. Containers keep the default file
under `/data`; the NixOS module uses `/var/lib/sigen-server/events.sqlite3`.
History is retained indefinitely; no telemetry samples are stored here and
Prometheus remains the measurement-history store. Reopening a journal never
restores live controls, hardware readiness, or active alert state.

Expand the panel to load the newest 25 events, filter by category, and use
**Older events** to page back. **Newest events** refreshes history explicitly so
regular status polling does not disturb a page being read. Demo mode hides the
journal and never fetches live event history. Structured details include the
observed device serial when known and a process session identifier. Control
request IDs are included in both journal details and write audit sources, linking
the command to its actual wire transmissions. Malformed requests and requests
rejected before a hardware job can be created are not journaled. An accepted
request without a terminal outcome after a crash has an unknown outcome.

`GET /api/events?category=control&limit=25&before=123` returns an object with
`events`, `next_before`, and `health`. All parameters are optional. Categories
are `server`, `connectivity`, `alert`, `control`, and `settings`; limit defaults
to 50 and must be 1–100. Events are newest first by insertion ID. Pass
`next_before` as `before` for the next page; a null cursor means the end. IDs
keep pagination stable across new arrivals and wall-clock changes. Each event
contains `id`, `timestamp_unix_ms`, `session`, `target`, `unit`, `serial_number`,
`category`, `kind`, and structured `details`. Like the other read APIs, this
endpoint has no built-in authentication.

SQLite runs on a dedicated thread with WAL and full synchronization. A bounded
queue keeps disk I/O out of hardware and HTTP workers. Database open/schema errors
fail startup; runtime write failures or queue overflow drop the affected events
and report errors without stopping hardware controls. `/api/status` exposes
`journal` health, and Prometheus exports `sigen_journal_written_total`,
`sigen_journal_dropped_total`, and `sigen_journal_errors`. Counters reset on
restart; the error remains visible for the rest of the process even after writes
recover. The journal supplements the synchronous, fail-closed Modbus audit below.

Graceful shutdown waits for the hardware worker, drains accepted journal entries,
and commits a final shutdown event. A crash or power loss can lose events still
in the memory queue, and cannot produce a graceful-shutdown record. To back up,
stop the server cleanly before copying the database, or use SQLite's online backup
facility while it is running. Copying only the main file while running can miss
events still in the WAL. Keep backups on persistent storage along with the audit.

### Query from the command line

`sigen-journal` reads an existing journal directly, whether the server is running
or stopped. It opens SQLite read-only and never creates or migrates the database.
It uses the same `SIGEN_JOURNAL_FILE` environment variable and default path as
the server. An explicit `--journal-file` overrides the environment.

```sh
cargo run -p sigen-server --bin sigen-journal -- --limit 20
sigen-journal --journal-file /var/lib/sigen-server/events.sqlite3 --category alert
sigen-journal --category control --before 123 --limit 50
sigen-journal --json
```

Results are newest first, with UTC timestamps, event IDs, category, kind, target,
unit, and structured details. The default limit is 50 (maximum 100). When more
events exist, text output prints the `--before` cursor for the next page. `--json`
prints one JSON object per line (JSONL), including all event fields; no matches
produce no output. Categories match the
HTTP API. The tool needs permission to read the journal and its SQLite sidecars.
With Nix, use `nix run .#sigen-journal -- --journal-file /path/to/events.sqlite3`.
The container also includes `/sigen-journal`; select it with
`--entrypoint /sigen-journal` and mount the same `/data` volume.

## Modbus write audit

Every server hardware write is logged to `sigen-writes.jsonl` in the working
 directory by default, independently of console log filtering. Select a path
with `--write-audit-file /path/to/writes.jsonl` or `SIGEN_WRITE_AUDIT_FILE`.
The parent directory must exist. The file is opened for append at startup;
existing records are preserved. New files have mode 0600 on Unix.
Every successful server startup appends a `server_started` record before the
hardware worker starts, even if no hardware writes occur. It includes the
timestamp, process ID, server version, bound HTTP URL, Modbus target and
monitored inverter unit. Failure to persist this record prevents startup.

Each JSON line includes Unix milliseconds, process ID, target IP/port, Modbus
unit and transaction IDs, function, register address, every raw register value,
the complete TCP frame in hexadecimal, and command source. HTTP records include
the route and TCP peer address; MQTT records include the topic and bridge ID.
The originating requested operation is included, so preparatory standby writes
can be associated with the enable request that caused them. HTTP peers may be
reverse proxies; MQTT does not identify the original publisher. These fields
identify the input path, not an authenticated person.

`attempt` is persisted before transmission, followed by a final cancellation
check. It can therefore appear for a cancelled request that sent no bytes.
`sent` means the complete frame was accepted by the local TCP stack, not proof
that hardware applied the command. `send_failed` means transmission failed and
may have been partial. Missing terminal records after a crash leave the outcome
unknown. Both single-register and multiple-register writes use this audit path.
Reads are excluded. Writes also emit debug-level events under the tracing target
`sigen_modbus::write_audit`; enable them with
`RUST_LOG=info,sigen_modbus::write_audit=debug`.

Records are written and synchronized before hardware transmission. Failure to
persist the attempt blocks that write. Failure recording a completed send
returns an error with an uncertain hardware outcome; writes are never retried.
Logging is synchronous on the dedicated hardware thread. The file grows by
appending; manage retention externally. After renaming the file for rotation,
restart the server to reopen the configured path.
