use sigen_modbus::{Client, Config, Error, ReadFunction, RemoteEmsMode, UnitId};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

fn mock(handler: impl FnOnce(TcpStream) + Send + 'static) -> (Client, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        handler(stream);
    });
    (Client::connect(address).unwrap(), worker)
}

fn request(stream: &mut TcpStream, unit: u8, pdu: &[u8]) -> [u8; 7] {
    let mut header = [0; 7];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(&header[2..4], &[0, 0]);
    assert_eq!(header[6], unit);
    assert_eq!(
        u16::from_be_bytes([header[4], header[5]]) as usize,
        1 + pdu.len()
    );
    let mut received = vec![0; pdu.len()];
    stream.read_exact(&mut received).unwrap();
    assert_eq!(received, pdu);
    header
}

fn reply(stream: &mut TcpStream, mut header: [u8; 7], pdu: &[u8]) {
    header[4..6].copy_from_slice(&(pdu.len() as u16 + 1).to_be_bytes());
    stream.write_all(&header).unwrap();
    // Fragment the frame to verify that a TCP read is not assumed to be a full frame.
    for byte in pdu {
        stream.write_all(&[*byte]).unwrap();
    }
}

fn registers(stream: &mut TcpStream, unit: u8, address: u16, values: &[u16]) {
    let [a, b] = address.to_be_bytes();
    let header = request(stream, unit, &[4, a, b, 0, values.len() as u8]);
    let mut pdu = vec![4, (values.len() * 2) as u8];
    for value in values {
        pdu.extend_from_slice(&value.to_be_bytes());
    }
    reply(stream, header, &pdu);
}

#[test]
fn literal_address_and_big_endian_u64_are_preserved() {
    let (mut client, worker) = mock(|mut stream| {
        registers(&mut stream, 247, 30200, &[0x1234, 0x5678, 0x9abc, 0xdef0]);
    });
    assert_eq!(
        client.read_energy_counter(UnitId::PLANT, 30200).unwrap(),
        0x123456789abcdef0
    );
    worker.join().unwrap();
}

#[test]
fn battery_registers_decode_signed_values_and_scaling() {
    let (mut client, worker) = mock(|mut stream| {
        let values = [
            0,
            5000,
            0,
            6000,
            0,
            1250,
            0,
            2500,
            0xffff,
            0xfb2e,
            567,
            995,
            (-125_i16) as u16,
            3300,
            1,
            2,
            4,
            8,
            16,
        ];
        registers(&mut stream, 2, 30591, &values);
        registers(&mut stream, 2, 30620, &[300, (-150_i16) as u16, 3400, 3200]);
    });
    let b = client.read_battery(UnitId::inverter(2).unwrap()).unwrap();
    assert_eq!(b.max_charge_power_kw, 5.0);
    assert_eq!(b.max_discharge_power_kw, 6.0);
    assert_eq!(b.available_charge_energy_kwh, 12.5);
    assert_eq!(b.available_discharge_energy_kwh, 25.0);
    assert_eq!(b.power_kw, -1.234);
    assert_eq!(b.soc_percent, 56.7);
    assert_eq!(b.soh_percent, 99.5);
    assert_eq!(b.average_cell_temperature_c, -12.5);
    assert_eq!(b.average_cell_voltage_v, 3.3);
    assert_eq!(b.alarms, [1, 2, 4, 8, 16]);
    assert_eq!(b.max_temperature_c, 30.0);
    assert_eq!(b.min_temperature_c, -15.0);
    assert_eq!(b.max_cell_voltage_v, 3.4);
    assert_eq!(b.min_cell_voltage_v, 3.2);
    worker.join().unwrap();
}

#[test]
fn writes_match_document_examples_and_are_paced() {
    let (mut client, worker) = mock(|mut stream| {
        let pdu = [0x10, 0x9c, 0x41, 0, 2, 4, 0, 0, 0x61, 0xa8];
        let first = request(&mut stream, 247, &pdu);
        let started = Instant::now();
        reply(&mut stream, first, &pdu[..5]);
        let pdu = [6, 0x9c, 0x5f, 0, 3];
        let second = request(&mut stream, 247, &pdu);
        assert!(started.elapsed() >= Duration::from_millis(950));
        assert_eq!(
            u16::from_be_bytes([second[0], second[1]]),
            u16::from_be_bytes([first[0], first[1]]) + 1
        );
        reply(&mut stream, second, &pdu);
    });
    client
        .write_registers(UnitId::PLANT, 40001, &[0, 25000])
        .unwrap();
    client
        .set_remote_ems_mode(RemoteEmsMode::ChargeFromGridFirst)
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn exceptions_leave_connection_usable() {
    let (mut client, worker) = mock(|mut stream| {
        let header = request(&mut stream, 247, &[4, 0x75, 0x30, 0, 1]);
        reply(&mut stream, header, &[0x84, 2]);
        registers(&mut stream, 247, 30000, &[42]);
    });
    assert!(matches!(
        client.read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1),
        Err(Error::Exception(2))
    ));
    assert_eq!(
        client
            .read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1)
            .unwrap(),
        [42]
    );
    worker.join().unwrap();
}

