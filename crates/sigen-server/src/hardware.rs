use crate::model::{Controls, Readings, Status};
use serde::Deserialize;
use sigen_modbus::{Client, Config, ReadFunction, RemoteEmsMode, UnitId};
use std::{
    net::SocketAddr,
    sync::{Arc, RwLock, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

pub type SharedStatus = Arc<RwLock<Status>>;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    RemoteEms { enabled: bool },
    EmsMode { mode: u16 },
    BackupSoc { percent: f64 },
}
impl Command {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::EmsMode { mode } if *mode > 6 => Err("EMS mode must be 0–6".into()),
            Self::BackupSoc { percent }
                if !percent.is_finite() || !(0.0..=100.0).contains(percent) =>
            {
                Err("Backup SOC must be between 0 and 100 percent".into())
            }
            _ => Ok(()),
        }
    }
}

pub struct Job {
    pub command: Command,
    pub reply: oneshot::Sender<Result<Controls, String>>,
    pub(crate) source: String,
    epoch: u64,
    deadline: Instant,
}

impl Job {
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Bind a command to the current hardware epoch and a monotonic expiry time.
    /// Reconnecting later must not revive work accepted against an earlier state.
    pub fn new(
        command: Command,
        reply: oneshot::Sender<Result<Controls, String>>,
        status: &Status,
    ) -> Result<Self, String> {
        command.validate()?;
        if !status.controls_ready() {
            return Err("Hardware is offline, stale, or stopping; no command was queued".into());
        }
        Ok(Self {
            command,
            reply,
            source: "internal".into(),
            epoch: status.control_epoch,
            deadline: Instant::now() + Duration::from_secs(60),
        })
    }

    /// Recheck both before execution and immediately before each write is sent.
    fn check(&self, status: &Status) -> sigen_modbus::Result<()> {
        if self.reply.is_closed() || Instant::now() >= self.deadline {
            return Err(sigen_modbus::Error::InvalidArgument(
                "command expired or was cancelled; no further write sent",
            ));
        }
        if self.epoch != status.control_epoch || !status.controls_ready() {
            return Err(sigen_modbus::Error::InvalidArgument(
                "hardware state changed, is stale, or server is stopping; submit a new command after checking readback",
            ));
        }
        Ok(())
    }
}

