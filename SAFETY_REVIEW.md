# Hardware control review — 2026-10-28

The review covered Modbus framing, typed register operations, the hardware worker,
HTTP and MQTT command submission, shutdown, cached status, and alert delivery.
No real inverter was contacted or controlled. This review and the mock tests
do not establish that the system is safe for unattended or safety-critical use.

The reference was the supplied [Sigenergy Modbus Protocol V2.7](https://www.sigenergy.com/uploads/us_download/1755488219226583.pdf),
especially sections 4.2, 5.2, 5.3 and 6, and appendices 1–6. The three exposed
controls use plant unit 247: enable 40029, mode 40031 and backup SOC 40046.
Enable values are 0/1, modes 0–6 and SOC uses tenths of a percent. The document
contradicts itself about telemetry function codes; the existing configurable
telemetry choice remains. Both relevant sections specify FC04 for control
readback. There is no automatic function-code fallback.

## Corrected findings

| Finding | Correction and regression coverage |
| --- | --- |
| An enable readback other than 1 became `false`, allowing values such as 65535 to falsely confirm a disable request. Mode and reserve readbacks were also unchecked. | Reject all out-of-domain control readbacks before using them. Tests inject invalid values both before and after writes. |
| A mode write could be reported successful when its mode matched but remote EMS had become disabled. | Confirmation requires both the requested mode and enabled EMS. A simulated external change verifies rejection. |
| Enabling remote EMS could silently activate previously stored forced charge/discharge or PCS dispatch settings. | When transitioning from disabled, write and verify standby before enabling; verify standby again after enable. Failure stops the sequence without rollback or retry. An already-enabled request preserves its mode. |
| Queued commands remained valid after a failed poll/control and could execute after reconnection. | Jobs capture a hardware-state generation. Failure invalidates it; reconnection cannot revive old jobs. Tests cover the real worker with multiple queued commands and explicit reconnection-state restoration. |
| Cancellation was checked only when removing a command from the queue, before pre-reads and pacing. | Check a monotonic expiry, receiver cancellation, state generation, freshness and shutdown immediately before each write, after pacing. Wire-level tests prove no cancelled write is transmitted. |
| Shutdown did not inhibit the hardware worker's queued writes. | Shutdown immediately invalidates controls and marks the worker stopping. The worker and final write check reject further commands. Already-started writes remain potentially applied. |
| HTTP accepted controls based only on `online`, while MQTT used stronger freshness checks. Cached HTTP/Prometheus data could remain available after stalling. | Shared monotonic freshness and readiness checks protect both command sources and hardware execution. HTTP and Prometheus also mark stale state unavailable. |
| Failed controls left the hardware marked online; reconnecting could bypass minimum request pacing. | Failures clear controls, mark offline, invalidate queued jobs and delay reconnection by the configured request interval. |
| Device identification was cached forever across reconnects. | Read identity on reconnect and reject a changed serial number, retaining the original identity until restart. This is consistency checking, not cryptographic authentication. |
| Origin equality with an attacker-controlled Host allowed browser DNS rebinding to reach hardware mutations. | Mutation hosts must be IP literals, localhost, or explicitly trusted exact authorities. Tests reject an attacker hostname even when its Origin matches. Custom hostnames need `--allowed-host`. |
| Controls looked specific to the monitored inverter although they affect the plant. | UI, Home Assistant entity names and documentation now identify plant controls. |
| PCS alarm words could be nonzero without activating the inverter-fault alert if the state register still said Running. | Either PCS alarm group now activates that alert. Tests cover each group independently. |
| An early hardware failure could transition alerts before the Pushover sender existed. | Configure notification delivery before starting the hardware worker; evaluate alerts immediately after each completed poll. |
| An extreme connection timeout was not checked for representability. | Validate it before attempting any connection. |

## Validation

Regression tests use local simulated Modbus, MQTT and notification services.
They exercise every exposed mode, reserve endpoints and rounding, plant unit
addressing, write echoes, malformed/truncated replies, connection loss, failed
confirmation, stale data, queue invalidation, cancellation, shutdown checks,
identity changes and web request rejection. The full workspace test suite and
Clippy with warnings denied pass. Formatting and JavaScript syntax are checked.

## Remaining operational limits

- This is supervisory software, not an emergency stop, certified battery
  protection system or guaranteed alarm channel. It must not replace the
  manufacturer's protection systems or qualified installation and commissioning.
- Validation used mocks. Confirm firmware compatibility and actual physical
  behavior on the exact installed model under a qualified commissioning plan,
  including standby preparation while EMS is disabled. Rejection of that step
  stops activation; the software must not bypass it by enabling first.
- Registers are read sequentially. A readback is evidence of a register value,
  not proof of electrical output, durable persistence, or a coherent atomic
  snapshot. Use one external controller per plant; competing controllers can
  change registers between reads and writes.
- The server does not configure or independently validate all plant dispatch,
  charge/discharge, grid, or protection limits. Modes use the existing settings.
  Standby can interrupt normal energy operation; it is not asserted to be a safe
  shutdown or electrical isolation for every installation.
- Polling and notification latency, process/power/network loss, queue overflow
  and failed API delivery can delay or lose alerts. Monitoring covers the
  selected inverter/battery and plant aggregates, not every inverter's alarms.
- Failed readback or a lost response can leave a write applied. There is no
  automatic retry, rollback, emergency write, or assumed firmware watchdog on
  disconnection/shutdown. Check state before issuing a new command.
- HTTP has no built-in authentication and Modbus has no authenticated device
  identity. Use network isolation and authenticated HTTPS access; restrict MQTT
  credentials and command-topic permissions. Host checks are not authentication.
- The Modbus library also exposes low-level writes. Its ordinary methods do not
  inherit the server's control policy; other applications must implement their
  own validation, cancellation and confirmation.