#[test]
fn malformed_responses_poison_the_connection() {
    // Transaction, protocol, unit, MBAP length, function, byte count, exception length.
    for case in 0..7 {
        let (mut client, worker) = mock(move |mut stream| {
            let mut header = request(&mut stream, 247, &[4, 0x75, 0x30, 0, 1]);
            let pdu: &[u8] = match case {
                0 => {
                    header[1] ^= 1;
                    &[4, 2, 0, 42]
                }
                1 => {
                    header[3] = 1;
                    &[4, 2, 0, 42]
                }
                2 => {
                    header[6] = 1;
                    &[4, 2, 0, 42]
                }
                3 => &[4], // MBAP length 2 is too short
                4 => &[3, 2, 0, 42],
                5 => &[4, 4, 0, 42],
                _ => &[0x84, 2, 0],
            };
            // Write as one frame, avoiding writes after the client rejects its header.
            header[4..6].copy_from_slice(&(pdu.len() as u16 + 1).to_be_bytes());
            let frame: Vec<_> = header.into_iter().chain(pdu.iter().copied()).collect();
            stream.write_all(&frame).unwrap();
        });
        assert!(
            matches!(
                client.read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1),
                Err(Error::Protocol(_))
            ),
            "case {case}"
        );
        assert!(matches!(
            client.read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1),
            Err(Error::Disconnected)
        ));
        worker.join().unwrap();
    }
}

#[test]
fn wrong_write_echo_is_rejected() {
    let (mut client, worker) = mock(|mut stream| {
        let header = request(&mut stream, 247, &[6, 0x9c, 0x6e, 0, 200]);
        reply(&mut stream, header, &[6, 0x9c, 0x6e, 0, 201]);
    });
    assert!(matches!(
        client.set_backup_soc_percent(20.0),
        Err(Error::Protocol(_))
    ));
    worker.join().unwrap();
}

#[test]
fn invalid_arguments_do_not_send_requests() {
    assert!(UnitId::inverter(0).is_err());
    assert!(UnitId::inverter(247).is_err());
    let (mut client, worker) = mock(|mut stream| {
        // Only the final valid request should reach the server.
        registers(&mut stream, 247, 30000, &[1]);
    });
    for (address, count) in [(0, 0), (0, 125), (65535, 2)] {
        assert!(matches!(
            client.read_registers(UnitId::PLANT, ReadFunction::Input, address, count),
            Err(Error::InvalidArgument(_))
        ));
    }
    assert!(client.write_registers(UnitId::PLANT, 40001, &[]).is_err());
    assert!(
        client
            .write_registers(UnitId::PLANT, 40001, &[0; 124])
            .is_err()
    );
    for percent in [f64::NAN, f64::INFINITY, -0.1, 100.1] {
        assert!(client.set_backup_soc_percent(percent).is_err());
    }
    assert!(client.read_battery(UnitId::PLANT).is_err());
    assert!(client.read_device_info(UnitId::PLANT).is_err());
    client
        .read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1)
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn alternate_telemetry_function_is_configurable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let header = request(&mut stream, 247, &[3, 0x75, 0xf8, 0, 4]);
        reply(&mut stream, header, &[3, 8, 0, 0, 0, 0, 0, 0, 0, 1]);
    });
    let mut client = Client::connect_with_config(
        address,
        Config {
            telemetry_function: ReadFunction::Holding,
            ..Config::default()
        },
    )
    .unwrap();
    assert_eq!(client.read_energy_counter(UnitId::PLANT, 30200).unwrap(), 1);
    worker.join().unwrap();
}

