use sigen_modbus::{Client, Error, PlantEnergyCounter, ReadFunction, RemoteEmsMode, UnitId};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Simulator {
    child: Child,
    address: SocketAddr,
}
impl Simulator {
    fn start(scenario: &str, unit: u8) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_sigen-simulator"))
            .args([
                "--bind",
                "127.0.0.1:0",
                "--scenario",
                scenario,
                "--unit-id",
                &unit.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line
            .split_whitespace()
            .nth(4)
            .expect("simulator readiness address")
            .parse()
            .unwrap();
        Self { child, address }
    }
    fn wire(&self) -> Wire {
        Wire::new(self.address)
    }
}
impl Drop for Simulator {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct Wire {
    stream: TcpStream,
    transaction: u16,
}
impl Wire {
    fn new(address: SocketAddr) -> Self {
        let stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        Self {
            stream,
            transaction: 0,
        }
    }
    fn frame(transaction: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut frame = transaction.to_be_bytes().to_vec();
        frame.extend([0, 0]);
        frame.extend(((pdu.len() + 1) as u16).to_be_bytes());
        frame.push(unit);
        frame.extend(pdu);
        frame
    }
    fn response(&mut self, transaction: u16, unit: u8) -> Vec<u8> {
        let mut header = [0; 7];
        self.stream.read_exact(&mut header).unwrap();
        assert_eq!(&header[..2], &transaction.to_be_bytes());
        assert_eq!(&header[2..4], &[0, 0]);
        assert_eq!(header[6], unit);
        let mut pdu = vec![0; u16::from_be_bytes([header[4], header[5]]) as usize - 1];
        self.stream.read_exact(&mut pdu).unwrap();
        pdu
    }
    fn request(&mut self, unit: u8, pdu: &[u8]) -> Vec<u8> {
        self.transaction += 1;
        self.stream
            .write_all(&Self::frame(self.transaction, unit, pdu))
            .unwrap();
        self.response(self.transaction, unit)
    }
    fn read(&mut self, unit: u8, function: u8, address: u16, count: u16) -> Vec<u16> {
        let mut pdu = vec![function];
        pdu.extend(address.to_be_bytes());
        pdu.extend(count.to_be_bytes());
        let response = self.request(unit, &pdu);
        assert_eq!(response[..2], [function, (count * 2) as u8]);
        response[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect()
    }
    fn write(&mut self, unit: u8, address: u16, value: u16) -> Vec<u8> {
        let mut pdu = vec![6];
        pdu.extend(address.to_be_bytes());
        pdu.extend(value.to_be_bytes());
        self.request(unit, &pdu)
    }
    fn multiple(&mut self, unit: u8, address: u16, values: &[u16]) -> Vec<u8> {
        let mut pdu = vec![16];
        pdu.extend(address.to_be_bytes());
        pdu.extend((values.len() as u16).to_be_bytes());
        pdu.push((values.len() * 2) as u8);
        pdu.extend(values.iter().flat_map(|v| v.to_be_bytes()));
        self.request(unit, &pdu)
    }
}

#[test]
fn real_client_reads_simulated_identity_core_and_battery_registers() {
    let sim = Simulator::start("solar", 7);
    let mut client = Client::connect(sim.address).unwrap();
    let unit = UnitId::inverter(7).unwrap();
    let device = client.read_device_info(unit).unwrap();
    assert!(device.model.contains("SIMULATOR"));
    assert_eq!(device.serial_number, "SIMULATED-000007");
    assert_eq!(device.rated_active_power_kw, 10.0);
    let p = client.read_plant().unwrap();
    assert!(p.epoch_seconds > 1_700_000_000);
    assert_eq!(p.timezone_offset_minutes, 0);
    assert!(p.grid_sensor_connected && p.pv_power_kw > 4.0);
    assert!(p.grid_power_kw < 0.0);
    let i = client.read_inverter(unit).unwrap();
    assert!((49.9..50.1).contains(&i.grid_frequency_hz));
    assert!(i.active_power_kw > 0.0);
    let b = client.read_battery(unit).unwrap();
    assert!(b.soc_percent > 70.0 && b.power_kw > 0.0);
    assert_eq!(b.alarms, [0; 5]);
    assert!(b.min_temperature_c < b.max_temperature_c);
    assert!(matches!(
        client.read_inverter(UnitId::inverter(1).unwrap()),
        Err(Error::Exception(11))
    ));
}

#[test]
fn real_client_reads_all_optional_server_groups_and_other_typed_helpers() {
    let sim = Simulator::start("night", 1);
    let mut client = Client::connect(sim.address).unwrap();
    let unit = UnitId::inverter(1).unwrap();
    assert!(client.read_pv_energy(unit).unwrap().daily > 0);
    assert!(client.read_daily_load_energy().unwrap() > 0);
    assert!(client.read_battery_energy(unit).unwrap().total_discharge > 0);
    assert!(
        client
            .read_plant_energy(PlantEnergyCounter::NewGridImport)
            .unwrap()
            > 0
    );
    assert!(
        client
            .read_plant_energy(PlantEnergyCounter::NewGridExport)
            .unwrap()
            > 0
    );
    let e = client.read_electrical(unit).unwrap();
    assert_eq!(e.output_type, 2);
    assert_eq!(e.phase_voltage_v, [230.0, 230.3, 230.6]);
    assert!(
        client
            .read_grid_phase_power()
            .unwrap()
            .active_kw
            .iter()
            .all(|v| v.abs() < 1.0)
    );
    assert_eq!(client.read_plant_alarms().unwrap(), [0; 5]);
    assert_eq!(
        client
            .read_plant_capabilities()
            .unwrap()
            .max_active_power_kw,
        10.0
    );
    assert_eq!(client.read_plant_battery().unwrap().rated_energy_kwh, 20.0);
    assert_eq!(
        client.read_battery_ratings(unit).unwrap().rated_energy_kwh,
        20.0
    );
    assert_eq!(client.read_pv_string(unit, 4).unwrap().current_a, 0.0);
    assert_eq!(client.read_dc_charger(unit).unwrap().output_power_kw, 0.0);
    assert!(
        client
            .read_inverter_diagnostics(unit)
            .unwrap()
            .insulation_resistance_megohms
            > 0.0
    );
    assert!(client.read_smart_load(24).unwrap().power_kw > 0.0);
    assert!(
        client
            .read_plant_phase_power()
            .unwrap()
            .active_kw
            .iter()
            .all(|v| *v > 0.0)
    );
    assert_eq!(
        client
            .read_power_adjustment_feedback(unit)
            .unwrap()
            .power_factor,
        1.0
    );
}

#[test]
fn real_client_controls_have_shared_readback_and_change_power_flow() {
    let sim = Simulator::start("solar", 1);
    let mut client = Client::connect(sim.address).unwrap();
    client.set_remote_ems_enabled(true).unwrap();
    let mut wire = sim.wire();
    assert_eq!(wire.read(247, 4, 40029, 3), [1, 0, 1]);
    assert_eq!(wire.read(247, 4, 30003, 1), [7]);
    assert_eq!(wire.read(247, 4, 30037, 2), [0, 0]); // standby
    client
        .set_remote_ems_mode(RemoteEmsMode::ChargeFromGridFirst)
        .unwrap();
    assert_eq!(wire.read(247, 4, 30037, 2), [0, 5000]);
    client
        .write_registers(UnitId::PLANT, 40032, &[0, 2500])
        .unwrap();
    assert_eq!(wire.read(247, 4, 30037, 2), [0, 2500]);
    client
        .set_remote_ems_mode(RemoteEmsMode::DischargeBatteryFirst)
        .unwrap();
    assert_eq!(wire.read(247, 4, 30037, 2), [0xffff, (-5000_i32) as u16]);
    client.set_backup_soc_percent(90.0).unwrap();
    assert_eq!(wire.read(247, 4, 40046, 1), [900]);
    assert_eq!(wire.read(247, 4, 30037, 2), [0, 0]); // reserve stops discharge
    let mut another = Client::connect(sim.address).unwrap();
    assert_eq!(
        another
            .read_registers(UnitId::PLANT, ReadFunction::Holding, 40031, 1)
            .unwrap(),
        [6]
    );
}

#[test]
fn malformed_pdus_return_exceptions_without_partial_writes_or_losing_framing() {
    let sim = Simulator::start("solar", 1);
    let mut w = sim.wire();
    for (pdu, expected) in [
        (vec![99], vec![0xe3, 1]),
        (vec![3], vec![0x83, 3]),
        (vec![4, 0, 0, 0, 0], vec![0x84, 3]),
        (vec![4, 0, 0, 0, 125], vec![0x84, 3]),
        (vec![4, 255, 255, 0, 2], vec![0x84, 2]),
        (vec![4, 255, 255, 0, 1], vec![0x84, 2]),
        (vec![16, 0, 0, 0, 1, 1, 0], vec![0x90, 3]),
    ] {
        assert_eq!(w.request(247, &pdu), expected);
    }
    assert_eq!(w.write(247, 30014, 0), [0x86, 2]);
    assert_eq!(w.write(247, 40031, 7), [0x86, 3]);
    assert_eq!(w.write(247, 40001, 100), [0x86, 2]); // cannot write half of S32
    assert_eq!(w.multiple(247, 40029, &[1, 0, 9]), [0x90, 3]);
    assert_eq!(w.read(247, 3, 40029, 3), [0, 0, 1]);
    assert_eq!(w.multiple(247, 40032, &[0, 6000]), [0x90, 3]);
    assert_eq!(w.read(247, 4, 40032, 2), [0, 5000]);
    assert_eq!(w.request(2, &[4, 0x77, 0x72, 0, 1]), [0x84, 11]);
    assert_eq!(w.request(247, &[4, 0x9c, 0x40, 0, 1]), [0x84, 2]); // start/stop is WO
    assert_eq!(w.read(1, 4, 31025, 1), [4]);
}

#[test]
fn fragmented_coalesced_and_broadcast_frames_preserve_transaction_and_unit_ids() {
    let sim = Simulator::start("solar", 1);
    let mut w = sim.wire();
    let frame = Wire::frame(0x1234, 247, &[4, 0x9c, 0x5d, 0, 1]);
    for byte in frame {
        w.stream.write_all(&[byte]).unwrap();
    }
    assert_eq!(w.response(0x1234, 247), [4, 2, 0, 0]);
    let mut frames = Wire::frame(1, 0, &[6, 0x9c, 0x5d, 0, 1]);
    frames.extend(Wire::frame(2, 247, &[3, 0x9c, 0x5d, 0, 1]));
    frames.extend(Wire::frame(3, 1, &[4, 0x79, 0x34, 0, 1])); // 31028 PV1 current
    w.stream.write_all(&frames).unwrap();
    assert_eq!(w.response(2, 247), [3, 2, 0, 0]); // broadcast did not mutate state
    assert_eq!(w.response(3, 1)[0], 4);
}

#[test]
fn malformed_mbap_closes_only_the_bad_connection() {
    let sim = Simulator::start("solar", 1);
    for header in [
        [0, 1, 0, 1, 0, 6, 247],
        [0, 1, 0, 0, 255, 255, 247],
        [0, 1, 0, 0, 0, 1, 247],
    ] {
        let mut w = sim.wire();
        w.stream.write_all(&header).unwrap();
        let mut byte = [0];
        assert_eq!(w.stream.read(&mut byte).unwrap(), 0);
    }
    assert_eq!(sim.wire().read(247, 4, 40046, 1), [200]);
}

#[test]
fn fault_and_grid_outage_scenarios_expose_real_alarm_registers() {
    for (scenario, alarm, grid) in [
        ("battery-alarm", 2, 0),
        ("thermal-runaway", 64, 0),
        ("grid-outage", 0, 1),
    ] {
        let sim = Simulator::start(scenario, 1);
        let mut w = sim.wire();
        assert_eq!(w.read(1, 4, 30607, 1), [alarm]);
        assert_eq!(w.read(247, 4, 30029, 1), [alarm]);
        assert_eq!(w.read(247, 4, 30009, 1), [grid]);
    }
}

#[test]
fn cli_rejects_invalid_arguments_and_explains_usage() {
    for args in [
        vec!["--unit-id", "247"],
        vec!["--cycle-seconds", "0"],
        vec!["--scenario", "bogus"],
        vec!["--bind"],
        vec!["--unknown"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_sigen-simulator"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
    let help = Command::new(env!("CARGO_BIN_EXE_sigen-simulator"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(
        String::from_utf8(help.stdout)
            .unwrap()
            .contains("127.0.0.1:1502")
    );
}
