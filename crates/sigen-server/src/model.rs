use serde::Serialize;
use sigen_modbus::{Battery, DeviceInfo, Inverter, Plant, RunningState};

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub journal: Option<crate::journal::Journal>,
    #[serde(skip)]
    journal_connected: Option<bool>,
    #[serde(skip)]
    journal_outage_since: Option<std::time::Instant>,
    #[serde(skip)]
    pub write_audit: Option<std::sync::Arc<crate::audit::WriteAuditLog>>,
    #[serde(skip)]
    pub control_epoch: u64,
    #[serde(skip)]
    pub control_valid_until: Option<std::time::Instant>,
    #[serde(skip)]
    pub shutting_down: bool,
    pub pushover: Option<crate::pushover::Status>,
    pub alerts: crate::alerts::Alerts,
    pub last_control_error: Option<String>,
    pub mqtt: Option<MqttStatus>,
    pub target: String,
    pub unit: u8,
    pub online: bool,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    pub poll_errors: u64,
    pub control_errors: u64,
    pub device: Option<Device>,
    pub readings: Option<Readings>,
    pub details: crate::telemetry::Details,
    /// Local device day (days since Unix epoch), for daily counter rollover.
    pub device_day: Option<i64>,
    pub controls: Option<Controls>,
}
impl Status {
    pub fn new(target: String, unit: u8) -> Self {
        Self {
            journal: None,
            journal_connected: None,
            journal_outage_since: None,
            write_audit: None,
            control_epoch: 0,
            control_valid_until: None,
            shutting_down: false,
            pushover: None,
            alerts: crate::alerts::Alerts::default(),
            last_control_error: None,
            mqtt: None,
            target,
            unit,
            online: false,
            last_success_unix: None,
            last_error: None,
            poll_errors: 0,
            control_errors: 0,
            device: None,
            readings: None,
            details: crate::telemetry::Details::default(),
            device_day: None,
            controls: None,
        }
    }

    pub fn controls_ready(&self) -> bool {
        self.fresh() && self.controls.is_some()
    }

    /// Enqueue event metadata only; this never waits for journal disk I/O.
    pub fn event(
        &self,
        category: crate::journal::Category,
        kind: &str,
        details: serde_json::Value,
    ) {
        if let Some(journal) = &self.journal {
            journal.record(self, category, kind, details);
        }
    }

    /// Record one event per observed connection transition, including initial failure.
    /// Outage duration uses a monotonic clock and covers this process's observations.
    pub fn connection_event(&mut self, connected: bool, error: Option<&str>) {
        if self.journal_connected == Some(connected) {
            return;
        }
        let duration = if connected {
            self.journal_outage_since
                .take()
                .map(|since| since.elapsed().as_secs())
        } else {
            self.journal_outage_since = Some(std::time::Instant::now());
            None
        };
        self.event(
            crate::journal::Category::Connectivity,
            if connected {
                "hardware_connected"
            } else {
                "hardware_disconnected"
            },
            serde_json::json!({"error":error, "outage_seconds":duration}),
        );
        self.journal_connected = Some(connected);
    }

    /// Use a monotonic deadline so wall-clock adjustments cannot extend data validity.
    pub fn fresh(&self) -> bool {
        !self.shutting_down
            && self.online
            && self
                .control_valid_until
                .is_some_and(|t| std::time::Instant::now() < t)
    }

    /// Hide expired controls in API responses while retaining readings for stale-data display.
    pub fn snapshot(&self) -> Self {
        let mut snapshot = self.clone();
        if !self.fresh() {
            snapshot.online = false;
            snapshot.controls = None;
        }
        snapshot
    }