#[test]
fn timeout_requires_reconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request(&mut stream, 247, &[4, 0x75, 0x30, 0, 1]);
        thread::sleep(Duration::from_millis(150));
    });
    let mut client = Client::connect_with_config(
        address,
        Config {
            response_timeout: Duration::from_millis(30),
            ..Config::default()
        },
    )
    .unwrap();
    assert!(matches!(
        client.read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1),
        Err(Error::Io(_))
    ));
    assert!(matches!(
        client.read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1),
        Err(Error::Disconnected)
    ));
    worker.join().unwrap();
}

#[test]
fn plant_uses_plant_unit_and_documented_offsets() {
    let (mut client, worker) = mock(|mut stream| {
        let mut a = [0; 15];
        a[..2].copy_from_slice(&[0x1234, 0x5678]);
        a[2] = (-60_i16) as u16;
        a[3] = 7;
        a[4] = 1;
        a[5..7].copy_from_slice(&[0xffff, 0xfc18]); // -1000 W
        a[7..9].copy_from_slice(&[0, 500]);
        a[9] = 2;
        a[14] = 755;
        registers(&mut stream, 247, 30000, &a);
        registers(
            &mut stream,
            247,
            30031,
            &[0, 1500, 0, 250, 0, 3000, 0xffff, 0xf830],
        );
        registers(&mut stream, 247, 30051, &[7]);
    });
    let p = client.read_plant().unwrap();
    assert_eq!(p.epoch_seconds, 0x12345678);
    assert_eq!(p.timezone_offset_minutes, -60);
    assert_eq!(p.ems_work_mode, 7);
    assert!(p.grid_sensor_connected);
    assert_eq!(p.grid_power_kw, -1.0);
    assert_eq!(p.grid_reactive_power_kvar, 0.5);
    assert_eq!(p.grid_status, 2);
    assert_eq!(p.battery_soc_percent, 75.5);
    assert_eq!(p.active_power_kw, 1.5);
    assert_eq!(p.reactive_power_kvar, 0.25);
    assert_eq!(p.pv_power_kw, 3.0);
    assert_eq!(p.battery_power_kw, -2.0);
    assert_eq!(
        p.running_state,
        sigen_modbus::RunningState::EnvironmentalAbnormality
    );
    worker.join().unwrap();
}

#[test]
fn identification_and_inverter_measurements_use_device_unit() {
    let (mut client, worker) = mock(|mut stream| {
        let mut info = [0; 46];
        info[..3].copy_from_slice(&[0x5369, 0x6765, 0x6e00]); // Sigen
        info[15] = 0x3132; // 12
        info[25] = 0x5631; // V1
        info[41] = 5000;
        info[43] = 6000;
        info[45] = 7000;
        registers(&mut stream, 1, 30500, &info);
        registers(&mut stream, 1, 30578, &[1]);
        registers(&mut stream, 1, 30587, &[0xffff, 0xfc18, 0, 250]);
        registers(&mut stream, 1, 31002, &[5000, (-50_i16) as u16]);
        registers(&mut stream, 1, 31035, &[0, 1234]);
    });
    let unit = UnitId::inverter(1).unwrap();
    let info = client.read_device_info(unit).unwrap();
    assert_eq!(info.model, "Sigen");
    assert_eq!(info.serial_number, "12");
    assert_eq!(info.firmware_version, "V1");
    assert_eq!(info.rated_active_power_kw, 5.0);
    assert_eq!(info.max_apparent_power_kva, 6.0);
    assert_eq!(info.max_active_power_kw, 7.0);
    let i = client.read_inverter(unit).unwrap();
    assert_eq!(i.running_state, sigen_modbus::RunningState::Running);
    assert_eq!(i.active_power_kw, -1.0);
    assert_eq!(i.reactive_power_kvar, 0.25);
    assert_eq!(i.grid_frequency_hz, 50.0);
    assert_eq!(i.internal_temperature_c, -5.0);
    assert_eq!(i.pv_power_kw, 1.234);
    worker.join().unwrap();
}

#[test]
fn cancelled_write_is_checked_after_pacing_and_sends_no_bytes() {
    let (mut client, worker) = mock(|mut stream| {
        registers(&mut stream, 247, 30000, &[1]);
        // The cancelled write must not appear between these reads.
        registers(&mut stream, 247, 30000, &[2]);
    });
    client
        .read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1)
        .unwrap();
    let started = Instant::now();
    let result = client.write_register_checked(UnitId::PLANT, 40029, 1, || {
        assert!(started.elapsed() >= Duration::from_millis(950));
        Err(Error::InvalidArgument("cancelled"))
    });
    assert!(matches!(result, Err(Error::InvalidArgument("cancelled"))));
    assert_eq!(
        client
            .read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1)
            .unwrap(),
        [2]
    );
    worker.join().unwrap();
}