pub fn start(
    address: SocketAddr,
    unit: UnitId,
    config: Config,
    interval: Duration,
    status: SharedStatus,
) -> mpsc::SyncSender<Job> {
    let (sender, receiver) = mpsc::sync_channel::<Job>(4);
    // This permanent worker uses blocking Modbus I/O, request pacing, and recv_timeout.
    // Give it a dedicated OS thread so those waits do not block Tokio workers.
    // Owning the client here also serializes polls, commands, and readback checks.
    thread::spawn(move || {
        let _span = tracing::info_span!("hardware", %address, unit_id = unit.get()).entered();
        tracing::info!("Hardware worker started");
        let mut client = None;
        let mut next_poll = Instant::now();
        let mut details = crate::telemetry::Poller::new();
        loop {
            if status.read().expect("status lock").shutting_down {
                break;
            }
            if Instant::now() >= next_poll {
                let result = poll(&mut client, address, unit, &config, &status);
                let mut s = status.write().expect("status lock");
                match result {
                    Ok((readings, controls)) => {
                        if !s.online {
                            tracing::info!("Hardware connected; readings available");
                        }
                        tracing::debug!("Hardware poll completed");
                        s.online = true;
                        s.last_error = None;
                        s.last_success_unix = Some(now());
                        s.readings = Some(readings);
                        s.controls = Some(controls);
                        s.control_valid_until =
                            Some(Instant::now() + interval + Duration::from_secs(30));
                    }
                    Err(error) => {
                        if s.online || s.last_error.as_ref() != Some(&error) {
                            tracing::warn!(%error, "Hardware poll failed");
                        } else {
                            tracing::debug!(%error, "Hardware poll still failing");
                        }
                        s.online = false;
                        s.last_error = Some(error);
                        s.poll_errors += 1;
                        s.invalidate_controls();
                        // A new connection next time also handles firmware disconnects.
                        client = None;
                    }
                }
                crate::alerts::refresh(&mut s, now());
                next_poll = Instant::now() + interval;
            }
            match receiver.recv_timeout(
                next_poll
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100)),
            ) {
                Ok(job) => {
                    // Requests which expired while queued must not execute later.
                    if job.reply.is_closed() {
                        tracing::debug!("Skipping expired hardware command");
                        continue;
                    }
                    if let Err(error) = job.check(&status.read().expect("status lock")) {
                        let _ = job.reply.send(Err(error.to_string()));
                        continue;
                    }
                    tracing::info!(command = ?job.command, "Applying hardware command");
                    let result = match client.as_mut() {
                        Some(client) => {
                            client.set_write_source(format!(
                                "{}; requested={:?}",
                                job.source, job.command
                            ));
                            apply(client, &job.command, || {
                                job.check(&status.read().expect("status lock"))
                            })
                        }
                        None => Err("Hardware is offline; no command was sent".into()),
                    };
                    {
                        let mut s = status.write().expect("status lock");
                        match &result {
                            Ok(controls) => {
                                tracing::info!("Hardware command confirmed");
                                s.controls = Some(controls.clone());
                                s.last_control_error = None;
                            }
                            Err(error) => {
                                tracing::warn!(%error, "Hardware command failed");
                                s.last_control_error = Some(error.clone());
                                s.control_errors += 1;
                                s.invalidate_controls();
                                s.online = false;
                                s.last_error = Some(error.clone());
                            }
                        }
                    }
                    if result.is_err() {
                        client = None;
                    }
                    crate::alerts::refresh(&mut status.write().expect("status lock"), now());
                    let _ = job.reply.send(result);
                    // Refresh telemetry following controls, while keeping queued commands serialized.
                    // Reconnecting must not bypass the plant's minimum request period.
                    next_poll = Instant::now()
                        + if client.is_none() {
                            config.request_interval
                        } else {
                            Duration::ZERO
                        };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(connected) = client.as_mut()
                        && details.poll_if_due(connected, unit, &config, next_poll, &status)
                    {
                        client = None;
                        next_poll = Instant::now() + config.request_interval;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        tracing::info!("Hardware worker stopped");
    });
    sender
}

#[tracing::instrument(level = "debug", skip_all, fields(%address, unit_id = unit.get()))]
/// Collect a complete core poll, checking device identity whenever reconnecting.
/// Reads span multiple requests; the worker publishes readings only if all succeed.
fn poll(
    client: &mut Option<Client>,
    address: SocketAddr,
    unit: UnitId,
    config: &Config,
    status: &SharedStatus,
) -> Result<(Readings, Controls), String> {
    let result = (|| -> sigen_modbus::Result<_> {
        let reconnecting = client.is_none();
        if reconnecting {
            *client = Some(Client::connect_with_config(address, config.clone())?);
        }
        let client = client.as_mut().expect("connected client");
        if let Some(audit) = &status.read().expect("status lock").write_audit {
            client.set_write_auditor(audit.clone());
        }
        if reconnecting || status.read().expect("status lock").device.is_none() {
            let device = client.read_device_info(unit)?;
            if status
                .read()
                .expect("status lock")
                .device
                .as_ref()
                .is_some_and(|known| known.serial_number != device.serial_number)
            {
                return Err(sigen_modbus::Error::Protocol(
                    "inverter serial number changed; verify the target before restarting the server",
                ));
            }
            status.write().expect("status lock").device = Some(device.into());
        }
        let plant = client.read_plant()?;
        status.write().expect("status lock").device_day = Some(
            (i64::from(plant.epoch_seconds) + i64::from(plant.timezone_offset_minutes) * 60)
                .div_euclid(86400),
        );
        let inverter = client.read_inverter(unit)?;
        let battery = client.read_battery(unit)?;
        let controls = read_controls(client)?;
        Ok((Readings::new(plant, inverter, battery), controls))
    })();
    result.map_err(|e| e.to_string())
}

fn read_controls(client: &mut Client) -> sigen_modbus::Result<Controls> {
    // V2.7 §6.1 calls holding reads FC04; use that explicit function here.
    let enabled = client.read_registers(UnitId::PLANT, ReadFunction::Input, 40029, 1)?[0];
    if enabled > 1 {
        return Err(sigen_modbus::Error::Protocol(
            "invalid remote EMS enable readback",
        ));
    }
    let mode = client.read_registers(UnitId::PLANT, ReadFunction::Input, 40031, 1)?[0];
    if mode > 6 {
        return Err(sigen_modbus::Error::Protocol(
            "invalid remote EMS mode readback",
        ));
    }
    let backup = client.read_registers(UnitId::PLANT, ReadFunction::Input, 40046, 1)?[0];
    if backup > 1000 {
        return Err(sigen_modbus::Error::Protocol("invalid backup SOC readback"));
    }
    Ok(Controls {
        remote_ems_enabled: enabled == 1,
        remote_ems_mode: mode,
        backup_soc_percent: backup as f64 / 10.0,
    })
}

/// Apply one command and confirm its readback, preparing standby before enabling EMS.
/// Errors can follow an applied write, so callers must not blindly retry the command.
fn apply(
    client: &mut Client,
    command: &Command,
    mut check: impl FnMut() -> sigen_modbus::Result<()>,
) -> Result<Controls, String> {
    command.validate()?;
    let (before, result) = (|| -> sigen_modbus::Result<(Controls, Controls)> {
        check()?;
        let before = read_controls(client)?;
        let (address, value) = match command {
            Command::RemoteEms { enabled } => {
                // Enabling must not silently resume a previous forced charge/discharge
                // or PCS dispatch. Prepare and confirm standby while EMS is disabled.
                if *enabled && !before.remote_ems_enabled && before.remote_ems_mode != RemoteEmsMode::Standby as u16 {
                    client.write_register_checked(UnitId::PLANT, 40031, RemoteEmsMode::Standby as u16, &mut check)?;
                    let prepared = read_controls(client)?;
                    if prepared.remote_ems_enabled || prepared.remote_ems_mode != RemoteEmsMode::Standby as u16 {
                        return Err(sigen_modbus::Error::Protocol("standby preparation readback differs; remote EMS was not enabled by this command"));
                    }
                }
                (40029, u16::from(*enabled))
            },
            Command::EmsMode { mode } => {
                if !before.remote_ems_enabled {
                    return Err(sigen_modbus::Error::InvalidArgument(
                        "enable remote EMS before selecting a mode",
                    ));
                }
                (40031, *mode)
            }
            Command::BackupSoc { percent } => (40046, (percent * 10.0).round() as u16),
        };
        client.write_register_checked(UnitId::PLANT, address, value, &mut check)?;
        Ok((before, read_controls(client)?))
    })()
    .map_err(|e| {
        format!("Control failed: {e}. The setting may have changed; refresh before retrying.")
    })?;
    let confirmed = match command {
        Command::RemoteEms { enabled } => {
            result.remote_ems_enabled == *enabled
                && (!*enabled
                    || before.remote_ems_enabled
                    || result.remote_ems_mode == RemoteEmsMode::Standby as u16)
        }
        Command::EmsMode { mode } => result.remote_ems_enabled && result.remote_ems_mode == *mode,
        Command::BackupSoc { percent } => {
            (result.backup_soc_percent * 10.0).round() == (percent * 10.0).round()
        }
    };
    if !confirmed {
        return Err("Write was acknowledged but readback differs; refresh before retrying".into());
    }
    Ok(result)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    fn exchange(stream: &mut std::net::TcpStream, expected: &[u8], response: &[u8]) {
        let mut header = [0; 7];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(header[6], 247);
        let mut pdu = vec![0; expected.len()];
        stream.read_exact(&mut pdu).unwrap();
        assert_eq!(pdu, expected);
        header[4..6].copy_from_slice(&(response.len() as u16 + 1).to_be_bytes());
        stream.write_all(&header).unwrap();
        stream.write_all(response).unwrap();
    }
    #[test]
    fn writes_require_actual_matching_readback() {
        for readback in [200_u16, 300] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                controls_reply(&mut stream, 1, 2, 100);
                exchange(
                    &mut stream,
                    &[6, 0x9c, 0x6e, 0, 200],
                    &[6, 0x9c, 0x6e, 0, 200],
                );
                exchange(&mut stream, &[4, 0x9c, 0x5d, 0, 1], &[4, 2, 0, 1]);
                exchange(&mut stream, &[4, 0x9c, 0x5f, 0, 1], &[4, 2, 0, 2]);
                let [hi, lo] = readback.to_be_bytes();
                exchange(&mut stream, &[4, 0x9c, 0x6e, 0, 1], &[4, 2, hi, lo]);
            });
            let mut client = Client::connect(address).unwrap();
            let result = apply(
                &mut client,
                &Command::BackupSoc { percent: 20.0 },
                || Ok(()),
            );
            if readback == 200 {
                assert_eq!(result.unwrap().backup_soc_percent, 20.0);
            } else {
                assert!(result.unwrap_err().contains("readback differs"));
            }
            worker.join().unwrap();
        }
    }
    #[test]
    fn mode_change_does_not_write_when_remote_ems_is_disabled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            controls_reply(&mut stream, 0, 2, 200);
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        });
        let mut client = Client::connect(address).unwrap();
        assert!(
            apply(&mut client, &Command::EmsMode { mode: 3 }, || Ok(()))
                .unwrap_err()
                .contains("enable remote EMS")
        );
        drop(client);
        worker.join().unwrap();
    }

    fn controls_reply(stream: &mut std::net::TcpStream, enabled: u16, mode: u16, backup: u16) {
        for (address, value) in [(40029_u16, enabled), (40031, mode), (40046, backup)] {
            let [a, b] = address.to_be_bytes();
            let [hi, lo] = value.to_be_bytes();
            exchange(stream, &[4, a, b, 0, 1], &[4, 2, hi, lo]);
        }
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    #[test]
    fn full_poll_populates_status_and_prometheus_in_engineering_units() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for (unit, address, count) in [
                (1, 30500_u16, 46_u16),
                (247, 30000, 15),
                (247, 30031, 8),
                (247, 30051, 1),
                (1, 30578, 1),
                (1, 30587, 4),
                (1, 31002, 2),
                (1, 31035, 2),
                (1, 30591, 19),
                (1, 30620, 4),
                (247, 40029, 1),
                (247, 40031, 1),
                (247, 40046, 1),
            ] {
                let mut header = [0; 7];
                stream.read_exact(&mut header).unwrap();
                assert_eq!(header[6], unit);
                let mut pdu = [0; 5];
                stream.read_exact(&mut pdu).unwrap();
                assert_eq!(pdu[0], 4);
                assert_eq!(u16::from_be_bytes([pdu[1], pdu[2]]), address);
                assert_eq!(u16::from_be_bytes([pdu[3], pdu[4]]), count);
                let mut words = vec![0_u16; count as usize];
                if address == 30031 {
                    words[5] = 1234;
                    words[6] = 0xffff;
                    words[7] = 0xfc18;
                }
                if address == 30591 {
                    words[10] = 755;
                    words[12] = (-50_i16) as u16;
                }
                let mut response = vec![4, (count * 2) as u8];
                for word in words {
                    response.extend_from_slice(&word.to_be_bytes());
                }
                header[4..6].copy_from_slice(&(response.len() as u16 + 1).to_be_bytes());
                stream.write_all(&header).unwrap();
                stream.write_all(&response).unwrap();
            }
        });
        let status = Arc::new(RwLock::new(Status::new(address.to_string(), 1)));
        let (readings, controls) = poll(
            &mut None,
            address,
            UnitId::inverter(1).unwrap(),
            &Config::default(),
            &status,
        )
        .unwrap();
        assert_eq!(readings.pv_power_watts, 1234.0);
        assert_eq!(readings.battery_power_watts, -1000.0);
        assert_eq!(readings.battery_soc_percent, 75.5);
        assert_eq!(readings.battery_temperature_celsius, -5.0);
        let mut s = status.write().unwrap();
        s.online = true;
        s.control_valid_until = Some(Instant::now() + Duration::from_secs(45));
        s.readings = Some(readings);
        s.controls = Some(controls);
        let text = crate::metrics::render(&s);
        assert!(text.contains("sigen_pv_power_watts 1234\n"));
        assert!(text.contains("sigen_battery_power_watts -1000\n"));
        assert!(text.contains("sigen_battery_soc_percent 75.5\n"));
        assert!(text.contains("sigen_alarm_bits{group=\"5\"} 0\n"));
        s.online = false;
        assert!(!crate::metrics::render(&s).contains("sigen_pv_power_watts"));
        worker.join().unwrap();
    }
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    enum Step {
        Read(u16, u16),
        Write(u16, u16),
        LostReply(u16, u16),
    }
    fn controls(enabled: u16, mode: u16, reserve: u16) -> Vec<Step> {
        vec![
            Step::Read(40029, enabled),
            Step::Read(40031, mode),
            Step::Read(40046, reserve),
        ]
    }
    fn execute(
        command: Command,
        steps: Vec<Step>,
        check: impl FnMut() -> sigen_modbus::Result<()>,
    ) -> Result<Controls, String> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            for step in steps {
                let mut header = [0; 7];
                stream.read_exact(&mut header).unwrap();
                assert_eq!(header[6], 247, "All supported controls target the plant");
                assert_eq!(&header[2..6], &[0, 0, 0, 6]);
                let mut pdu = [0; 5];
                stream.read_exact(&mut pdu).unwrap();
                let (address, value, function) = match step {
                    Step::Read(a, v) => (a, v, 4),
                    Step::Write(a, v) | Step::LostReply(a, v) => (a, v, 6),
                };
                let [a, b] = address.to_be_bytes();
                let [hi, lo] = value.to_be_bytes();
                assert_eq!(
                    pdu,
                    if function == 4 {
                        [4, a, b, 0, 1]
                    } else {
                        [6, a, b, hi, lo]
                    }
                );
                if matches!(step, Step::LostReply(..)) {
                    return;
                }
                let response = if function == 4 {
                    vec![4, 2, hi, lo]
                } else {
                    pdu.to_vec()
                };
                header[4..6].copy_from_slice(&(response.len() as u16 + 1).to_be_bytes());
                stream.write_all(&header).unwrap();
                stream.write_all(&response).unwrap();
            }
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "Unexpected extra request/write"
            );
        });
        let mut client = Client::connect(address).unwrap();
        let result = apply(&mut client, &command, check);
        drop(client);
        mock.join().unwrap();
        result
    }

    #[test]
    fn invalid_control_readbacks_stop_before_any_write() {
        let cases = vec![
            vec![Step::Read(40029, 2)],
            vec![Step::Read(40029, 65535)],
            vec![Step::Read(40029, 1), Step::Read(40031, 7)],
            vec![Step::Read(40029, 1), Step::Read(40031, 65535)],
            controls(1, 2, 1001),
            controls(1, 2, 65535),
        ];
        let threads: Vec<_> = cases
            .into_iter()
            .map(|steps| {
                thread::spawn(move || {
                    assert!(
                        execute(Command::RemoteEms { enabled: false }, steps, || Ok(()))
                            .unwrap_err()
                            .contains("readback")
                    );
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn invalid_disabled_readback_is_never_reported_as_confirmation() {
        let mut steps = controls(1, 2, 200);
        steps.extend([Step::Write(40029, 0), Step::Read(40029, 65535)]);
        assert!(execute(Command::RemoteEms { enabled: false }, steps, || Ok(())).is_err());
    }

    #[test]
    fn enabling_prepares_standby_before_enabling_and_confirms_both() {
        let mut steps = controls(0, 6, 200);
        steps.push(Step::Write(40031, 1));
        steps.extend(controls(0, 1, 200));
        steps.push(Step::Write(40029, 1));
        steps.extend(controls(1, 1, 200));
        let result = execute(Command::RemoteEms { enabled: true }, steps, || Ok(())).unwrap();
        assert!(result.remote_ems_enabled);
        assert_eq!(result.remote_ems_mode, 1);
    }

    #[test]
    fn failed_standby_preparation_never_enables_remote_ems() {
        let mut steps = controls(0, 6, 200);
        steps.push(Step::Write(40031, 1));
        steps.extend(controls(0, 6, 200));
        assert!(
            execute(Command::RemoteEms { enabled: true }, steps, || Ok(()))
                .unwrap_err()
                .contains("standby preparation")
        );
    }

    #[test]
    fn cancellation_between_standby_preparation_and_enable_sends_no_enable_write() {
        let mut steps = controls(0, 6, 200);
        steps.push(Step::Write(40031, 1));
        steps.extend(controls(0, 1, 200));
        let mut checks = 0;
        let result = execute(Command::RemoteEms { enabled: true }, steps, || {
            checks += 1;
            if checks < 3 {
                Ok(())
            } else {
                Err(sigen_modbus::Error::InvalidArgument(
                    "cancelled before enable",
                ))
            }
        });
        assert!(result.unwrap_err().contains("cancelled before enable"));
        assert_eq!(checks, 3);
    }

    #[test]
    fn already_enabled_request_preserves_mode_and_activation_rechecks_standby() {
        let workers: Vec<_> = [false, true]
            .into_iter()
            .map(|already_enabled| {
                thread::spawn(move || {
                    let mut steps = controls(
                        u16::from(already_enabled),
                        if already_enabled { 6 } else { 1 },
                        200,
                    );
                    steps.push(Step::Write(40029, 1));
                    steps.extend(controls(1, 6, 200));
                    let result = execute(Command::RemoteEms { enabled: true }, steps, || Ok(()));
                    if already_enabled {
                        assert_eq!(result.unwrap().remote_ems_mode, 6);
                    } else {
                        assert!(result.unwrap_err().contains("readback differs"));
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[test]
    fn matching_mode_is_not_confirmation_if_remote_ems_was_disabled() {
        let mut steps = controls(1, 2, 200);
        steps.push(Step::Write(40031, 3));
        steps.extend(controls(0, 3, 200));
        assert!(
            execute(Command::EmsMode { mode: 3 }, steps, || Ok(()))
                .unwrap_err()
                .contains("readback differs")
        );
    }

    #[test]
    fn every_mode_and_reserve_boundary_writes_the_exact_plant_register() {
        let mut cases: Vec<_> = (0..=6)
            .map(|mode| (Command::EmsMode { mode }, 40031, mode, mode, 200))
            .collect();
        cases.extend([
            (Command::BackupSoc { percent: 0.0 }, 40046, 0, 2, 0),
            (Command::BackupSoc { percent: 100.0 }, 40046, 1000, 2, 1000),
            (Command::BackupSoc { percent: 12.34 }, 40046, 123, 2, 123),
        ]);
        let threads: Vec<_> = cases
            .into_iter()
            .map(|(command, address, value, mode, reserve)| {
                thread::spawn(move || {
                    let mut steps = controls(1, 2, 200);
                    steps.push(Step::Write(address, value));
                    steps.extend(controls(1, mode, reserve));
                    execute(command, steps, || Ok(())).unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }

    #[test]
    fn cancellation_after_prereads_prevents_the_write() {
        let mut checks = 0;
        assert!(
            execute(
                Command::BackupSoc { percent: 20.0 },
                controls(1, 2, 200),
                || {
                    checks += 1;
                    if checks == 1 {
                        Ok(())
                    } else {
                        Err(sigen_modbus::Error::InvalidArgument("cancelled"))
                    }
                }
            )
            .unwrap_err()
            .contains("cancelled")
        );
        assert_eq!(checks, 2);
    }

    #[test]
    fn lost_write_reply_is_not_retried_or_confirmed() {
        let mut steps = controls(1, 2, 200);
        steps.push(Step::LostReply(40046, 300));
        assert!(
            execute(Command::BackupSoc { percent: 30.0 }, steps, || Ok(()))
                .unwrap_err()
                .contains("may have changed")
        );
    }

    fn ready_status() -> Status {
        let mut s = Status::new("127.0.0.1:502".into(), 1);
        s.online = true;
        s.controls = Some(Controls {
            remote_ems_enabled: true,
            remote_ems_mode: 2,
            backup_soc_percent: 20.0,
        });
        s.control_valid_until = Some(Instant::now() + Duration::from_secs(45));
        s
    }

    #[test]
    fn jobs_cannot_survive_cancellation_expiry_shutdown_or_hardware_failure() {
        for case in 0..6 {
            let mut s = ready_status();
            let (reply, response) = oneshot::channel();
            let mut job = Job::new(Command::BackupSoc { percent: 20.0 }, reply, &s).unwrap();
            assert!(job.check(&s).is_ok());
            match case {
                0 => drop(response),
                1 => job.deadline = Instant::now(),
                2 => s.shutting_down = true,
                3 => s.control_valid_until = Some(Instant::now()),
                4 => s.online = false,
                _ => {
                    s.invalidate_controls();
                    let epoch = s.control_epoch;
                    s = ready_status(); // A later successful reconnect must not revive this job.
                    s.control_epoch = epoch;
                }
            }
            assert!(job.check(&s).is_err(), "case {case}");
        }
        for command in [
            Command::EmsMode { mode: 7 },
            Command::BackupSoc { percent: f64::NAN },
            Command::BackupSoc {
                percent: f64::INFINITY,
            },
            Command::BackupSoc { percent: -0.1 },
            Command::BackupSoc { percent: 100.1 },
        ] {
            let (reply, _response) = oneshot::channel();
            assert!(Job::new(command, reply, &ready_status()).is_err());
        }
    }

    #[test]
    fn changed_inverter_identity_on_reconnect_blocks_poll_and_controls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = [0; 7];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(header[6], 1);
            let mut pdu = [0; 5];
            stream.read_exact(&mut pdu).unwrap();
            assert_eq!(pdu, [4, 0x77, 0x24, 0, 46]);
            let mut response = vec![4, 92];
            response.extend([0; 92]); // Empty serial differs from the cached identity.
            header[4..6].copy_from_slice(&95_u16.to_be_bytes());
            stream.write_all(&header).unwrap();
            stream.write_all(&response).unwrap();
            assert_eq!(stream.read(&mut [0]).unwrap(), 0);
        });
        let mut s = ready_status();
        s.device = Some(crate::model::Device {
            model: "Sigen".into(),
            serial_number: "expected".into(),
            firmware_version: "V1".into(),
        });
        let status = Arc::new(RwLock::new(s));
        let mut client = None;
        assert!(
            poll(
                &mut client,
                address,
                UnitId::inverter(1).unwrap(),
                &Config::default(),
                &status
            )
            .unwrap_err()
            .contains("serial number changed")
        );
        drop(client);
        mock.join().unwrap();
    }

    #[test]
    fn worker_failure_invalidates_other_queued_commands() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut writes = 0;
            loop {
                let mut header = [0; 7];
                match stream.read_exact(&mut header) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                    Err(e) => panic!("{e}"),
                }
                let mut pdu = [0; 5];
                stream.read_exact(&mut pdu).unwrap();
                let register = u16::from_be_bytes([pdu[1], pdu[2]]);
                let response = if pdu[0] == 6 {
                    writes += 1;
                    assert_eq!(header[6], 247);
                    assert_eq!(pdu, [6, 0x9c, 0x6e, 0, 200]);
                    vec![6, 0x9c, 0x6e, 0, 201] // Wrong echo: outcome uncertain.
                } else {
                    assert_eq!(pdu[0], 4);
                    let count = u16::from_be_bytes([pdu[3], pdu[4]]);
                    let mut reply = vec![4, (count * 2) as u8];
                    let mut words = vec![0_u16; count as usize];
                    if register == 40029 {
                        words[0] = 1;
                    }
                    if register == 40031 {
                        words[0] = 2;
                    }
                    for word in words {
                        reply.extend(word.to_be_bytes());
                    }
                    reply
                };
                header[4..6].copy_from_slice(&(response.len() as u16 + 1).to_be_bytes());
                stream.write_all(&header).unwrap();
                stream.write_all(&response).unwrap();
            }
            assert_eq!(writes, 1);
        });
        let status = Arc::new(RwLock::new(Status::new(address.to_string(), 1)));
        let sender = start(
            address,
            UnitId::inverter(1).unwrap(),
            Config::default(),
            Duration::from_secs(15),
            status.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(25);
        while !status.read().unwrap().controls_ready() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        let (reply1, response1) = oneshot::channel();
        let (reply2, response2) = oneshot::channel();
        {
            let s = status.read().unwrap();
            sender
                .try_send(Job::new(Command::BackupSoc { percent: 20.0 }, reply1, &s).unwrap())
                .unwrap();
            sender
                .try_send(Job::new(Command::EmsMode { mode: 3 }, reply2, &s).unwrap())
                .unwrap();
        }
        assert!(
            response1
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("echo mismatch")
        );
        assert!(
            response2
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("hardware state changed")
        );
        {
            let mut s = status.write().unwrap();
            assert!(!s.online);
            assert!(!s.controls_ready());
            s.shutting_down = true;
        }
        drop(sender);
        mock.join().unwrap();
    }
}
