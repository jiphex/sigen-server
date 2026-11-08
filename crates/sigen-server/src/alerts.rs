use crate::{
    hardware::{self, SharedStatus},
    model::Status,
};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf, time::Duration};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    High,
    Critical,
}

fn severity(id: Id, readings: Option<&crate::model::Readings>) -> Severity {
    // Physical hazard bits from Sigenergy Modbus V2.7 appendices 2–4.
    // Vendor "Critical" also includes operational faults (e.g. version mismatch),
    // so it is not itself evidence of physical danger.
    let dangerous = readings.is_some_and(|r| match id {
        // Insulation, overheating, PV overvoltage/reversal/back-feed, arc, DC current.
        Id::InverterFault => {
            (r.alarms[0] | r.plant_alarms[0]) & 0x81e6 != 0
                || (r.alarms[1] | r.plant_alarms[1]) & 0x0101 != 0
        }
        // Battery overheating or thermal runaway.
        Id::BatteryAlarm => (r.alarms[2] | r.plant_alarms[2]) & 0x0044 != 0,
        Id::GatewayAlarm => (r.alarms[3] | r.plant_alarms[3]) & 0xff != 0,
        Id::DcChargerAlarm => (r.alarms[4] | r.plant_alarms[4]) & 0x3f != 0,
        Id::AcChargerAlarm => r
            .ac_charger_alarms
            .values()
            .any(|a| a[0] & 0x1ff != 0 || a[1] & 0x1f != 0 || a[2] & 3 != 0),
        _ => false,
    });
    if dangerous {
        Severity::Critical
    } else {
        Severity::High
    }
}

