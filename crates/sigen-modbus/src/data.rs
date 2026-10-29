use crate::{Client, Error, Result, UnitId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunningState {
    Standby,
    Running,
    Fault,
    Shutdown,
    EnvironmentalAbnormality,
    Unknown(u16),
}
impl From<u16> for RunningState {
    fn from(value: u16) -> Self {
        match value {
            0 => Self::Standby,
            1 => Self::Running,
            2 => Self::Fault,
            3 => Self::Shutdown,
            7 => Self::EnvironmentalAbnormality,
            value => Self::Unknown(value),
        }
    }
}

/// V2.7 appendix 6. Setting this does not itself enable remote EMS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum RemoteEmsMode {
    PcsRemoteControl = 0,
    Standby = 1,
    MaximumSelfConsumption = 2,
    ChargeFromGridFirst = 3,
    ChargeFromPvFirst = 4,
    DischargePvFirst = 5,
    DischargeBatteryFirst = 6,
}

/// Plant readings (§5.1). Collected over multiple requests, not an atomic snapshot.
/// Battery fields require a hybrid inverter. Power signs follow the specification.
#[derive(Clone, Debug, PartialEq)]
pub struct Plant {
    pub epoch_seconds: u32,
    pub timezone_offset_minutes: i16,
    /// Raw value preserves future/unknown modes (0, 1, 2, 5, 7, 9 in V2.7).
    pub ems_work_mode: u16,
    pub grid_sensor_connected: bool,
    /// Positive import, negative export.
    pub grid_power_kw: f64,
    pub grid_reactive_power_kvar: f64,
    /// 0 on-grid, 1 automatic off-grid, 2 manual off-grid.
    pub grid_status: u16,
    pub battery_soc_percent: f64,
    pub active_power_kw: f64,
    pub reactive_power_kvar: f64,
    pub pv_power_kw: f64,
    /// Positive charging, negative discharging.
    pub battery_power_kw: f64,
    pub running_state: RunningState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub model: String,
    pub serial_number: String,
    pub firmware_version: String,
    pub rated_active_power_kw: f64,
    pub max_apparent_power_kva: f64,
    pub max_active_power_kw: f64,
}

/// Inverter electrical readings supported by hybrid and PV inverters (§5.3).
#[derive(Clone, Debug, PartialEq)]
pub struct Inverter {
    pub running_state: RunningState,
    pub active_power_kw: f64,
    pub reactive_power_kvar: f64,
    pub grid_frequency_hz: f64,
    pub internal_temperature_c: f64,
    pub pv_power_kw: f64,
}

/// Battery cluster readings accessed through a hybrid inverter, not individual packs.
#[derive(Clone, Debug, PartialEq)]
pub struct Battery {
    pub max_charge_power_kw: f64,
    pub max_discharge_power_kw: f64,
    pub available_charge_energy_kwh: f64,
    pub available_discharge_energy_kwh: f64,
    pub power_kw: f64,
    pub soc_percent: f64,
    pub soh_percent: f64,
    pub average_cell_temperature_c: f64,
    pub average_cell_voltage_v: f64,
    /// Raw alarm bitfields, in appendix 2, 3, 4, 5, 11 order.
    pub alarms: [u16; 5],
    pub max_temperature_c: f64,
    pub min_temperature_c: f64,
    pub max_cell_voltage_v: f64,
    pub min_cell_voltage_v: f64,
}