#[test]
fn truncated_and_wrong_multiwrite_echoes_never_confirm_and_poison_the_stream() {
    for response in [
        vec![0x10, 0x9c, 0x41, 0, 1],
        vec![0x10, 0x9c, 0x42, 0, 2],
        vec![0x10, 0x9c, 0x41, 0],
    ] {
        let (mut client, worker) = mock(move |mut stream| {
            let header = request(
                &mut stream,
                247,
                &[0x10, 0x9c, 0x41, 0, 2, 4, 0, 0, 0x61, 0xa8],
            );
            reply(&mut stream, header, &response);
        });
        assert!(matches!(
            client.write_registers(UnitId::PLANT, 40001, &[0, 25000]),
            Err(Error::Protocol(_))
        ));
        assert!(matches!(
            client.set_remote_ems_enabled(true),
            Err(Error::Disconnected)
        ));
        worker.join().unwrap();
    }
}

#[test]
fn unrepresentable_connection_timeout_is_rejected_before_connecting() {
    assert!(matches!(
        Client::connect_with_config(
            "127.0.0.1:1".parse().unwrap(),
            Config {
                connect_timeout: Duration::MAX,
                ..Config::default()
            }
        ),
        Err(Error::InvalidArgument(_))
    ));
}

#[derive(Debug, Default)]
struct AuditCapture(std::sync::Mutex<Vec<sigen_modbus::WriteRecord>>);
impl sigen_modbus::WriteAuditor for AuditCapture {
    fn record(&self, record: &sigen_modbus::WriteRecord) -> std::io::Result<()> {
        self.0.lock().unwrap().push(record.clone());
        Ok(())
    }
}

#[test]
fn audit_captures_single_multiword_and_cancelled_writes_with_target_and_source() {
    let (mut client, worker) = mock(|mut stream| {
        let pdu = [6, 0x9c, 0x6e, 0, 200];
        let header = request(&mut stream, 247, &pdu);
        reply(&mut stream, header, &pdu);
        let pdu = [0x10, 0x9c, 0x41, 0, 2, 4, 0, 0, 0x61, 0xa8];
        let header = request(&mut stream, 247, &pdu);
        reply(&mut stream, header, &pdu[..5]);
        registers(&mut stream, 247, 30000, &[1]);
    });
    let capture = std::sync::Arc::new(AuditCapture::default());
    client.set_write_auditor(capture.clone());
    client.set_write_source("mqtt topic=sigen/test/command/backup_soc");
    client.set_backup_soc_percent(20.0).unwrap();
    client.set_write_source("http POST /api/control");
    client
        .write_registers(UnitId::PLANT, 40001, &[0, 25000])
        .unwrap();
    assert!(
        client
            .write_register_checked(UnitId::PLANT, 40029, 1, || Err(Error::InvalidArgument(
                "cancelled"
            )))
            .is_err()
    );
    client
        .read_registers(UnitId::PLANT, ReadFunction::Input, 30000, 1)
        .unwrap();
    worker.join().unwrap();
    let records = capture.0.lock().unwrap();
    assert_eq!(
        records.iter().map(|r| r.phase).collect::<Vec<_>>(),
        ["attempt", "sent", "attempt", "sent", "attempt"]
    );
    assert_eq!(records[0].unit, 247);
    assert_eq!(records[0].register, 40046);
    assert_eq!(records[0].values, [200]);
    assert!(records[0].source.starts_with("mqtt topic="));
    assert!(records[0].target.ip().is_loopback());
    assert!(records[0].frame_hex.ends_with("f7069c6e00c8"));
    assert_eq!(records[2].function, 0x10);
    assert_eq!(records[2].values, [0, 25000]);
    assert_eq!(records[2].source, "http POST /api/control");
}

#[test]
fn failed_audit_prevents_transmission() {
    #[derive(Debug)]
    struct FailingAudit;
    impl sigen_modbus::WriteAuditor for FailingAudit {
        fn record(&self, _: &sigen_modbus::WriteRecord) -> std::io::Result<()> {
            Err(std::io::Error::other("audit disk failure"))
        }
    }
    let (mut client, worker) = mock(|mut stream| {
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    client.set_write_auditor(std::sync::Arc::new(FailingAudit));
    assert!(matches!(
        client.set_remote_ems_enabled(true),
        Err(Error::Io(_))
    ));
    worker.join().unwrap();
}
