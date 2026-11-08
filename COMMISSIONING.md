# Installation verification — open, 2026-11-09

Status: NOT PERFORMED. No real inverter has been contacted by these reviews.
Mock tests do not provide evidence for the following checks. Do not mark these
items complete from a software test result or the existence of a register echo.

Record the operator, date, exact model, serial, firmware, protocol revision,
plant topology, configured inverter/charger IDs and deployed software revision.
Retain observations and manufacturer confirmation with the installation record.

| Verification | Required evidence | Status |
| --- | --- | --- |
| Hardware/firmware compatibility | Identity and supported readbacks on the installed model; wiring and device IDs checked | Open |
| Standby preparation | Verify standby can be prepared while EMS is disabled and that activation produces the intended observed behavior | Open |
| Existing control configuration | Record grid limits, inverter overrides, phase settings and protection settings; establish their precedence | Open |
| Mode/physical feedback | Compare requested settings, plant EMS status and independently observed power flow after settling | Open |
| Failure behavior | Establish behavior during controller stop, network loss and power loss; obtain evidence of any watchdog rather than assuming one | Open |
| Alarm coverage | Verify selected-inverter, aggregate plant and every configured AC-charger alarm source and notification route | Open |
| Deployment access | One controlling authority, isolated Modbus network, authenticated HTTP access and restricted MQTT command permissions | Open |

These require an installation-specific commissioning procedure and authorized
operator; this file does not authorize automatic hardware changes. No automatic
rollback, emergency-stop guarantee or persistence guarantee is implemented.
Forced dispatch remains disabled regardless of this checklist's status. Supporting
it requires a separately reviewed implementation of commissioned profiles and
fresh prerequisite validation. Record any unavailable register or ambiguous
behavior as a blocker rather than relaxing software checks to force activation.