fn vendor_severity(id: Id, r: &crate::model::Readings) -> Option<&'static str> {
    let a: [u16; 5] = std::array::from_fn(|i| r.alarms[i] | r.plant_alarms[i]);
    let (critical, general, unknown) = match id {
        Id::InverterFault => (
            a[0] & 0xffef != 0 || a[1] & 0x03bd != 0,
            a[0] & 0x10 != 0 || a[1] & 2 != 0,
            a[1] & !0x03bf != 0,
        ),
        Id::BatteryAlarm => (a[2] & 0x7d != 0, a[2] & 2 != 0, a[2] & !0x7f != 0),
        Id::GatewayAlarm => (a[3] & 0xff != 0, false, a[3] & !0xff != 0),
        Id::DcChargerAlarm => (a[4] & 0x3f != 0, false, a[4] & !0x3f != 0),
        Id::AcChargerAlarm => {
            let mut a = [0_u16; 3];
            for words in r.ac_charger_alarms.values() {
                for i in 0..3 {
                    a[i] |= words[i];
                }
            }
            (
                a[0] & 0x1ff != 0 || a[1] & 0x1f != 0 || a[2] & 3 != 0,
                a[1] & 0x20 != 0 || a[2] & 4 != 0,
                a[0] & !0x1ff != 0 || a[1] & !0x3f != 0 || a[2] & !7 != 0,
            )
        }
        _ => return None,
    };
    if critical {
        Some("critical")
    } else if unknown {
        Some("unknown")
    } else if general {
        Some("general")
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Id {
    HardwareOffline,
    InverterFault,
    BatteryAlarm,
    GridDisconnected,
    ControlFailed,
    GatewayAlarm,
    DcChargerAlarm,
    AcChargerAlarm,
}
impl Id {
    pub const ALL: [Self; 8] = [
        Self::HardwareOffline,
        Self::InverterFault,
        Self::BatteryAlarm,
        Self::GridDisconnected,
        Self::ControlFailed,
        Self::GatewayAlarm,
        Self::DcChargerAlarm,
        Self::AcChargerAlarm,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::HardwareOffline => "hardware_offline",
            Self::InverterFault => "inverter_fault",
            Self::BatteryAlarm => "battery_alarm",
            Self::GridDisconnected => "grid_disconnected",
            Self::ControlFailed => "control_failed",
            Self::GatewayAlarm => "gateway_alarm",
            Self::DcChargerAlarm => "dc_charger_alarm",
            Self::AcChargerAlarm => "ac_charger_alarm",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::HardwareOffline => "Hardware offline or stale",
            Self::InverterFault => "Inverter fault",
            Self::BatteryAlarm => "Battery alarm",
            Self::GridDisconnected => "Grid disconnected",
            Self::ControlFailed => "Control failed",
            Self::GatewayAlarm => "Gateway alarm",
            Self::DcChargerAlarm => "DC charger alarm",
            Self::AcChargerAlarm => "AC charger alarm",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub hardware_offline: bool,
    pub inverter_fault: bool,
    pub battery_alarm: bool,
    pub grid_disconnected: bool,
    pub control_failed: bool,
    pub gateway_alarm: bool,
    pub dc_charger_alarm: bool,
    pub ac_charger_alarm: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            hardware_offline: true,
            inverter_fault: true,
            battery_alarm: true,
            grid_disconnected: true,
            control_failed: true,
            gateway_alarm: true,
            dc_charger_alarm: true,
            ac_charger_alarm: true,
        }
    }
}
impl Settings {
    fn enabled(&self, id: Id) -> bool {
        match id {
            Id::HardwareOffline => self.hardware_offline,
            Id::InverterFault => self.inverter_fault,
            Id::BatteryAlarm => self.battery_alarm,
            Id::GridDisconnected => self.grid_disconnected,
            Id::ControlFailed => self.control_failed,
            Id::GatewayAlarm => self.gateway_alarm,
            Id::DcChargerAlarm => self.dc_charger_alarm,
            Id::AcChargerAlarm => self.ac_charger_alarm,
        }
    }
    fn set(&mut self, id: Id, enabled: bool) {
        *match id {
            Id::HardwareOffline => &mut self.hardware_offline,
            Id::InverterFault => &mut self.inverter_fault,
            Id::BatteryAlarm => &mut self.battery_alarm,
            Id::GridDisconnected => &mut self.grid_disconnected,
            Id::ControlFailed => &mut self.control_failed,
            Id::GatewayAlarm => &mut self.gateway_alarm,
            Id::DcChargerAlarm => &mut self.dc_charger_alarm,
            Id::AcChargerAlarm => &mut self.ac_charger_alarm,
        } = enabled;
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Alert {
    pub id: Id,
    pub name: &'static str,
    pub enabled: bool,
    pub active: bool,
    pub severity: Severity,
    pub vendor_severity: Option<&'static str>,
    /// False when hardware data is unavailable; previously active alerts remain latched.
    pub known: bool,
    pub since_unix: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Alerts {
    #[serde(skip)]
    pub notifications: Option<tokio::sync::mpsc::Sender<crate::pushover::Notification>>,
    pub items: Vec<Alert>,
    #[serde(skip)]
    settings: Settings,
    #[serde(skip)]
    path: Option<PathBuf>,
    #[serde(skip)]
    stale_after: u64,
    #[serde(skip)]
    started: u64,
}
impl Default for Alerts {
    fn default() -> Self {
        Self::new(Settings::default(), None, 45)
    }
}
impl Alerts {
    fn new(settings: Settings, path: Option<PathBuf>, stale_after: u64) -> Self {
        Self {
            notifications: None,
            items: Id::ALL
                .into_iter()
                .map(|id| Alert {
                    id,
                    name: id.name(),
                    enabled: settings.enabled(id),
                    active: false,
                    severity: Severity::High,
                    vendor_severity: None,
                    known: false,
                    since_unix: None,
                })
                .collect(),
            settings,
            path,
            stale_after,
            started: hardware::now(),
        }
    }
    pub fn load(path: PathBuf, poll_seconds: u64) -> io::Result<Self> {
        let settings = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e),
        };
        Ok(Self::new(settings, Some(path), poll_seconds + 30))
    }
    /// Replace settings via a synced sibling file so readers do not see partial JSON.
    fn save(&self, settings: &Settings) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let temp = path.with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            use io::Write;
            let mut file = fs::File::create(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(settings).map_err(io::Error::other)?)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

/// Evaluate from cached readings only. Never clears hardware faults using stale data.
/// Notify on activation, escalation, or confirmed recovery, rather than on every poll.
pub fn refresh(status: &mut Status, now: u64) {
    // Intentional shutdown is not a new hardware outage or alert transition.
    if status.shutting_down {
        return;
    }
    let mut transitions = Vec::new();
    let fresh = status.fresh()
        && status
            .last_success_unix
            .is_some_and(|t| now.saturating_sub(t) <= status.alerts.stale_after);
    let offline = if let Some(t) = status.last_success_unix {
        !status.fresh() || now.saturating_sub(t) > status.alerts.stale_after
    } else {
        status.poll_errors > 0
            || now.saturating_sub(status.alerts.started) > status.alerts.stale_after
    };
    let r = status.readings.as_ref().filter(|_| fresh);
    let conditions = [
        Some(offline),
        r.map(|r| {
            (r.alarms[0] | r.plant_alarms[0]) != 0
                || (r.alarms[1] | r.plant_alarms[1]) != 0
                || [r.plant_state.as_str(), r.inverter_state.as_str()]
                    .iter()
                    .any(|s| matches!(*s, "Fault" | "Environmental abnormality"))
        }),
        // Battery ESS alarms are group 3 (V2.7 appendix 4), not PCS/gateway/charger alarms.
        r.map(|r| (r.alarms[2] | r.plant_alarms[2]) != 0),
        r.map(|r| !r.grid_connected),
        Some(status.last_control_error.is_some()),
        r.map(|r| (r.alarms[3] | r.plant_alarms[3]) != 0),
        r.map(|r| (r.alarms[4] | r.plant_alarms[4]) != 0),
        r.filter(|r| !r.ac_charger_alarms.is_empty()).map(|r| {
            r.ac_charger_alarms
                .values()
                .flatten()
                .any(|bits| *bits != 0)
        }),
    ];
    let severities = Id::ALL.map(|id| severity(id, r));
    for ((alert, condition), severity) in status
        .alerts
        .items
        .iter_mut()
        .zip(conditions)
        .zip(severities)
    {
        if let Some(readings) = r {
            alert.vendor_severity = vendor_severity(alert.id, readings);
        }
        alert.enabled = status.alerts.settings.enabled(alert.id);
        alert.known = !alert.enabled || condition.is_some();
        let active = alert.enabled && condition.unwrap_or(alert.active);
        // Latch the highest severity until recovery, including while data is stale.
        let next_severity = if active && alert.active {
            alert.severity.max(severity)
        } else {
            severity
        };
        let escalated = active && alert.active && next_severity > alert.severity;
        if active != alert.active || escalated {
            // Disabling is an administrative clear, not evidence of hardware recovery.
            transitions.push((
                if !alert.enabled { "alert_disabled" } else if escalated { "alert_escalated" }
                else if active { "alert_activated" } else { "alert_recovered" },
                serde_json::json!({"alert":alert.id, "name":alert.name,"severity":if active { next_severity } else { alert.severity },
                    "since_unix":if active && !alert.active { Some(now) } else { alert.since_unix },
                    "duration_seconds":if !active { alert.since_unix.map(|t| now.saturating_sub(t)) } else { None },
                    "vendor_severity":alert.vendor_severity, "ac_charger_alarms":r.map(|r| &r.ac_charger_alarms), "plant_alarms":r.map(|r| r.plant_alarms), "alarms":r.map(|r| r.alarms)}),
            ));
            if alert.enabled
                && (active || alert.known)
                && let Some(sender) = &status.alerts.notifications
            {
                let note = crate::pushover::Notification {
                    id: alert.id,
                    name: alert.name,
                    active,
                    severity: next_severity,
                    timestamp: now,
                    target: status.target.clone(),
                    unit: status.unit,
                };
                if sender.try_send(note).is_err() {
                    tracing::warn!(
                        alert = alert.id.key(),
                        "Pushover notification queue full or unavailable"
                    );
                    if let Some(p) = &mut status.pushover {
                        p.failed_total += 1;
                        p.last_error = Some("Notification queue full or unavailable".into());
                    }
                }
            }
            if active {
                tracing::warn!(alert = alert.id.key(), "Alert activated");
            } else {
                tracing::info!(alert = alert.id.key(), "Alert cleared");
            }
        }
        if active && !alert.active {
            alert.since_unix = Some(now);
        }
        if !active {
            alert.since_unix = None;
        }
        alert.active = active;
        alert.severity = next_severity;
    }
    for (kind, details) in transitions {
        status.event(crate::journal::Category::Alert, kind, details);
    }
}

/// Persist a toggle before applying it; a failed save leaves live settings intact.
pub fn set_enabled(status: &SharedStatus, id: Id, enabled: bool) -> io::Result<Alerts> {
    let mut s = status.write().expect("status lock");
    let mut settings = s.alerts.settings.clone();
    let changed = settings.enabled(id) != enabled;
    settings.set(id, enabled);
    // Persist before changing live settings, keeping concurrent edits serialized.
    s.alerts.save(&settings).inspect_err(|error| {
        tracing::error!(%error, alert = id.key(), "Could not save alert settings");
    })?;
    tracing::info!(alert = id.key(), enabled, "Alert setting saved");
    s.alerts.settings = settings;
    if changed {
        s.event(
            crate::journal::Category::Settings,
            "alert_setting_changed",
            serde_json::json!({"alert":id,"enabled":enabled}),
        );
    }
    refresh(&mut s, hardware::now());
    Ok(s.alerts.clone())
}

pub fn start(status: SharedStatus, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    // This loop waits on async timer/shutdown events and only inspects cached hardware state.
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                _ = timer.tick() => refresh(&mut status.write().expect("status lock"),hardware::now()),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Readings;
    use std::sync::{Arc, RwLock};
    fn readings() -> Readings {
        Readings {
            grid_power_watts: Some(0.0),
            plant_power_watts: 0.0,
            pv_power_watts: 0.0,
            battery_power_watts: 0.0,
            battery_soc_percent: 50.0,
            battery_soh_percent: 100.0,
            battery_temperature_celsius: 20.0,
            battery_min_temperature_celsius: 20.0,
            battery_max_temperature_celsius: 20.0,
            battery_cell_voltage_volts: 3.3,
            battery_min_cell_voltage_volts: 3.3,
            battery_max_cell_voltage_volts: 3.3,
            battery_charge_limit_watts: 5000.0,
            battery_discharge_limit_watts: 5000.0,
            battery_charge_available_wh: 5000.0,
            battery_discharge_available_wh: 5000.0,
            inverter_power_watts: 0.0,
            inverter_temperature_celsius: 20.0,
            grid_frequency_hertz: 50.0,
            grid_connected: true,
            grid_sensor_connected: true,
            ems_work_mode: 0,
            plant_state: "Running".into(),
            inverter_state: "Running".into(),
            alarms: [0; 5],
            plant_alarms: [0; 5],
            ac_charger_alarms: Default::default(),
        }
    }
    fn healthy() -> Status {
        let mut s = Status::new("127.0.0.1:502".into(), 1);
        s.online = true;
        s.control_valid_until = Some(std::time::Instant::now() + Duration::from_secs(45));
        s.last_success_unix = Some(100);
        s.alerts.started = 100;
        s.readings = Some(readings());
        s
    }
    fn alert(s: &Status, id: Id) -> &Alert {
        s.alerts.items.iter().find(|a| a.id == id).unwrap()
    }

    #[test]
    fn ac_charger_alerts_preserve_source_and_vendor_severity() {
        let mut s = healthy();
        refresh(&mut s, 100);
        assert!(!alert(&s, Id::AcChargerAlarm).known);
        for (words, severity) in [
            ([1, 0, 0], "critical"),
            ([0, 32, 0], "general"),
            ([0, 0, 4], "general"),
            ([0, 0, 0x8000], "unknown"),
        ] {
            s.readings
                .as_mut()
                .unwrap()
                .ac_charger_alarms
                .insert(2, words);
            refresh(&mut s, 101);
            assert!(alert(&s, Id::AcChargerAlarm).active);
            assert_eq!(
                alert(&s, Id::AcChargerAlarm).vendor_severity,
                Some(severity)
            );
        }
        s.readings
            .as_mut()
            .unwrap()
            .ac_charger_alarms
            .insert(2, [0; 3]);
        refresh(&mut s, 102);
        assert!(!alert(&s, Id::AcChargerAlarm).active);
    }

    #[test]
    fn aggregate_alarms_cover_each_group_and_unknown_bits() {
        for (group, id) in [
            (0, Id::InverterFault),
            (1, Id::InverterFault),
            (2, Id::BatteryAlarm),
            (3, Id::GatewayAlarm),
            (4, Id::DcChargerAlarm),
        ] {
            let mut s = healthy();
            s.readings.as_mut().unwrap().plant_alarms[group] = 1;
            refresh(&mut s, 100);
            assert!(alert(&s, id).active);
            assert_eq!(alert(&s, id).vendor_severity, Some("critical"));
            s.readings.as_mut().unwrap().plant_alarms[group] = 0;
            refresh(&mut s, 101);
            assert!(!alert(&s, id).active);
        }
        let mut s = healthy();
        s.readings.as_mut().unwrap().plant_alarms[4] = 0x8000;
        refresh(&mut s, 100);
        assert!(alert(&s, Id::DcChargerAlarm).active);
        assert_eq!(
            alert(&s, Id::DcChargerAlarm).vendor_severity,
            Some("unknown")
        );
        s.readings.as_mut().unwrap().alarms[2] = 2;
        refresh(&mut s, 101);
        assert_eq!(alert(&s, Id::BatteryAlarm).vendor_severity, Some("general"));
    }

    #[tokio::test]
    async fn journal_records_transitions_once_and_does_not_call_disabling_recovery() {
        let fixture = crate::journal::TestJournal::new();
        let mut s = healthy();
        s.journal = Some(fixture.journal.clone());
        s.readings.as_mut().unwrap().alarms[2] = 1;
        refresh(&mut s, 100);
        refresh(&mut s, 101);
        s.readings.as_mut().unwrap().alarms[2] = 1 << 6;
        refresh(&mut s, 102);
        // A stale observation cannot manufacture recovery or lower severity.
        s.online = false;
        refresh(&mut s, 103);
        s.online = true;
        s.readings.as_mut().unwrap().alarms[2] = 0;
        refresh(&mut s, 104);
        s.readings.as_mut().unwrap().alarms[2] = 1;
        // set_enabled uses wall time, so keep this fixture fresh in both clocks.
        s.last_success_unix = Some(hardware::now());
        refresh(&mut s, hardware::now());
        let shared = Arc::new(RwLock::new(s));
        set_enabled(&shared, Id::BatteryAlarm, false).unwrap();
        set_enabled(&shared, Id::BatteryAlarm, false).unwrap();
        {
            let mut s = shared.write().unwrap();
            s.shutting_down = true;
            s.online = false;
            refresh(&mut s, hardware::now());
        }
        let page = fixture
            .journal
            .query(crate::journal::Query::default())
            .await
            .unwrap();
        let battery: Vec<_> = page
            .events
            .iter()
            .rev()
            .filter(|e| e.details["alert"] == "battery_alarm")
            .collect();
        let kinds: Vec<_> = battery.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "alert_activated",
                "alert_escalated",
                "alert_recovered",
                "alert_activated",
                "alert_setting_changed",
                "alert_disabled"
            ]
        );
        assert_eq!(battery[2].details["duration_seconds"], 4);
        fixture.journal.shutdown().await.unwrap();
    }
    #[test]
    fn physical_hazards_are_critical_and_other_alarm_bits_are_high() {
        for (group, id, critical_bits) in [
            (0, Id::InverterFault, vec![1, 2, 5, 6, 7, 8, 15]),
            (1, Id::InverterFault, vec![0, 8]),
            (2, Id::BatteryAlarm, vec![2, 6]),
        ] {
            for bit in 0..16 {
                let mut s = healthy();
                s.readings.as_mut().unwrap().alarms[group] = 1 << bit;
                refresh(&mut s, 100);
                assert!(alert(&s, id).active);
                assert_eq!(
                    alert(&s, id).severity,
                    if critical_bits.contains(&bit) {
                        Severity::Critical
                    } else {
                        Severity::High
                    },
                    "group {group}, bit {bit}"
                );
            }
        }
    }
    #[test]
    fn active_alert_escalates_once_and_latches_until_confirmed_recovery() {
        let mut s = healthy();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        s.alerts.notifications = Some(sender);
        s.readings.as_mut().unwrap().alarms[2] = 1;
        refresh(&mut s, 100);
        assert_eq!(receiver.try_recv().unwrap().severity, Severity::High);
        s.readings.as_mut().unwrap().alarms[2] = 64;
        refresh(&mut s, 101);
        let escalation = receiver.try_recv().unwrap();
        assert!(escalation.active);
        assert_eq!(escalation.severity, Severity::Critical);
        assert_eq!(alert(&s, Id::BatteryAlarm).since_unix, Some(100));
        s.readings.as_mut().unwrap().alarms[2] = 1;
        refresh(&mut s, 102);
        s.readings.as_mut().unwrap().alarms[2] = 64;
        refresh(&mut s, 103);
        assert!(receiver.try_recv().is_err());
        s.readings.as_mut().unwrap().alarms[2] = 0;
        refresh(&mut s, 146);
        assert_eq!(receiver.try_recv().unwrap().id, Id::HardwareOffline);
        assert!(alert(&s, Id::BatteryAlarm).active);
        assert_eq!(alert(&s, Id::BatteryAlarm).severity, Severity::Critical);
        s.last_success_unix = Some(147);
        refresh(&mut s, 147);
        let notes = [receiver.try_recv().unwrap(), receiver.try_recv().unwrap()];
        assert!(notes.iter().all(|n| !n.active));
        s.readings.as_mut().unwrap().alarms[2] = 1;
        refresh(&mut s, 148);
        assert_eq!(receiver.try_recv().unwrap().severity, Severity::High);
    }
    #[test]
    fn device_alerts_trigger_recover_and_do_not_mistake_other_alarm_groups_for_battery() {
        let mut s = healthy();
        refresh(&mut s, 100);
        assert!(
            s.alerts
                .items
                .iter()
                .all(|a| !a.active && (a.known || a.id == Id::AcChargerAlarm))
        );
        let r = s.readings.as_mut().unwrap();
        r.inverter_state = "Fault".into();
        r.grid_connected = false;
        r.alarms = [1, 2, 0, 4, 8];
        refresh(&mut s, 101);
        assert!(alert(&s, Id::InverterFault).active);
        assert!(alert(&s, Id::GridDisconnected).active);
        assert!(!alert(&s, Id::BatteryAlarm).active);
        s.readings.as_mut().unwrap().alarms[2] = 64;
        refresh(&mut s, 102);
        assert!(alert(&s, Id::BatteryAlarm).active);
        assert_eq!(alert(&s, Id::InverterFault).since_unix, Some(101));
        s.readings = Some(readings());
        refresh(&mut s, 103);
        assert!(s.alerts.items.iter().all(|a| !a.active));
        assert_eq!(alert(&s, Id::InverterFault).since_unix, None);
    }
    #[test]
    fn startup_grace_staleness_and_unknown_fault_state_are_distinct() {
        let mut s = healthy();
        s.online = false;
        s.last_success_unix = None;
        refresh(&mut s, 110);
        assert!(!alert(&s, Id::HardwareOffline).active);
        assert!(!alert(&s, Id::GridDisconnected).known);
        refresh(&mut s, 146);
        assert!(alert(&s, Id::HardwareOffline).active);
        s = healthy();
        s.readings.as_mut().unwrap().inverter_state = "Fault".into();
        refresh(&mut s, 100);
        // A stale cached value must not clear an active fault.
        s.readings.as_mut().unwrap().inverter_state = "Running".into();
        refresh(&mut s, 146);
        assert!(alert(&s, Id::HardwareOffline).active);
        assert!(alert(&s, Id::InverterFault).active);
        assert!(!alert(&s, Id::InverterFault).known);
        s.last_success_unix = Some(147);
        refresh(&mut s, 147);
        assert!(!alert(&s, Id::HardwareOffline).active);
        assert!(!alert(&s, Id::InverterFault).active);
    }
    #[test]
    fn failed_controls_survive_poll_recovery_but_clear_after_confirmed_success() {
        let mut s = healthy();
        s.last_control_error = Some("readback differs".into());
        refresh(&mut s, 100);
        assert!(alert(&s, Id::ControlFailed).active);
        s.last_success_unix = Some(200);
        refresh(&mut s, 200);
        assert!(alert(&s, Id::ControlFailed).active);
        s.last_control_error = None;
        refresh(&mut s, 201);
        assert!(!alert(&s, Id::ControlFailed).active);
    }
    #[test]
    fn pcs_alarm_bits_trigger_an_alert_even_while_running_state_still_says_running() {
        for group in [0, 1] {
            let mut s = healthy();
            s.readings.as_mut().unwrap().alarms[group] = 4;
            refresh(&mut s, 100);
            assert!(alert(&s, Id::InverterFault).active);
            assert!(!alert(&s, Id::BatteryAlarm).active);
        }
    }
    #[test]
    fn toggles_persist_and_failed_saves_do_not_change_live_settings() {
        let path =
            std::env::temp_dir().join(format!("sigen-alert-test-{}.json", std::process::id()));
        let mut s = healthy();
        s.alerts = Alerts::load(path.clone(), 15).unwrap();
        s.last_control_error = Some("failure".into());
        let shared = Arc::new(RwLock::new(s));
        set_enabled(&shared, Id::ControlFailed, false).unwrap();
        assert!(!alert(&shared.read().unwrap(), Id::ControlFailed).active);
        let loaded = Alerts::load(path.clone(), 15).unwrap();
        assert!(!loaded.settings.control_failed);
        assert!(loaded.settings.hardware_offline);
        fs::remove_file(path).unwrap();
        shared.write().unwrap().alerts.path = Some(std::env::temp_dir().join(format!(
            "sigen-missing-parent-{}/alerts.json",
            std::process::id()
        )));
        assert!(set_enabled(&shared, Id::HardwareOffline, false).is_err());
        assert!(shared.read().unwrap().alerts.settings.hardware_offline);
    }
    #[test]
    fn notifications_follow_transitions_not_poll_ticks_and_disabling_is_not_recovery() {
        let mut s = healthy();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        s.alerts.notifications = Some(sender);
        refresh(&mut s, 100);
        assert!(receiver.try_recv().is_err());
        s.readings.as_mut().unwrap().grid_connected = false;
        refresh(&mut s, 101);
        refresh(&mut s, 102);
        let activation = receiver.try_recv().unwrap();
        assert_eq!(activation.id, Id::GridDisconnected);
        assert!(activation.active);
        assert!(receiver.try_recv().is_err());
        s.online = false;
        refresh(&mut s, 103);
        // Offline triggers its own event, but doesn't produce a false grid recovery.
        assert_eq!(receiver.try_recv().unwrap().id, Id::HardwareOffline);
        assert!(receiver.try_recv().is_err());
        s.alerts.settings.grid_disconnected = false;
        refresh(&mut s, 104);
        assert!(receiver.try_recv().is_err());
        s.online = true;
        s.last_success_unix = Some(105);
        refresh(&mut s, 105);
        let recovery = receiver.try_recv().unwrap();
        assert_eq!(recovery.id, Id::HardwareOffline);
        assert!(!recovery.active);
        assert!(receiver.try_recv().is_err());
    }
}
