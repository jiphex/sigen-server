use super::{Config, Scenario};
use std::{
    collections::BTreeMap,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

type Registers = BTreeMap<u16, u16>;

pub(super) struct Model {
    config: Config,
    started: Instant,
    updated: Instant,
    day: u64,
    epoch_started: u32,
    soc: f64,
    daily: [f64; 6],
    totals: [f64; 6],
    plant_settings: Registers,
    inverter_settings: Registers,
}
struct Power {
    pv: f64,
    battery: f64,
    grid: f64,
    load: f64,
    fault: bool,
    off_grid: bool,
}
impl Model {
    pub fn new(config: Config) -> Self {
        let mut plant_settings = Registers::new();
        for (address, value) in [
            (40000, 1),
            (40029, 0),
            (40030, 0),
            (40031, 1),
            (40046, 200),
            (40047, 1000),
            (40048, 50),
        ] {
            plant_settings.insert(address, value);
        }
        for (address, value) in [
            (40001, 10000),
            (40003, 0),
            (40032, 5000),
            (40034, 5000),
            (40036, 12000),
            (40038, 100000),
            (40040, 100000),
            (40042, u32::MAX),
            (40044, u32::MAX),
        ] {
            u32_value(&mut plant_settings, address, value);
        }
        plant_settings.extend([(40005, 10000), (40006, 0), (40007, 1000)]);
        let mut inverter_settings = Registers::new();
        inverter_settings.extend([
            (40500, 1),
            (41500, 0),
            (41505, 10000),
            (41506, 0),
            (41507, 1000),
        ]);
        u32_value(&mut inverter_settings, 41501, 10000);
        u32_value(&mut inverter_settings, 41503, 0);
        let now = epoch();
        let soc = if config.scenario == Scenario::Night {
            48.2
        } else {
            73.6
        };
        Self {
            config,
            started: Instant::now(),
            updated: Instant::now(),
            day: now / 86400,
            epoch_started: now as u32,
            soc,
            daily: [14.82, 11.37, 6.24, 4.18, 3.2, 7.1],
            totals: [2100.0, 3200.0, 1200.0, 1000.0, 1800.0, 2500.0],
            plant_settings,
            inverter_settings,
        }
    }
    fn scenario(&self) -> Scenario {
        if self.config.scenario != Scenario::Cycle {
            return self.config.scenario;
        }
        [
            Scenario::Solar,
            Scenario::Night,
            Scenario::BatteryAlarm,
            Scenario::GridOutage,
        ][(self.started.elapsed().as_secs() / self.config.cycle_seconds % 4) as usize]
    }
    /// Derive instantaneous flows from the scenario and writable EMS settings.
    /// Positive battery power charges; positive grid power imports, balancing the load.
    fn power(&self) -> Power {
        let scenario = self.scenario();
        let wave = (self.started.elapsed().as_secs_f64() / 12.0).sin();
        let fault = scenario == Scenario::ThermalRunaway;
        let off_grid = scenario == Scenario::GridOutage;
        let running =
            self.plant_settings[&40000] == 1 && self.inverter_settings[&40500] == 1 && !fault;
        let load = 1800.0 + 150.0 * wave;
        let pv = if running && scenario != Scenario::Night {
            (5200.0 + 350.0 * wave).min(self.setting32(40036) as f64)
        } else {
            0.0
        };
        let mut battery = if scenario == Scenario::Night {
            -1800.0
        } else {
            1600.0
        };
        if self.plant_settings[&40029] == 1 {
            battery = match self.plant_settings[&40031] {
                0 => pv - self.setting32(40001) as i32 as f64,
                1 => 0.0,
                2 => pv - load,
                3 => self.setting32(40032) as f64,
                4 => (pv - load).max(0.0),
                5 | 6 => -(self.setting32(40034) as f64),
                _ => 0.0,
            };
        }
        if off_grid {
            battery = pv - load;
        }
        battery = battery.clamp(
            -(self.setting32(40034) as f64),
            self.setting32(40032) as f64,
        );
        let reserve = self.plant_settings[&40046].max(self.plant_settings[&40048]) as f64 / 10.0;
        if self.soc <= reserve {
            battery = battery.max(0.0);
        }
        if self.soc >= self.plant_settings[&40047] as f64 / 10.0 {
            battery = battery.min(0.0);
        }
        if !running {
            battery = 0.0;
        }
        Power {
            pv,
            battery,
            grid: if off_grid { 0.0 } else { load + battery - pv },
            load,
            fault,
            off_grid,
        }
    }
    fn setting32(&self, address: u16) -> u32 {
        (u32::from(self.plant_settings[&address]) << 16)
            | u32::from(self.plant_settings[&(address + 1)])
    }
    /// Integrate the current power over elapsed real time before reads or setting changes.
    /// Daily counters reset on the simulator's UTC day boundary; totals keep accumulating.
    fn tick(&mut self) {
        let elapsed = self.updated.elapsed().as_secs_f64();
        self.updated = Instant::now();
        let day = epoch() / 86400;
        if day != self.day {
            self.daily = [0.0; 6];
            self.day = day;
        }
        let p = self.power();
        // One 20 kWh cluster; time progresses independently of request frequency.
        self.soc = (self.soc + p.battery * elapsed / 720000.0).clamp(0.0, 100.0);
        let rates = [
            p.pv,
            p.load,
            p.battery.max(0.0),
            (-p.battery).max(0.0),
            p.grid.max(0.0),
            (-p.grid).max(0.0),
        ];
        for (i, rate) in rates.into_iter().enumerate() {
            let kwh = rate * elapsed / 3_600_000.0;
            self.daily[i] += kwh;
            self.totals[i] += kwh;
        }
    }
    pub fn read(&mut self, unit: u8, address: u16, count: u16) -> Result<Vec<u16>, u8> {
        self.tick();
        let map = if unit == 247 {
            self.plant_registers()
        } else if unit == self.config.unit {
            self.inverter_registers()
        } else {
            return Err(11);
        };
        let settings = if unit == 247 {
            &self.plant_settings
        } else {
            &self.inverter_settings
        };
        (0..count)
            .map(|offset| {
                let a = address + offset;
                if matches!(a, 40000 | 40500) {
                    return Err(2);
                } // write-only commands
                map.get(&a).or_else(|| settings.get(&a)).copied().ok_or(2)
            })
            .collect()
    }
    /// Validate the whole request before applying settings, preventing partial multiword writes.
    pub fn write(&mut self, unit: u8, address: u16, values: &[u16]) -> Result<(), u8> {
        self.tick();
        let plant = unit == 247;
        if !plant && unit != self.config.unit {
            return Err(11);
        }
        // Validate every complete field before mutating anything, including FC16.
        let mut index = 0;
        while index < values.len() {
            let a = address + index as u16;
            let width = if plant {
                match a {
                    40000 | 40005..=40007 | 40029..=40031 | 40046..=40048 => 1,
                    40001 | 40003 | 40032 | 40034 | 40036 | 40038 | 40040 | 40042 | 40044 => 2,
                    _ => return Err(2),
                }
            } else {
                match a {
                    40500 | 41500 | 41505..=41507 => 1,
                    41501 | 41503 => 2,
                    _ => return Err(2),
                }
            };
            if index + width > values.len() {
                return Err(2);
            }
            let value = values[index];
            let valid = match a {
                40000 | 40029 | 40030 | 40500 | 41500 => value <= 1,
                40031 => value <= 6,
                40046..=40048 => value <= 1000,
                40005 | 41505 => (-10000..=10000).contains(&(value as i16)),
                40006 | 41506 => (-6000..=6000).contains(&(value as i16)),
                40007 | 41507 => (800..=1000).contains(&(value as i16).unsigned_abs()),
                40032 | 40034 => ((u32::from(value) << 16) | u32::from(values[index + 1])) <= 5000,
                _ => true,
            };
            if !valid {
                return Err(3);
            }
            index += width;
        }
        let settings = if plant {
            &mut self.plant_settings
        } else {
            &mut self.inverter_settings
        };
        for (i, value) in values.iter().enumerate() {
            settings.insert(address + i as u16, *value);
        }
        Ok(())
    }
    fn plant_registers(&self) -> Registers {
        let mut r = Registers::new();
        let p = self.power();
        u32_value(&mut r, 30000, epoch() as u32);
        r.extend([
            (30002, 0),
            (
                30003,
                if self.plant_settings[&40029] == 1 {
                    7
                } else {
                    0
                },
            ),
            (30004, 1),
            (30009, u16::from(p.off_grid)),
            (30014, (self.soc * 10.0).round() as u16),
        ]);
        scaled32(&mut r, 30005, p.grid);
        u32_value(&mut r, 30007, 0);
        u32_value(&mut r, 30010, 10000);
        u32_value(&mut r, 30012, 11000);
        for i in 0..3 {
            scaled32(&mut r, 30015 + i * 2, (p.pv - p.battery) / 3.0);
            u32_value(&mut r, 30021 + i * 2, 0);
            scaled32(&mut r, 30052 + i * 2, p.grid / 3.0);
            u32_value(&mut r, 30058 + i * 2, 0);
        }
        let alarms = self.alarms();
        for (address, value) in [30027, 30028, 30029, 30030, 30072].into_iter().zip(alarms) {
            r.insert(address, value);
        }
        scaled32(&mut r, 30031, p.pv - p.battery);
        u32_value(&mut r, 30033, 0);
        scaled32(&mut r, 30035, p.pv);
        scaled32(&mut r, 30037, p.battery);
        for (address, value) in [
            (30039, 10000),
            (30041, 10000),
            (30043, 6000),
            (30045, 6000),
            (30047, 5000),
            (30049, 5000),
            (30064, ((100.0 - self.soc) * 20.0) as u32),
            (30066, (self.soc * 20.0) as u32),
            (30068, 5000),
            (30070, 5000),
            (30083, 2000),
        ] {
            u32_value(&mut r, address, value);
        }
        r.extend([
            (30051, self.running_state()),
            (30085, self.plant_settings[&40047]),
            (30086, self.plant_settings[&40048]),
            (30087, 994),
        ]);
        energy64(&mut r, 30088, self.totals[0]);
        u32_value(&mut r, 30092, hundredths(self.daily[1]) as u32);
        energy64(&mut r, 30094, self.totals[1]);
        for i in 0..24 {
            u32_value(
                &mut r,
                30098 + i * 2,
                hundredths(self.totals[1] / 24.0) as u32,
            );
            scaled32(&mut r, 30146 + i * 2, p.load / 24.0);
        }
        u32_value(&mut r, 30194, 0);
        for (address, value) in [
            (30196, 0.0),
            (30200, self.totals[2]),
            (30204, self.totals[3]),
            (30208, 0.0),
            (30212, 0.0),
            (30216, self.totals[4]),
            (30220, self.totals[5]),
            (30224, 0.0),
            (30228, self.totals[1]),
            (30232, 0.0),
            (30236, self.totals[0]),
            (30240, 0.0),
            (30244, self.totals[2]),
            (30248, self.totals[3]),
            (30252, 0.0),
            (30256, 0.0),
            (30260, self.totals[4]),
            (30264, self.totals[5]),
            (30268, 0.0),
        ] {
            energy64(&mut r, address, value);
        }
        r
    }
    fn running_state(&self) -> u16 {
        if self.power().fault {
            2
        } else if self.plant_settings[&40000] == 0 || self.inverter_settings[&40500] == 0 {
            3
        } else {
            1
        }
    }
    fn alarms(&self) -> [u16; 5] {
        [
            0,
            0,
            match self.scenario() {
                Scenario::BatteryAlarm => 2,
                Scenario::ThermalRunaway => 64,
                _ => 0,
            },
            0,
            0,
        ]
    }
    fn inverter_registers(&self) -> Registers {
        let mut r = Registers::new();
        let p = self.power();
        let wave = (self.started.elapsed().as_secs_f64() / 12.0).sin();
        ascii(&mut r, 30500, 15, "SigenStor SIMULATOR");
        ascii(
            &mut r,
            30515,
            10,
            &format!("SIMULATED-{:06}", self.config.unit),
        );
        ascii(&mut r, 30525, 15, "SIM V2.7");
        for (a, v) in [
            (30540, 10000),
            (30542, 11000),
            (30544, 10000),
            (30546, 10000),
            (30548, 2000),
            (30550, 5000),
            (30552, 5000),
            (30579, 10000),
            (30581, (-10000_i32) as u32),
            (30583, 6000),
            (30585, 6000),
            (30591, 5000),
            (30593, 5000),
            (30595, ((100.0 - self.soc) * 20.0) as u32),
            (30597, (self.soc * 20.0) as u32),
        ] {
            u32_value(&mut r, a, v);
        }
        u32_value(&mut r, 30566, hundredths(self.daily[2]) as u32);
        energy64(&mut r, 30568, self.totals[2]);
        u32_value(&mut r, 30572, hundredths(self.daily[3]) as u32);
        energy64(&mut r, 30574, self.totals[3]);
        r.insert(30578, self.running_state());
        scaled32(&mut r, 30587, p.pv - p.battery);
        u32_value(&mut r, 30589, 0);
        scaled32(&mut r, 30599, p.battery);
        let temp = if p.fault {
            850
        } else {
            (265.0 + wave * 4.0) as u16
        };
        r.extend([
            (30601, (self.soc * 10.0).round() as u16),
            (30602, 994),
            (30603, temp),
            (30604, 3320),
            (30620, temp + 7),
            (30621, temp - 7),
            (30622, 3340),
            (30623, 3300),
        ]);
        for (i, alarm) in self.alarms().into_iter().enumerate() {
            r.insert(30605 + i as u16, alarm);
        }
        for a in 0..7 {
            r.insert(30613 + a, self.plant_settings[&(40001 + a)]);
        }
        r.extend([
            (31000, 2300),
            (31001, 5000),
            (31002, (5000.0 + wave * 2.0) as u16),
            (31003, (382.0 + wave * 6.0) as u16),
            (31004, 2),
            (31023, 999),
            (31024, 2),
            (31025, 4),
            (31026, 2),
            (31037, 10000),
        ]);
        for i in 0..3 {
            u32_value(&mut r, 31005 + i * 2, 40000);
            u32_value(&mut r, 31011 + i * 2, 23000 + i as u32 * 30);
            scaled32(
                &mut r,
                31017 + i * 2,
                (p.pv - p.battery) / (3.0 * 230.0) * 100.0,
            );
        }
        for i in 0..4 {
            r.insert(31027 + i * 2, 4000);
            r.insert(31028 + i * 2, (p.pv / 4.0 / 400.0 * 100.0) as u16);
        }
        u32_value(&mut r, 31035, p.pv.round() as u32);
        u32_value(&mut r, 31038, self.epoch_started);
        u32_value(&mut r, 31040, 0);
        // Attached DC charger is present but idle; PV counters are U32, not U64.
        r.extend([(31500, 4000), (31501, 0), (31504, 620)]);
        for a in [31502, 31505, 31507] {
            u32_value(&mut r, a, 0);
        }
        u32_value(&mut r, 31509, hundredths(self.daily[0]) as u32);
        u32_value(&mut r, 31511, hundredths(self.totals[0]) as u32);
        r
    }
}
fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn hundredths(kwh: f64) -> u64 {
    (kwh * 100.0).floor() as u64
}
fn u32_value(r: &mut Registers, address: u16, value: u32) {
    r.insert(address, (value >> 16) as u16);
    r.insert(address + 1, value as u16);
}
fn scaled32(r: &mut Registers, address: u16, value: f64) {
    u32_value(r, address, (value.round() as i32) as u32);
}
fn energy64(r: &mut Registers, address: u16, kwh: f64) {
    let value = hundredths(kwh);
    for i in 0..4 {
        r.insert(address + i, (value >> (48 - i * 16)) as u16);
    }
}
fn ascii(r: &mut Registers, address: u16, count: u16, text: &str) {
    let mut bytes = vec![0; count as usize * 2];
    let n = bytes.len().min(text.len());
    bytes[..n].copy_from_slice(&text.as_bytes()[..n]);
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        r.insert(address + i as u16, u16::from_be_bytes([pair[0], pair[1]]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn energy_and_soc_advance_with_elapsed_time_and_daily_counters_roll_over() {
        let mut model = Model::new(Config::default());
        model.updated = Instant::now() - Duration::from_secs(360);
        model.tick();
        assert!((model.soc - 74.4).abs() < 0.01);
        assert!((model.daily[2] - 6.4).abs() < 0.001);
        assert!((model.totals[2] - 1200.16).abs() < 0.001);
        let total = model.totals[0];
        model.day -= 1;
        model.tick();
        assert!(model.daily.iter().all(|energy| *energy < 0.001));
        assert!(model.totals[0] >= total);
    }

    #[test]
    fn cycle_scenarios_raise_and_clear_alarm_and_grid_states() {
        let mut model = Model::new(Config {
            scenario: Scenario::Cycle,
            cycle_seconds: 1,
            ..Config::default()
        });
        for (elapsed, scenario, alarm, grid) in [
            (0, Scenario::Solar, 0, 0),
            (1, Scenario::Night, 0, 0),
            (2, Scenario::BatteryAlarm, 2, 0),
            (3, Scenario::GridOutage, 0, 1),
            (4, Scenario::Solar, 0, 0),
        ] {
            model.started = Instant::now() - Duration::from_secs(elapsed);
            assert_eq!(model.scenario(), scenario);
            assert_eq!(model.read(247, 30029, 1).unwrap(), [alarm]);
            assert_eq!(model.read(247, 30009, 1).unwrap(), [grid]);
        }
    }
}
