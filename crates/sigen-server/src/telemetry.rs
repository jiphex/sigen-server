//! Optional telemetry uses spare time between core polls, one request at a time.
use crate::hardware::{SharedStatus, now};
use serde::Serialize;
use sigen_modbus::{Client, Config, Error, PlantEnergyCounter, UnitId};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize)]
pub struct Sample<T> {
    pub value: Option<T>,
    pub updated_unix: Option<u64>,
    pub device_day: Option<i64>,
    pub last_error: Option<String>,
    pub unsupported: bool,
}
impl<T> Default for Sample<T> {
    fn default() -> Self {
        Self {
            value: None,
            updated_unix: None,
            device_day: None,
            last_error: None,
            unsupported: false,
        }
    }
}
impl<T> Sample<T> {
    /// Remove unusable values from an export copy, keeping timestamps and error diagnostics.
    /// Daily counters also expire when the device-local day changes.
    fn retain_current(&mut self, timestamp: u64, max_age: u64, day: Option<i64>, daily: bool) {
        if self.unsupported
            || self.last_error.is_some()
            || !self
                .updated_unix
                .is_some_and(|t| timestamp.saturating_sub(t) <= max_age)
            || (daily && (day.is_none() || self.device_day != day))
        {
            self.value = None;
        }
    }

