//! Optional V2.7 register groups. Reads do not change device settings.
use super::*;

/// Active/reactive powers in A, B, C order. Availability depends on output wiring.
#[derive(Clone, Debug, PartialEq)]
pub struct PhasePower {
    pub active_kw: [f64; 3],
    pub reactive_kvar: [f64; 3],
}

/// Available plant capabilities; ESS fields require hybrid hardware (§5.1).
#[derive(Clone, Debug, PartialEq)]
pub struct PlantCapabilities {
    pub max_active_power_kw: f64,
    /// The document labels this register kVar despite calling it apparent power.
    pub max_apparent_power: f64,
    pub available_export_power_kw: f64,
    /// Unsigned magnitude of absorption capability, not a negative power.
    pub available_import_power_kw: f64,
    pub available_reactive_export_kvar: f64,
    pub available_reactive_import_kvar: f64,
    pub available_charge_power_kw: f64,
    pub available_discharge_power_kw: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlantBattery {
    pub available_charge_energy_kwh: f64,
    pub available_discharge_energy_kwh: f64,
    pub rated_charge_power_kw: f64,
    pub rated_discharge_power_kw: f64,
    pub rated_energy_kwh: f64,
    pub charge_cutoff_soc_percent: f64,
    pub discharge_cutoff_soc_percent: f64,
    pub soh_percent: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BatteryRatings {
    pub max_absorption_power_kw: f64,
    pub rated_energy_kwh: f64,
    pub rated_charge_power_kw: f64,
    pub rated_discharge_power_kw: f64,
}

/// All energy values are exact integer hundredths of kWh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryEnergy {
    pub daily_charge: u32,
    pub total_charge: u64,
    pub daily_discharge: u32,
    pub total_discharge: u64,
}

/// Both PV energy registers are U32, including total generation (§5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PvEnergy {
    pub daily: u32,
    pub total: u32,
}

