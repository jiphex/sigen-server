# sigen-modbus

Blocking Rust client for Sigenergy Modbus TCP, based on
[Sigenergy Modbus Protocol V2.7, released 2025-05-23](https://www.sigenergy.com/uploads/us_download/1755488219226583.pdf).
Register addresses, types and gains come from sections 5.1–5.4; framing and
exceptions from section 6. Multiword values use the most significant word first,
consistent with the document's examples. This is a subset of V2.7, not a claim
of support for every model or register in the document.

```rust,no_run
use sigen_modbus::{Client, UnitId};

let mut client = Client::connect("192.168.1.50:502".parse()?)?;
let inverter = UnitId::inverter(1)?; // use the actual ID configured in mySigen
let info = client.read_device_info(inverter)?;
let plant = client.read_plant()?;
let battery = client.read_battery(inverter)?;
tracing::info!(model = %info.model, soc_percent = battery.soc_percent, "Battery status");
# Ok::<(), Box<dyn std::error::Error>>(())
```

Enable the device's Modbus TCP server before connecting. Port 502 is the default;
the endpoint is explicit so another port can be used. Plant data uses unit 247;
inverter data uses the configured ID in 1–246. Battery data describes the ESS
cluster attached to an inverter, rather than individual packs. Hybrid-only
battery registers must not be requested from PV-only models. Device exceptions
are surfaced to the caller, including unsupported registers on older firmware.

Supported typed operations:

- Plant time, timezone, EMS/grid status, SOC, grid/plant/PV/battery power and state
  (30000–30014, 30031–30038, 30051).
- Inverter identification (30500–30545), state and power (30578, 30587–30590),
  frequency/temperature (31002–31003) and PV power (31035–31036).
- Battery power/energy limits, SOC/SOH, cell temperature/voltage, alarm bitfields
  (30591–30609), and temperature/voltage extrema (30620–30623).
- Exact U64 energy counters via `read_energy_counter`, returned in 0.01 kWh units.
- Explicit remote EMS enable/mode and backup SOC writes (40029, 40031, 40046).
  Enabling remote EMS changes plant operating mode; mode selection and limits
  have the preconditions described in section 5.2. No writes occur during reads.

`read_registers`, `write_register` (0x06) and `write_registers` (0x10) provide
access to other documented registers. Addresses are sent literally: **30000
means address 30000**, with no Modbus reference-number subtraction. Raw writes
require the caller to check permissions, ranges, applicable models and EMS mode.
For multiword writes, pass the high word first. Read helpers divide by each
register's documented gain. Positive grid power means import; negative means
export. Plant battery power is positive while charging and negative while
discharging. The inverter battery power field is exposed with its raw signed
convention; V2.7 does not explicitly define its sign in that table.

## Function-code discrepancy

V2.7 section 4.1 says RO registers use **0x04**, while section 6.1 and its
examples say RO uses **0x03** and holding reads use **0x04**. The client defaults
to 0x04 for telemetry, following section 4.1. Set
`Config::telemetry_function = ReadFunction::Holding` to use 0x03 if required by
your firmware. Raw reads always use the explicitly supplied function. No
fallback request is sent automatically.

## Connection behavior

Requests are serialized and spaced at least one second apart (section 4.2).
Defaults are one second for connecting and one second for a complete response;
`Config` allows longer timeouts and request intervals. Use one client per plant
so multiple connections don't defeat pacing. Snapshots span multiple requests
and are not atomic. Methods block; use a blocking worker in async servers.

The client validates transaction/protocol/unit/function IDs, lengths, byte
counts and write echoes. I/O or malformed response errors close the connection;
create a new client to reconnect, preventing late responses from contaminating
later requests. Device exception responses leave the connection usable.
Requests are never retried. A failed write response does not prove the write
was unapplied; read back state before deciding whether to issue another write.
Broadcast unit 0 is excluded because it cannot acknowledge commands.

`write_register_checked` evaluates a caller-supplied check after request pacing,
immediately before transmission. A rejected check sends no request. This is
used by the server for cancellation, expiry and hardware-state checks. It cannot
undo a write once transmission begins. The library's ordinary typed/raw write
methods do not implement the server's standby preparation, readback verification,
identity checks or command invalidation; callers must supply those policies.

Run the read-only example with:

```sh
cargo run -p sigen-modbus --example monitor -- 192.168.1.50:502 1
```

Tests use a local mock TCP server and exercise on-wire frames, scaling, errors
and pacing. No live inverter has been used for verification.

## Tracing

The library emits structured `tracing` spans and events; it does not install a
subscriber. Applications configure their own subscriber. The `monitor` example
initializes `tracing_subscriber` with logs on stderr and `RUST_LOG` filtering
(default `info`). Use `RUST_LOG=sigen_modbus=trace,monitor=info` for request IDs,
unit IDs, function codes, durations and protocol errors. Register operations and
typed reads appear at debug level; individual request/response summaries at
trace level. Raw TCP payloads are not logged.

All FC06 and FC10 transmissions emit structured debug events on
`sigen_modbus::write_audit`, including the peer endpoint, unit, transaction,
registers, exact frame and source. Standalone library clients default to source
`library`; applications can call `set_write_source` and attach a `WriteAuditor`
using `set_write_auditor` for durable records. The server installs its file
writer by default; standalone library clients do not open files automatically.
Audit phases distinguish attempts, completed sends and failed sends. A completed
send does not imply successful hardware application. The final cancellation
check runs after the attempt record is persisted. An auditor error before
transmission prevents the write; an error afterwards leaves its outcome uncertain.
