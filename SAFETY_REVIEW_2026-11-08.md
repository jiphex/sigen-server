# Protocol and application safety review — 2026-11-08

Reviewed revision: `f3774fc7` (clean working copy before this report).
Reference: [Sigenergy Modbus Protocol V2.7, 2025-05-23](https://www.sigenergy.com/uploads/us_download/1755488219226583.pdf).
Scope: all protocol sections and appendices, production TCP client, typed reads,
server controls, telemetry, alert evaluation and command ingress. This supplements
the historical `SAFETY_REVIEW.md`; its earlier test results are not current evidence.
No real hardware was contacted. This is a source review with mock regression
testing, not certification or confirmation of installed firmware behavior.

## Findings

Priorities describe application risk: P1 should be resolved before unattended
write-enabled operation; P2 is a monitoring/compatibility weakness; P3 is hardening.
Recommendations below are review conclusions, not additional manufacturer rules.

### P1 — Dispatch modes use unexamined existing settings

Evidence: `crates/sigen-server/src/hardware.rs:369`, `apply` and `read_controls`.
The server permits every mode 0–6 after checking enable state. It reads only
enable, mode and reserve. Neither PCS targets nor charge/discharge/PV limits,
grid constraints, sensor readiness, independent-phase configuration or inverter
dispatch override are checked. A successful mode echo therefore cannot establish
that resulting power flow matches the operator's intended envelope.

Trigger: request mode 0 or 3–6 on a plant previously configured by another
controller. Existing targets can become active. The server's standby-before-enable
sequence reduces one activation hazard but does not address subsequent mode changes.

Recommendation: define explicit supported operating profiles and require fresh
readback of applicable targets, ratings and configuration before accepting them.
Block profiles whose prerequisites cannot be verified. Include inverter override
state in commissioning. Do not silently overwrite grid/protection configuration.
Verification: mock mismatched, missing and out-of-range prerequisites; demonstrate
zero writes on rejection, then verify physical behavior on the installed model.

### P1 — Alarm coverage is incomplete for whole-plant monitoring

Evidence: `hardware.rs:298`, `alerts.rs:214`, `data/extended.rs:read_plant_alarms`.
Core polling reads the selected inverter's alarm words. The library's aggregate
plant alarm helper is unused. Alert conditions inspect groups 1–3; gateway and
DC-charger groups 4/5 do not independently trigger an alert. AC-charger alarms are
also outside server polling. A fault elsewhere may therefore be absent from
notifications even while monitoring remains online. A plant fault state may catch
some cases, but is not equivalent to examining the alarm words.

Recommendation: collect aggregate alarms and add gateway/charger conditions with
explicit supported coverage. Preserve unknown bits. Test each group independently,
including an otherwise-running plant and a fault on another inverter.

### P2 — Optional telemetry permanently suppresses temporary exceptions

Evidence: `crates/sigen-server/src/telemetry.rs:43` and `Poller::poll_if_due`.
Exceptions 1, 2 and 3 all set `unsupported`; that group is skipped thereafter.
Reconnection does not reset this flag. A transient device state or malformed request
can thus become a process-lifetime loss of readings rather than a recoverable error.

Recommendation: distinguish unsupported capability from transient/protocol failure,
retry conservatively, and invalidate capability decisions on firmware/device change.
Test recovery after exceptions and reconnect without restarting the application.

### P2 — Core telemetry assumes hybrid hardware and required firmware

Evidence: `hardware.rs:298`, `data.rs:read_plant`, `read_battery`.
Identity checking verifies serial continuity, not supported model or capabilities.
Battery and reserve reads are unconditional. An unsupported core register fails the
whole poll, including read-only operation. This blocks controls, which is protective,
but also removes otherwise-usable monitoring.

Recommendation: make the supported hybrid/model requirement explicit at startup,
or implement capability-aware polling. Do not infer support merely from a numeric
unit ID. Typed library helpers document several caller obligations but cannot
enforce model, string-count or charger identity themselves.

### P2 — Fresh transport data can still be invalid measurement data

Evidence: `data.rs:read_plant`, `read_battery`; `metrics.rs:135`.
Most telemetry is scaled without domain validation. SOC/SOH outside 0–100, unknown
grid enums and implausible extrema can be exported as current. Aggregate grid power
is exported even if its separate sensor-connected flag is false; optional grid
phase exports do gate on that flag.

Recommendation: expose explicit validity, preserve raw diagnostics, and suppress
invalid dependent values consistently across HTTP, MQTT and Prometheus. Use only
documented invalid sentinels; do not invent universal zero/FFFF rules. Test missing
sensors, unknown enums and invalid percentages. This finding concerns consumer
interpretation, not a demonstrated unintended write.

### P2 — Confirmation covers settings, not achieved operation

Evidence: `hardware.rs:342–427`.
The server verifies enable/mode/reserve values, but does not confirm plant EMS
work mode or achieved electrical behavior. Reads occur sequentially; competing
controllers can intervene. Shutdown and connection loss inhibit future commands
but do not reverse already-applied settings. No watchdog guarantee was identified
in the reference.

Recommendation: describe confirmation precisely, commission failure behavior, use
one controlling authority, and verify operational feedback separately. Any automatic
fallback needs its own explicit policy; adding a blind rollback is not a safe fix.

### P3 — Pacing configurability and process boundaries

Evidence: `client.rs:193,333`, `hardware.rs:188,261`, `main.rs:90`.
The library enforces at least one second and the server serializes all requests.
Server reconnect paths delay before sending again. This corrects the earlier
conversation's implication that normal server reconnects necessarily bypass pacing.
The library alone resets its timer on reconnect; separate processes do not share it.
The server exposes poll delay, not the underlying request interval or timeout.

Recommendation: expose bounded transport configuration and document a single client
owner. Increasing poll delay does not slow requests within each poll.

### P3 — Application severity differs from vendor severity

Evidence: `alerts.rs:18`.
The application intentionally promotes selected physical-hazard bits to Critical,
while other faults are High. This is not a faithful export of vendor severity.
Recommendation: retain vendor severity separately and label notification priority
as application policy. Do not imply that a lower notification priority means the
device can continue operating normally.

## Protocol coverage matrix

These are section-level dispositions, not a claim that every possible register is
implemented. Register tables were compared with typed helper ranges, widths,
signedness and gains; no additional definite decoding mismatch was identified.

| Reference | Implementation disposition |
| --- | --- |
| §§1–3, addressing/interfaces | Plant 247, devices 1–246; broadcast rejected. TCP client only. RTU and reverse-connect/4G modes are outside scope. |
| §2, model applicability | Caller-dependent library checks; core hybrid assumption: finding above. |
| §4.1, types/access | Big-endian words, signed decoding, scaling; raw APIs do not enforce per-register permissions. FC03/04 ambiguity is configurable. |
| §4.2, timing | Shared pacing and bounded response reads; poor-link tuning unavailable in CLI. |
| §4.3, severity | Application priority differs; see finding. |
| §5.1, plant telemetry | Subset implemented; exact integer energy helpers; new-counter reset documented. Aggregate alarms unused. |
| §5.2, plant settings | Only three server controls; prerequisite gap above. Global settings, phase requirements and invalid-limit sentinel need validation before future control expansion. |
| §5.3, inverter telemetry | Typed subset checked; phase availability guarded in metric export. Ambiguous PV8/9 helpers rejected. |
| §5.4, inverter controls | No server writes. Dispatch override remains an unverified external precondition. Removed grid-code example must not guide implementation. |
| §§5.5–5.6, AC charger | Read helper only; no server charger controls. Future current control requires breaker/rating checks; start/stop polarity differs from inverter. |
| §6.1, framing/functions | IDs, lengths, functions, byte counts and write echoes checked; reads capped at 124, multiwrites at 123; address overflow rejected. |
| §6.2, exceptions | Transport preserves valid exception responses; optional polling overinterprets exceptions. |
| Appendix 1 | Known states decoded; unknown values preserved. |
| Appendices 2–5, 11 | Raw words retained; notification coverage incomplete. |
| Appendix 6 | All enumerated modes accepted; prerequisite validation incomplete. |
| Appendices 7–10 | AC state/alarms preserved by library; server monitoring outside scope. |

## Existing safeguards verified in source

- Server starts read-only; library allowlists read functions at transmission boundary.
  Standalone library defaults allow writes, so callers must explicitly choose policy.
- Range validation and malformed-response retirement prevent reuse of uncertain streams.
- No automatic write retry; lost reply and failed confirmation remain uncertain outcomes.
- Commands have bounded queueing, cancellation, expiry, generation and freshness checks.
  Authorization is rechecked after pacing immediately before transmission.
- Enable preparation verifies standby; mode confirmation also requires EMS enabled.
- Shutdown invalidates queued work; reconnect cannot revive jobs from an old generation.
- Write attempts are persisted before sending; audit failure prevents that transmission.
  Audit failure after transmission can still leave a setting applied.
- HTTP rejects cross-origin/rebinding requests; MQTT rejects retained/duplicate deliveries.
  These are not authentication. HTTP has no built-in authentication, and network/broker
  access controls remain deployment requirements when enabling writes.
- Monotonic core freshness gates controls; stale cached readings remain distinguishable.

## Validation and limits

Existing tests cover pacing, framing, wrong echoes, cancellation, audit failures,
read-only rejection, lost replies, enable sequencing, queue invalidation, identity
changes, register decoding and local simulator behavior. They do not establish
manufacturer firmware compliance or independent electrical protection.

Validation completed: `cargo test --workspace` passed, including integration tests
and doctests; `cargo clippy --workspace --all-targets -- -D warnings` passed.
The initial sandboxed test run failed to start local simulator listeners; the
successful run used approved execution outside the sandbox. No physical endpoint
was used. Browser tests and live commissioning were not run.

No production code
was modified. Remediation should begin with dispatch prerequisites and alarm coverage,
then exception recovery and telemetry validity. Live commissioning must establish
standby preparation behavior, override precedence, achieved mode/power feedback and
behavior after controller/network/power failure before unattended control is relied on.


## Remediation applied after review

Dispatch modes 0 and 3–6 are blocked before I/O pending a commissioned envelope;
no automatic limit writes were introduced. Aggregate plant alarms and dedicated
gateway/DC alerts were added, with separate vendor severity. Optional exceptions
are reprobed at bounded intervals and firmware changes clear cached detail data.
Core domain checks, explicit hybrid compatibility errors, missing-sensor nulls,
transport CLI bounds and matching freshness budgets were added. EMS activation
now checks plant work mode, while success reports explicitly limit confirmation
to readback. Hardware commissioning and AC-charger monitoring remain outside scope.

Remediation validation completed 2026-11-09: 119 Rust tests/doctests and 15
JavaScript dashboard tests passed. Clippy with warnings denied, formatting and
diff whitespace checks passed. Tests used local mocks only. Forced dispatch is
disabled rather than made available with an unverified operating envelope; adding
commissioned profiles and their prerequisite readers remains future work.

## Manual closure audit — 2026-11-09

The original findings above describe the pre-remediation revision. They must not
be read as a claim that every recommendation below them is now implemented.
Manual inspection of the current source does **not** support unconditional closure.

| Finding | Current disposition | Remaining work or constraint |
| --- | --- | --- |
| Dispatch prerequisites | Mitigated by rejecting mode selection 0/3–6 in `Command::validate`, including before `apply` I/O | No commissioned profiles, prerequisite readers, override validation or physical verification. An already-enabled EMS request preserves an existing mode. Library raw/typed writes remain caller-governed. |
| Whole-plant alarms | Aggregate polling and groups 1–5 alert handling implemented in `poll`/`alerts::refresh` | AC-charger alarms are not polled. Aggregate words do not identify the individual faulting device. |
| Permanent optional suppression | Fixed: `Poller` retries on schedule; `Sample::record` clears unavailable state on success | Firmware cache invalidation occurs when identity is reread on reconnect. Recovery tests cover repeated requests, not every firmware/reconnect scenario. |
| Model/firmware assumptions | Partially addressed by documentation and explicit errors | No startup model/capability validation. Core reads remain unconditional and unsupported core registers still take all monitoring offline. |
| Measurement validity | Partially addressed: core percentages, grid enums, ordered extrema and missing grid sensor | No general field-validity representation or raw invalid-value diagnostic capture. Optional electrical output types remain unchecked and raw phase arrays remain in HTTP/MQTT even where metric exports mask absent phases. |
| Operational confirmation | Improved: activation reads plant EMS status; responses distinguish setting readback | No achieved-output verification, commissioned failure behavior or guaranteed watchdog. Sequential reads and competing-controller races remain. |
| Transport tuning | Bounded CLI settings and freshness budget implemented | Pacing is per client/process; single-controller deployment remains required. |
| Vendor severity | Separate `vendor_severity` implemented with independent notification priority | Unknown alarm bits remain unknown; classification cannot guarantee electrical safety. |

The older review's operational limits also remain: external authentication/network
controls, uncertain outcomes after lost replies, best-effort notifications and
real-hardware commissioning. The previously reported 119 Rust and 15 JavaScript
test passes validate covered software behavior; they do not close these residuals.
This audit changed documentation only and did not repeat those tests.


## Follow-up remediation — 2026-11-09

This disposition supersedes the manual closure audit above where noted.

| Residual | Action and evidence | Disposition |
| --- | --- | --- |
| Startup model compatibility | `supported_hybrid_model` gates identity before core reads on connection/reconnection; unknown/PV-only families fail explicitly. Required register reads still gate controls. | Software gate implemented; installed firmware verification open |
| Electrical validity/export mismatch | `Electrical::serialize` emits null for inapplicable phases; `Details::for_export` withholds unknown wiring with the raw type in its error. Shared export path used by HTTP/MQTT/metrics. | Implemented; tests cover every output type and unknown type |
| AC-charger coverage | Explicit bounded charger-ID configuration; paced per-device reads, raw alarm maps and AC alert/vendor severity. Configured unreadable chargers fail the poll; unconfigured coverage is unknown. | Implemented for configured units; installed IDs must be supplied |
| Dispatch prerequisites | Modes 0/3–6 remain prohibited server operations with no override. No claim of commissioned dispatch support. | Mitigated by removing operation from supported scope; profile implementation deferred |
| Physical output/failure verification | Added `COMMISSIONING.md` with evidence requirements and explicitly open installation checks. | Open: cannot be established with local code or mock devices |

Family-name matching is deliberately conservative and does not prove identity,
firmware correctness or physical safety. Unsupported firmware still fails closed;
partial monitoring for unsupported models is not offered. Low-level library writes
remain caller-governed. Sequential snapshots, external authentication and single
controller requirements remain documented operational constraints. This follow-up
must not be described as having no outstanding commissioning work.

Follow-up validation: `cargo test --workspace` passed (122 tests/doctests),
`node --test crates/sigen-server/tests/demo.cjs` passed (15 tests), and Clippy
with warnings denied, formatting and diff whitespace checks passed. The core
poll mock now verifies the configured charger unit/address and returned alarm
map; additional tests cover AC severity/unknown bits, model-family rejection,
all electrical wiring types and invalid-type diagnostics. No live hardware was
contacted or configured, and no commissioning item was marked complete.