/// Plant U64 counters in §5.1. Values are returned in exact 0.01 kWh units.
/// New-interface counters start from zero on the firmware upgrade that adds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum PlantEnergyCounter {
    PvGeneration = 30088,
    LoadConsumption = 30094,
    ThirdPartyGeneration = 30196,
    BatteryCharge = 30200,
    BatteryDischarge = 30204,
    DcChargerCharge = 30208,
    DcChargerDischarge = 30212,
    GridImport = 30216,
    GridExport = 30220,
    GeneratorOutput = 30224,
    CommonLoadConsumption = 30228,
    AcChargerCharge = 30232,
    SelfPvGeneration = 30236,
    NewThirdPartyGeneration = 30240,
    NewBatteryCharge = 30244,
    NewBatteryDischarge = 30248,
    NewDcChargerCharge = 30252,
    NewDcChargerDischarge = 30256,
    NewGridImport = 30260,
    NewGridExport = 30264,
    NewGeneratorOutput = 30268,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmartLoad {
    /// Exact 0.01 kWh units; U32 in the specification.
    pub total_energy: u32,
    pub power_kw: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Electrical {
    pub rated_grid_voltage_v: f64,
    pub rated_grid_frequency_hz: f64,
    pub grid_frequency_hz: f64,
    pub internal_temperature_c: f64,
    /// Raw output type: 0 L/N, 1 L1/L2/L3, 2 L1/L2/L3/N, 3 L1/L2/N.
    pub output_type: u16,
    /// AB, BC, CA order. Not valid for all output types; see §5.3.
    pub line_voltage_v: [f64; 3],
    /// A, B, C order; on single-phase systems only A is meaningful.
    pub phase_voltage_v: [f64; 3],
    pub phase_current_a: [f64; 3],
    /// U16 in V2.7; no inferred signed reinterpretation.
    pub power_factor: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PvString {
    pub voltage_v: f64,
    pub current_a: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InverterDiagnostics {
    pub insulation_resistance_megohms: f64,
    pub startup_epoch_seconds: u32,
    pub shutdown_epoch_seconds: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PowerAdjustmentFeedback {
    pub active_power_kw: f64,
    pub reactive_power_kvar: f64,
    pub active_power_percent: f64,
    pub reactive_qs_percent: f64,
    pub power_factor: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DcCharger {
    pub battery_voltage_v: f64,
    pub current_a: f64,
    pub output_power_kw: f64,
    pub vehicle_soc_percent: f64,
    pub session_energy_kwh: f64,
    pub session_duration_seconds: u32,
}

/// EVAC-only telemetry (§5.5); use the charger's own configured slave ID.
#[derive(Clone, Debug, PartialEq)]
pub struct AcCharger {
    /// Raw appendix 7 state, preserving unknown values.
    pub state: u16,
    pub total_energy_kwh: f64,
    pub power_kw: f64,
    pub rated_power_kw: f64,
    pub rated_current_a: f64,
    pub rated_voltage_v: f64,
    pub breaker_rated_current_a: f64,
    /// Appendix 8, 9, 10 bitfields.
    pub alarms: [u16; 3],
}

impl Client {
    pub fn read_plant_phase_power(&mut self) -> Result<PhasePower> {
        let r = self.telemetry(UnitId::PLANT, 30015, 12)?;
        Ok(phase_power(&r))
    }

    /// Grid sensor powers: positive active power imports, negative exports.
    pub fn read_grid_phase_power(&mut self) -> Result<PhasePower> {
        let r = self.telemetry(UnitId::PLANT, 30052, 12)?;
        Ok(phase_power(&r))
    }

    /// Aggregated plant alarms in appendix 2, 3, 4, 5, 11 order.
    pub fn read_plant_alarms(&mut self) -> Result<[u16; 5]> {
        let r = self.telemetry(UnitId::PLANT, 30027, 4)?;
        let fifth = self.telemetry(UnitId::PLANT, 30072, 1)?;
        Ok([r[0], r[1], r[2], r[3], fifth[0]])
    }

    pub fn read_plant_capabilities(&mut self) -> Result<PlantCapabilities> {
        let rated = self.telemetry(UnitId::PLANT, 30010, 4)?;
        let r = self.telemetry(UnitId::PLANT, 30039, 12)?;
        Ok(PlantCapabilities {
            max_active_power_kw: unsigned(&rated, 0, 1000.0),
            max_apparent_power: unsigned(&rated, 2, 1000.0),
            available_export_power_kw: unsigned(&r, 0, 1000.0),
            available_import_power_kw: unsigned(&r, 2, 1000.0),
            available_reactive_export_kvar: unsigned(&r, 4, 1000.0),
            available_reactive_import_kvar: unsigned(&r, 6, 1000.0),
            available_charge_power_kw: unsigned(&r, 8, 1000.0),
            available_discharge_power_kw: unsigned(&r, 10, 1000.0),
        })
    }

    pub fn read_plant_battery(&mut self) -> Result<PlantBattery> {
        let r = self.telemetry(UnitId::PLANT, 30064, 8)?;
        let b = self.telemetry(UnitId::PLANT, 30083, 5)?;
        Ok(PlantBattery {
            available_charge_energy_kwh: unsigned(&r, 0, 100.0),
            available_discharge_energy_kwh: unsigned(&r, 2, 100.0),
            rated_charge_power_kw: unsigned(&r, 4, 1000.0),
            rated_discharge_power_kw: unsigned(&r, 6, 1000.0),
            rated_energy_kwh: unsigned(&b, 0, 100.0),
            charge_cutoff_soc_percent: b[2] as f64 / 10.0,
            discharge_cutoff_soc_percent: b[3] as f64 / 10.0,
            soh_percent: b[4] as f64 / 10.0,
        })
    }

    pub fn read_plant_energy(&mut self, counter: PlantEnergyCounter) -> Result<u64> {
        self.read_energy_counter(UnitId::PLANT, counter as u16)
    }

    /// Exact 0.01 kWh units.
    pub fn read_daily_load_energy(&mut self) -> Result<u32> {
        let r = self.telemetry(UnitId::PLANT, 30092, 2)?;
        Ok(u32_at(&r, 0))
    }

    pub fn read_third_party_power_kw(&mut self) -> Result<f64> {
        let r = self.telemetry(UnitId::PLANT, 30194, 2)?;
        Ok(signed(&r, 0, 1000.0))
    }

    /// Select one of the 24 documented smart loads (numbered 1–24).
    pub fn read_smart_load(&mut self, number: u8) -> Result<SmartLoad> {
        if !(1..=24).contains(&number) {
            return Err(Error::InvalidArgument("smart load number must be 1..=24"));
        }
        let offset = u16::from(number - 1) * 2;
        let energy = self.telemetry(UnitId::PLANT, 30098 + offset, 2)?;
        let power = self.telemetry(UnitId::PLANT, 30146 + offset, 2)?;
        Ok(SmartLoad {
            total_energy: u32_at(&energy, 0),
            power_kw: signed(&power, 0, 1000.0),
        })
    }

    pub fn read_battery_ratings(&mut self, unit: UnitId) -> Result<BatteryRatings> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 30546, 8)?;
        Ok(BatteryRatings {
            max_absorption_power_kw: unsigned(&r, 0, 1000.0),
            rated_energy_kwh: unsigned(&r, 2, 100.0),
            rated_charge_power_kw: unsigned(&r, 4, 1000.0),
            rated_discharge_power_kw: unsigned(&r, 6, 1000.0),
        })
    }

    pub fn read_battery_energy(&mut self, unit: UnitId) -> Result<BatteryEnergy> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 30566, 12)?;
        Ok(BatteryEnergy {
            daily_charge: u32_at(&r, 0),
            total_charge: u64_at(&r, 2),
            daily_discharge: u32_at(&r, 6),
            total_discharge: u64_at(&r, 8),
        })
    }

    pub fn read_pv_energy(&mut self, unit: UnitId) -> Result<PvEnergy> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 31509, 4)?;
        Ok(PvEnergy {
            daily: u32_at(&r, 0),
            total: u32_at(&r, 2),
        })
    }

    pub fn read_electrical(&mut self, unit: UnitId) -> Result<Electrical> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 31000, 24)?;
        Ok(Electrical {
            rated_grid_voltage_v: r[0] as f64 / 10.0,
            rated_grid_frequency_hz: r[1] as f64 / 100.0,
            grid_frequency_hz: r[2] as f64 / 100.0,
            internal_temperature_c: r[3] as i16 as f64 / 10.0,
            output_type: r[4],
            line_voltage_v: std::array::from_fn(|i| unsigned(&r, 5 + i * 2, 100.0)),
            phase_voltage_v: std::array::from_fn(|i| unsigned(&r, 11 + i * 2, 100.0)),
            phase_current_a: std::array::from_fn(|i| signed(&r, 17 + i * 2, 100.0)),
            power_factor: r[23] as f64 / 1000.0,
        })
    }

    /// Caller must check the model's PV count. Strings 8–9 are rejected because
    /// V2.7 duplicates PV8 and omits a PV9 voltage label at 31048–31051.
    pub fn read_pv_string(&mut self, unit: UnitId, number: u8) -> Result<PvString> {
        require_inverter(unit)?;
        let address = match number {
            1..=4 => 31027 + u16::from(number - 1) * 2,
            5..=7 => 31042 + u16::from(number - 5) * 2,
            10..=16 => 31052 + u16::from(number - 10) * 2,
            _ => {
                return Err(Error::InvalidArgument(
                    "PV string must be 1..=7 or 10..=16; V2.7 strings 8–9 are ambiguous",
                ));
            }
        };
        let r = self.telemetry(unit, address, 2)?;
        Ok(PvString {
            voltage_v: r[0] as i16 as f64 / 10.0,
            current_a: r[1] as i16 as f64 / 100.0,
        })
    }

    pub fn read_inverter_diagnostics(&mut self, unit: UnitId) -> Result<InverterDiagnostics> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 31037, 5)?;
        Ok(InverterDiagnostics {
            insulation_resistance_megohms: r[0] as f64 / 1000.0,
            startup_epoch_seconds: u32_at(&r, 1),
            shutdown_epoch_seconds: u32_at(&r, 3),
        })
    }

    pub fn read_power_adjustment_feedback(
        &mut self,
        unit: UnitId,
    ) -> Result<PowerAdjustmentFeedback> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 30613, 7)?;
        Ok(PowerAdjustmentFeedback {
            active_power_kw: signed(&r, 0, 1000.0),
            reactive_power_kvar: signed(&r, 2, 1000.0),
            active_power_percent: r[4] as i16 as f64 / 100.0,
            reactive_qs_percent: r[5] as i16 as f64 / 100.0,
            power_factor: r[6] as i16 as f64 / 1000.0,
        })
    }

    /// Hybrid-only DC charger readings, addressed through the inverter.
    pub fn read_dc_charger(&mut self, unit: UnitId) -> Result<DcCharger> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 31500, 9)?;
        Ok(DcCharger {
            battery_voltage_v: r[0] as f64 / 10.0,
            current_a: r[1] as f64 / 10.0,
            output_power_kw: signed(&r, 2, 1000.0),
            vehicle_soc_percent: r[4] as f64 / 10.0,
            session_energy_kwh: unsigned(&r, 5, 100.0),
            session_duration_seconds: u32_at(&r, 7),
        })
    }

    pub fn read_ac_charger(&mut self, unit: UnitId) -> Result<AcCharger> {
        require_inverter(unit)?;
        let r = self.telemetry(unit, 32000, 15)?;
        Ok(AcCharger {
            state: r[0],
            total_energy_kwh: unsigned(&r, 1, 100.0),
            power_kw: signed(&r, 3, 1000.0),
            rated_power_kw: unsigned(&r, 5, 1000.0),
            rated_current_a: signed(&r, 7, 100.0),
            rated_voltage_v: r[9] as f64 / 10.0,
            breaker_rated_current_a: signed(&r, 10, 100.0),
            alarms: [r[12], r[13], r[14]],
        })
    }
}

fn phase_power(r: &[u16]) -> PhasePower {
    PhasePower {
        active_kw: std::array::from_fn(|i| signed(r, i * 2, 1000.0)),
        reactive_kvar: std::array::from_fn(|i| signed(r, 6 + i * 2, 1000.0)),
    }
}
fn u64_at(r: &[u16], i: usize) -> u64 {
    (u64::from(u32_at(r, i)) << 32) | u64::from(u32_at(r, i + 2))
}