    /// Invalidate all requests accepted against the previous hardware state.
    pub fn invalidate_controls(&mut self) {
        self.control_epoch = self.control_epoch.wrapping_add(1);
        self.control_valid_until = None;
        self.controls = None;
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MqttStatus {
    pub connected: bool,
    pub last_error: Option<String>,
    pub last_command_error: Option<String>,
    pub topic_prefix: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub model: String,
    pub serial_number: String,
    pub firmware_version: String,
}
impl From<DeviceInfo> for Device {
    fn from(d: DeviceInfo) -> Self {
        Self {
            model: d.model,
            serial_number: d.serial_number,
            firmware_version: d.firmware_version,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Controls {
    pub remote_ems_enabled: bool,
    pub remote_ems_mode: u16,
    pub backup_soc_percent: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Readings {
    pub grid_power_watts: f64,
    pub plant_power_watts: f64,
    pub pv_power_watts: f64,
    pub battery_power_watts: f64,
    pub battery_soc_percent: f64,
    pub battery_soh_percent: f64,
    pub battery_temperature_celsius: f64,
    pub battery_min_temperature_celsius: f64,
    pub battery_max_temperature_celsius: f64,
    pub battery_cell_voltage_volts: f64,
    pub battery_min_cell_voltage_volts: f64,
    pub battery_max_cell_voltage_volts: f64,
    pub battery_charge_limit_watts: f64,
    pub battery_discharge_limit_watts: f64,
    pub battery_charge_available_wh: f64,
    pub battery_discharge_available_wh: f64,
    pub inverter_power_watts: f64,
    pub inverter_temperature_celsius: f64,
    pub grid_frequency_hertz: f64,
    pub grid_connected: bool,
    pub grid_sensor_connected: bool,
    pub ems_work_mode: u16,
    pub plant_state: String,
    pub inverter_state: String,
    pub alarms: [u16; 5],
}
impl Readings {
    pub fn new(p: Plant, i: Inverter, b: Battery) -> Self {
        Self {
            grid_power_watts: p.grid_power_kw * 1000.0,
            plant_power_watts: p.active_power_kw * 1000.0,
            pv_power_watts: p.pv_power_kw * 1000.0,
            battery_power_watts: p.battery_power_kw * 1000.0,
            battery_soc_percent: b.soc_percent,
            battery_soh_percent: b.soh_percent,
            battery_temperature_celsius: b.average_cell_temperature_c,
            battery_min_temperature_celsius: b.min_temperature_c,
            battery_max_temperature_celsius: b.max_temperature_c,
            battery_cell_voltage_volts: b.average_cell_voltage_v,
            battery_min_cell_voltage_volts: b.min_cell_voltage_v,
            battery_max_cell_voltage_volts: b.max_cell_voltage_v,
            battery_charge_limit_watts: b.max_charge_power_kw * 1000.0,
            battery_discharge_limit_watts: b.max_discharge_power_kw * 1000.0,
            battery_charge_available_wh: b.available_charge_energy_kwh * 1000.0,
            battery_discharge_available_wh: b.available_discharge_energy_kwh * 1000.0,
            inverter_power_watts: i.active_power_kw * 1000.0,
            inverter_temperature_celsius: i.internal_temperature_c,
            grid_frequency_hertz: i.grid_frequency_hz,
            grid_connected: p.grid_status == 0,
            grid_sensor_connected: p.grid_sensor_connected,
            ems_work_mode: p.ems_work_mode,
            plant_state: state_name(p.running_state),
            inverter_state: state_name(i.running_state),
            alarms: b.alarms,
        }
    }
}
fn state_name(state: RunningState) -> String {
    match state {
        RunningState::Standby => "Standby".into(),
        RunningState::Running => "Running".into(),
        RunningState::Fault => "Fault".into(),
        RunningState::Shutdown => "Shutdown".into(),
        RunningState::EnvironmentalAbnormality => "Environmental abnormality".into(),
        RunningState::Unknown(n) => format!("Unknown ({n})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn monotonic_expiry_hides_controls_and_metrics_even_with_future_wall_clock_timestamp() {
        let mut status = Status::new("127.0.0.1:502".into(), 1);
        status.online = true;
        status.last_success_unix = Some(u64::MAX);
        status.controls = Some(Controls {
            remote_ems_enabled: true,
            remote_ems_mode: 2,
            backup_soc_percent: 20.0,
        });
        status.control_valid_until = Some(Instant::now() + Duration::from_secs(30));
        assert!(status.controls_ready());
        assert!(status.snapshot().online);
        status.control_valid_until = Some(Instant::now());
        assert!(!status.controls_ready());
        let snapshot = status.snapshot();
        assert!(!snapshot.online);
        assert!(snapshot.controls.is_none());
        let metrics = crate::metrics::render(&status);
        assert!(metrics.contains("sigen_up 0\n"));
        assert!(!metrics.contains("sigen_remote_ems_enabled"));
    }
}