    /// Preserve the last successful value on failure; report whether reconnection is needed.
    fn record(&mut self, result: sigen_modbus::Result<T>, day: Option<i64>) -> bool {
        match result {
            Ok(value) => {
                self.value = Some(value);
                self.updated_unix = Some(now());
                self.device_day = day;
                self.last_error = None;
                self.unsupported = false;
                false
            }
            Err(error) => {
                self.unsupported = matches!(error, Error::Exception(2));
                // Device exceptions leave the TCP connection usable.
                let disconnected = matches!(
                    error,
                    Error::Io(_) | Error::Protocol(_) | Error::Disconnected
                );
                self.last_error = Some(error.to_string());
                disconnected
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DailyBattery {
    pub charge_kwh: f64,
    pub discharge_kwh: f64,
}
#[derive(Clone, Debug)]
pub struct Electrical {
    pub output_type: u16,
    pub phase_voltage_volts: [f64; 3],
    pub phase_current_amps: [f64; 3],
    pub power_factor: f64,
}
impl Serialize for Electrical {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let volts: [Option<f64>; 3] = std::array::from_fn(|i| {
            (self.output_type == 2 || (self.output_type == 0 && i == 0))
                .then_some(self.phase_voltage_volts[i])
        });
        let amps: [Option<f64>; 3] = std::array::from_fn(|i| {
            (matches!(self.output_type, 1 | 2) || (self.output_type == 0 && i == 0))
                .then_some(self.phase_current_amps[i])
        });
        let mut out = serializer.serialize_struct("Electrical", 4)?;
        out.serialize_field("output_type", &self.output_type)?;
        out.serialize_field("phase_voltage_volts", &volts)?;
        out.serialize_field("phase_current_amps", &amps)?;
        out.serialize_field("power_factor", &self.power_factor)?;
        out.end()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PhasePower {
    pub active_watts: [f64; 3],
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Details {
    /// Inverter PV daily generation; distinct from plant-wide consumption.
    pub pv_daily_kwh: Sample<f64>,
    pub load_daily_kwh: Sample<f64>,
    pub battery_daily: Sample<DailyBattery>,
    /// Exact decimal strings in 0.01 kWh units, avoiding JSON U64 precision loss.
    pub grid_import: Sample<String>,
    pub grid_export: Sample<String>,
    pub electrical: Sample<Electrical>,
    pub grid_phases: Sample<PhasePower>,
}

impl Details {
    /// Current measurements for machine consumers; retain diagnostics for missing values.
    /// Callers must also gate on core hardware freshness.
    pub fn for_export(&self, timestamp: u64, day: Option<i64>, grid_connected: bool) -> Self {
        let mut details = self.clone();
        details
            .pv_daily_kwh
            .retain_current(timestamp, 600, day, true);
        details
            .load_daily_kwh
            .retain_current(timestamp, 600, day, true);
        details
            .battery_daily
            .retain_current(timestamp, 600, day, true);
        details
            .grid_import
            .retain_current(timestamp, 600, day, false);
        details
            .grid_export
            .retain_current(timestamp, 600, day, false);
        details
            .electrical
            .retain_current(timestamp, 120, day, false);
        details
            .grid_phases
            .retain_current(timestamp, 120, day, false);
        if let Some(e) = &details.electrical.value
            && e.output_type > 3
        {
            details.electrical.last_error =
                Some(format!("Invalid electrical output type: {}", e.output_type));
            details.electrical.value = None;
        }
        if !grid_connected {
            details.grid_phases.value = None;
        }
        details
    }
}

pub struct Poller {
    due: [Instant; 7],
    cursor: usize,
}
impl Poller {
    pub fn new() -> Self {
        Self {
            due: [Instant::now(); 7],
            cursor: 0,
        }
    }

    /// Returns true if the optional read broke the connection. Never attempts writes.
    /// Read at most one due group, rotating fairly and leaving time for the next core poll.
    pub fn poll_if_due(
        &mut self,
        client: &mut Client,
        unit: UnitId,
        config: &Config,
        next_core_poll: Instant,
        status: &SharedStatus,
    ) -> bool {
        let started = Instant::now();
        // Reserve enough time for request pacing plus one complete response.
        let budget = config
            .request_interval
            .saturating_add(config.response_timeout);
        if next_core_poll.saturating_duration_since(started) <= budget
            || !status.read().expect("status lock").fresh()
        {
            return false;
        }
        let Some(index) = (0..7)
            .map(|offset| (self.cursor + offset) % 7)
            .find(|&i| started >= self.due[i])
        else {
            return false;
        };
        self.cursor = (index + 1) % 7;
        // Reprobe unavailable groups at the normal bounded interval. An exception
        // is evidence about this request, never a permanent capability verdict.
        self.due[index] = started + Duration::from_secs(if index >= 5 { 60 } else { 300 });
        let day = status.read().expect("status lock").device_day;
        // Complete the blocking read before taking the write lock.
        macro_rules! read {
            ($field:ident, $result:expr) => {{
                let result = $result;
                status
                    .write()
                    .expect("status lock")
                    .details
                    .$field
                    .record(result, day)
            }};
        }
        let disconnected = match index {
            0 => read!(
                pv_daily_kwh,
                client
                    .read_pv_energy(unit)
                    .map(|r| f64::from(r.daily) / 100.0)
            ),
            1 => read!(
                load_daily_kwh,
                client
                    .read_daily_load_energy()
                    .map(|r| f64::from(r) / 100.0)
            ),
            2 => read!(
                battery_daily,
                client.read_battery_energy(unit).map(|r| DailyBattery {
                    charge_kwh: f64::from(r.daily_charge) / 100.0,
                    discharge_kwh: f64::from(r.daily_discharge) / 100.0,
                })
            ),
            3 => read!(
                grid_import,
                client
                    .read_plant_energy(PlantEnergyCounter::NewGridImport)
                    .map(|r| r.to_string())
            ),
            4 => read!(
                grid_export,
                client
                    .read_plant_energy(PlantEnergyCounter::NewGridExport)
                    .map(|r| r.to_string())
            ),
            5 => read!(
                electrical,
                client.read_electrical(unit).map(|r| Electrical {
                    output_type: r.output_type,
                    phase_voltage_volts: r.phase_voltage_v,
                    phase_current_amps: r.phase_current_a,
                    power_factor: r.power_factor,
                })
            ),
            _ => read!(
                grid_phases,
                client.read_grid_phase_power().map(|r| PhasePower {
                    active_watts: r.active_kw.map(|kw| kw * 1000.0),
                })
            ),
        };
        if disconnected {
            let mut s = status.write().expect("status lock");
            s.connection_event(false, Some("Optional telemetry lost the Modbus connection"));
            s.online = false;
            s.last_error =
                Some("Optional telemetry lost the Modbus connection; reconnecting".into());
            s.invalidate_controls();
            crate::alerts::refresh(&mut s, now());
        }
        disconnected
    }
}

#[cfg(test)]
pub(crate) fn test_details(timestamp: u64, day: i64) -> Details {
    fn sample<T>(value: T, timestamp: u64, day: i64) -> Sample<T> {
        Sample {
            value: Some(value),
            updated_unix: Some(timestamp),
            device_day: Some(day),
            last_error: None,
            unsupported: false,
        }
    }
    Details {
        pv_daily_kwh: sample(14.82, timestamp, day),
        load_daily_kwh: sample(11.37, timestamp, day),
        battery_daily: sample(
            DailyBattery {
                charge_kwh: 6.24,
                discharge_kwh: 4.18,
            },
            timestamp,
            day,
        ),
        grid_import: sample(u64::MAX.to_string(), timestamp, day),
        grid_export: sample("0".into(), timestamp, day),
        electrical: sample(
            Electrical {
                output_type: 2,
                phase_voltage_volts: [230.0, 231.0, 232.0],
                phase_current_amps: [-1.0, 2.0, 3.0],
                power_factor: 0.999,
            },
            timestamp,
            day,
        ),
        grid_phases: sample(
            PhasePower {
                active_watts: [-1000.0, 2000.0, 3000.0],
            },
            timestamp,
            day,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, RwLock},
        thread,
    };

    #[test]
    fn electrical_exports_mask_inapplicable_phases_and_reject_unknown_wiring() {
        let mut details = test_details(1000, 20000);
        for output in 0..=3 {
            details.electrical.value.as_mut().unwrap().output_type = output;
            let exported = details.for_export(1000, Some(20000), true);
            let json = serde_json::to_value(exported).unwrap();
            for i in 0..3 {
                assert_eq!(
                    json["electrical"]["value"]["phase_voltage_volts"][i].is_number(),
                    output == 2 || (output == 0 && i == 0)
                );
                assert_eq!(
                    json["electrical"]["value"]["phase_current_amps"][i].is_number(),
                    matches!(output, 1 | 2) || (output == 0 && i == 0)
                );
            }
        }
        details.electrical.value.as_mut().unwrap().output_type = 65535;
        let exported = details.for_export(1000, Some(20000), true);
        assert!(exported.electrical.value.is_none());
        assert!(exported.electrical.last_error.unwrap().contains("65535"));
        assert_eq!(details.electrical.value.unwrap().output_type, 65535);
    }

    #[test]
    fn temporary_exceptions_do_not_become_permanent_capability_decisions() {
        for code in [1, 2, 3] {
            let mut sample = Sample::<u32>::default();
            assert!(!sample.record(Err(Error::Exception(code)), None));
            assert_eq!(sample.unsupported, code == 2);
            assert!(!sample.record(Ok(42), None));
            assert_eq!(sample.value, Some(42));
            assert!(!sample.unsupported);
            assert!(sample.last_error.is_none());
        }
    }

    #[test]
    fn exports_filter_samples_independently_and_preserve_diagnostics_and_cache() {
        let details = test_details(1000, 20000);
        let current = details.for_export(1120, Some(20000), true);
        assert_eq!(
            current.grid_phases.value.unwrap().active_watts,
            [-1000.0, 2000.0, 3000.0]
        );
        assert!(current.electrical.value.is_some());
        let expired = details.for_export(1121, Some(20000), true);
        assert!(expired.grid_phases.value.is_none());
        assert!(expired.electrical.value.is_none());
        assert!(expired.pv_daily_kwh.value.is_some());
        let midnight = details.for_export(1120, Some(20001), false);
        assert!(midnight.pv_daily_kwh.value.is_none());
        assert!(midnight.load_daily_kwh.value.is_none());
        assert!(midnight.battery_daily.value.is_none());
        assert!(midnight.grid_phases.value.is_none());
        assert!(midnight.grid_import.value.is_some());
        assert_eq!(midnight.pv_daily_kwh.updated_unix, Some(1000));
        assert!(details.pv_daily_kwh.value.is_some());
        assert!(
            details
                .for_export(1600, Some(20000), true)
                .grid_import
                .value
                .is_some()
        );
        assert!(
            details
                .for_export(1601, Some(20000), true)
                .grid_import
                .value
                .is_none()
        );
        assert!(
            details
                .for_export(1000, None, true)
                .pv_daily_kwh
                .value
                .is_none()
        );
    }

    fn shared() -> SharedStatus {
        let mut status = Status::new("127.0.0.1:502".into(), 1);
        status.online = true;
        status.device_day = Some(20000);
        status.control_valid_until = Some(Instant::now() + Duration::from_secs(120));
        Arc::new(RwLock::new(status))
    }

    // Each response also checks the literal unit, address and quantity on the wire.
    fn mock(responses: Vec<(u8, u16, u16, Vec<u8>)>) -> (Client, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for (unit, address, count, pdu) in responses {
                let mut request = [0; 12];
                stream.read_exact(&mut request).unwrap();
                assert_eq!(request[6], unit);
                assert_eq!(request[7], 4);
                assert_eq!(u16::from_be_bytes([request[8], request[9]]), address);
                assert_eq!(u16::from_be_bytes([request[10], request[11]]), count);
                let mut response = request[..7].to_vec();
                response[4..6].copy_from_slice(&((pdu.len() + 1) as u16).to_be_bytes());
                response.extend(pdu);
                stream.write_all(&response).unwrap();
            }
        });
        (Client::connect(address).unwrap(), worker)
    }
    fn words(values: &[u16]) -> Vec<u8> {
        let mut pdu = vec![4, (values.len() * 2) as u8];
        pdu.extend(values.iter().flat_map(|v| v.to_be_bytes()));
        pdu
    }
    fn poll(poller: &mut Poller, client: &mut Client, status: &SharedStatus) -> bool {
        poller.poll_if_due(
            client,
            UnitId::inverter(1).unwrap(),
            &Config::default(),
            Instant::now() + Duration::from_secs(60),
            status,
        )
    }

    #[test]
    fn optional_groups_decode_and_keep_separate_timestamps_without_changing_core_status() {
        let mut electrical = vec![0; 24];
        electrical[4] = 2;
        electrical[12] = 23000;
        electrical[18] = 123;
        electrical[23] = 999;
        let (mut client, worker) = mock(vec![
            (1, 31509, 4, words(&[0, 1482, 0, 30000])),
            (247, 30092, 2, words(&[0, 1137])),
            (
                1,
                30566,
                12,
                words(&[0, 624, 0, 0, 0, 0, 0, 418, 0, 0, 0, 0]),
            ),
            (247, 30260, 4, words(&[0xffff; 4])),
            (247, 30264, 4, words(&[0, 0, 0, 0])),
            (1, 31000, 24, words(&electrical)),
            (
                247,
                30052,
                12,
                words(&[0xffff, 64536, 0, 2000, 0, 3000, 0, 0, 0, 0, 0, 0]),
            ),
        ]);
        let shared = shared();
        let mut poller = Poller::new();
        for _ in 0..7 {
            assert!(!poll(&mut poller, &mut client, &shared));
        }
        worker.join().unwrap();
        let s = shared.read().unwrap();
        assert!(s.online);
        assert_eq!(s.poll_errors, 0);
        assert_eq!(s.details.pv_daily_kwh.value, Some(14.82));
        assert_eq!(s.details.load_daily_kwh.value, Some(11.37));
        let battery = s.details.battery_daily.value.as_ref().unwrap();
        assert_eq!(battery.charge_kwh, 6.24);
        assert_eq!(battery.discharge_kwh, 4.18);
        assert_eq!(
            s.details.grid_import.value.as_deref(),
            Some("18446744073709551615")
        );
        assert_eq!(s.details.grid_export.value.as_deref(), Some("0"));
        let e = s.details.electrical.value.as_ref().unwrap();
        assert_eq!(e.phase_voltage_volts[0], 230.0);
        assert_eq!(e.phase_current_amps[0], 1.23);
        assert_eq!(e.power_factor, 0.999);
        assert_eq!(
            s.details.grid_phases.value.as_ref().unwrap().active_watts,
            [-1000.0, 2000.0, 3000.0]
        );
        assert!(s.details.electrical.updated_unix.is_some());
        assert_eq!(s.details.pv_daily_kwh.device_day, Some(20000));
        assert!(poller.due[0] > poller.due[5]);
        let json = serde_json::to_value(s.snapshot()).unwrap();
        assert_eq!(
            json["details"]["grid_import"]["value"],
            "18446744073709551615"
        );
    }

    #[test]
    fn optional_reads_wait_for_core_poll_budget_and_unsupported_groups_are_skipped() {
        let (mut client, worker) = mock(vec![
            (1, 31509, 4, vec![0x84, 2]),
            (1, 31509, 4, words(&[0, 100, 0, 200])),
            (247, 30092, 2, words(&[0, 123])),
        ]);
        let shared = shared();
        let mut poller = Poller::new();
        assert!(!poller.poll_if_due(
            &mut client,
            UnitId::inverter(1).unwrap(),
            &Config::default(),
            Instant::now(),
            &shared
        ));
        assert_eq!(poller.cursor, 0);
        shared.write().unwrap().online = false;
        assert!(!poll(&mut poller, &mut client, &shared));
        assert_eq!(poller.cursor, 0);
        shared.write().unwrap().online = true;
        assert!(!poll(&mut poller, &mut client, &shared));
        assert!(shared.read().unwrap().details.pv_daily_kwh.unsupported);
        assert!(shared.read().unwrap().fresh());
        poller.cursor = 0;
        poller.due[0] = Instant::now();
        assert!(!poll(&mut poller, &mut client, &shared)); // reprobe succeeds
        assert!(!shared.read().unwrap().details.pv_daily_kwh.unsupported);
        assert!(!poll(&mut poller, &mut client, &shared)); // load still works
        assert_eq!(
            shared.read().unwrap().details.load_daily_kwh.value,
            Some(1.23)
        );
        worker.join().unwrap();
    }

    #[test]
    fn broken_optional_connection_invalidates_controls_and_preserves_last_known_value() {
        let (mut client, worker) = mock(vec![(1, 31509, 4, vec![4, 2, 0, 1])]); // wrong byte count
        let shared = shared();
        shared
            .write()
            .unwrap()
            .details
            .pv_daily_kwh
            .record(Ok(12.34), Some(20000));
        assert!(poll(&mut Poller::new(), &mut client, &shared));
        worker.join().unwrap();
        let s = shared.read().unwrap();
        assert!(!s.online);
        assert!(!s.fresh());
        assert!(s.controls.is_none());
        assert_eq!(s.control_epoch, 1);
        assert_eq!(s.details.pv_daily_kwh.value, Some(12.34));
        assert!(s.details.pv_daily_kwh.last_error.is_some());
        assert!(!s.details.pv_daily_kwh.unsupported);
    }
}