impl Client {
    /// Read the core plant metrics. Requires hybrid hardware for the ESS fields.
    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn read_plant(&mut self) -> Result<Plant> {
        let a = self.telemetry(UnitId::PLANT, 30000, 15)?;
        let b = self.telemetry(UnitId::PLANT, 30031, 8)?;
        let state = self.telemetry(UnitId::PLANT, 30051, 1)?;
        Ok(Plant {
            epoch_seconds: u32_at(&a, 0),
            timezone_offset_minutes: a[2] as i16,
            ems_work_mode: a[3],
            grid_sensor_connected: a[4] == 1,
            grid_power_kw: signed(&a, 5, 1000.0),
            grid_reactive_power_kvar: signed(&a, 7, 1000.0),
            grid_status: a[9],
            battery_soc_percent: a[14] as f64 / 10.0,
            active_power_kw: signed(&b, 0, 1000.0),
            reactive_power_kvar: signed(&b, 2, 1000.0),
            pv_power_kw: signed(&b, 4, 1000.0),
            battery_power_kw: signed(&b, 6, 1000.0),
            running_state: state[0].into(),
        })
    }

    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn read_device_info(&mut self, unit: UnitId) -> Result<DeviceInfo> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 30500, 46)?;
        Ok(DeviceInfo {
            model: ascii(&r[..15])?,
            serial_number: ascii(&r[15..25])?,
            firmware_version: ascii(&r[25..40])?,
            rated_active_power_kw: unsigned(&r, 40, 1000.0),
            max_apparent_power_kva: unsigned(&r, 42, 1000.0),
            max_active_power_kw: unsigned(&r, 44, 1000.0),
        })
    }

    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn read_inverter(&mut self, unit: UnitId) -> Result<Inverter> {
        require_inverter(unit)?;
        let state = self.telemetry(unit, 30578, 1)?;
        let power = self.telemetry(unit, 30587, 4)?;
        let grid = self.telemetry(unit, 31002, 2)?;
        let pv = self.telemetry(unit, 31035, 2)?;
        Ok(Inverter {
            running_state: state[0].into(),
            active_power_kw: signed(&power, 0, 1000.0),
            reactive_power_kvar: signed(&power, 2, 1000.0),
            grid_frequency_hz: grid[0] as f64 / 100.0,
            internal_temperature_c: grid[1] as i16 as f64 / 10.0,
            pv_power_kw: signed(&pv, 0, 1000.0),
        })
    }

    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn read_battery(&mut self, unit: UnitId) -> Result<Battery> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 30591, 19)?;
        let extremes = self.telemetry(unit, 30620, 4)?;
        Ok(Battery {
            max_charge_power_kw: unsigned(&r, 0, 1000.0),
            max_discharge_power_kw: unsigned(&r, 2, 1000.0),
            available_charge_energy_kwh: unsigned(&r, 4, 100.0),
            available_discharge_energy_kwh: unsigned(&r, 6, 100.0),
            power_kw: signed(&r, 8, 1000.0),
            soc_percent: r[10] as f64 / 10.0,
            soh_percent: r[11] as f64 / 10.0,
            average_cell_temperature_c: r[12] as i16 as f64 / 10.0,
            average_cell_voltage_v: r[13] as f64 / 1000.0,
            alarms: r[14..19].try_into().expect("fixed-size register range"),
            max_temperature_c: extremes[0] as i16 as f64 / 10.0,
            min_temperature_c: extremes[1] as i16 as f64 / 10.0,
            max_cell_voltage_v: extremes[2] as f64 / 1000.0,
            min_cell_voltage_v: extremes[3] as f64 / 1000.0,
        })
    }

    /// Raw U64 energy counter in hundredths of kWh, without floating-point precision loss.
    /// `address` must identify a U64 energy register in §5.1 or §5.3.
    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn read_energy_counter(&mut self, unit: UnitId, address: u16) -> Result<u64> {
        let r = self.telemetry(unit, address, 4)?;
        Ok(r.iter().fold(0, |value, word| (value << 16) | *word as u64))
    }

    /// Enabling switches the plant into remote EMS (§5.2). No other setting is changed.
    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn set_remote_ems_enabled(&mut self, enabled: bool) -> Result<()> {
        self.write_register(UnitId::PLANT, 40029, u16::from(enabled))
    }

    /// Remote EMS must be enabled separately. Limits in 40032/40034 apply only in
    /// the corresponding charging/discharging modes; see §5.2 and appendix 6.
    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn set_remote_ems_mode(&mut self, mode: RemoteEmsMode) -> Result<()> {
        self.write_register(UnitId::PLANT, 40031, mode as u16)
    }

    /// Set backup reserve (0–100%, rounded to the nearest 0.1%).
    #[tracing::instrument(level = "debug", skip(self), err(level = "debug"))]
    pub fn set_backup_soc_percent(&mut self, percent: f64) -> Result<()> {
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            return Err(Error::InvalidArgument(
                "SOC must be finite and in 0..=100 percent",
            ));
        }
        self.write_register(UnitId::PLANT, 40046, (percent * 10.0).round() as u16)
    }
}

fn require_inverter(unit: UnitId) -> Result<()> {
    if unit == UnitId::PLANT {
        Err(Error::InvalidArgument(
            "use an inverter unit ID for this operation",
        ))
    } else {
        Ok(())
    }
}
fn u32_at(r: &[u16], i: usize) -> u32 {
    ((r[i] as u32) << 16) | r[i + 1] as u32
}
fn unsigned(r: &[u16], i: usize, gain: f64) -> f64 {
    u32_at(r, i) as f64 / gain
}
fn signed(r: &[u16], i: usize, gain: f64) -> f64 {
    u32_at(r, i) as i32 as f64 / gain
}
fn ascii(r: &[u16]) -> Result<String> {
    let bytes: Vec<u8> = r
        .iter()
        .flat_map(|r| r.to_be_bytes())
        .take_while(|b| *b != 0)
        .collect();
    if !bytes.is_ascii() {
        return Err(Error::Protocol("device string is not ASCII"));
    }
    Ok(String::from_utf8(bytes)
        .expect("ASCII is UTF-8")
        .trim_end()
        .to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoding_preserves_sign_word_order_and_ascii() {
        assert_eq!(signed(&[0xffff, 0xfb2e], 0, 1000.0), -1.234);
        assert_eq!(unsigned(&[1, 2], 0, 100.0), 655.38);
        assert_eq!(ascii(&[0x5349, 0x4745, 0x4e20, 0]).unwrap(), "SIGEN");
        assert!(ascii(&[0xffff]).is_err());
        assert_eq!(RunningState::from(42), RunningState::Unknown(42));
    }
}
