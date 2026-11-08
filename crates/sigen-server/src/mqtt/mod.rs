mod config;
mod discovery;
#[cfg(test)]
mod tests;

use crate::{
    hardware::{self, Command, Job, SharedStatus},
    model::{MqttStatus, Status},
};
pub use config::Config;
use rumqttc::{AsyncClient, Event, Incoming, Outgoing, Publish, QoS};
use serde_json::json;
use std::{
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio::sync::{Notify, Semaphore, watch};

/// No blocking network work on the HTTP runtime, and no additional Modbus polls.
pub fn start(
    config: Config,
    status: SharedStatus,
    commands: mpsc::SyncSender<Job>,
    stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    status.write().expect("status lock").mqtt = Some(MqttStatus {
        connected: false,
        last_error: None,
        last_command_error: None,
        topic_prefix: config.prefix.clone(),
    });
    // Broker I/O, timers, and shutdown waits are async, so they yield Tokio workers.
    tokio::spawn(run(config, status, commands, stop))
}

#[tracing::instrument(name = "mqtt", skip_all, fields(broker = %config.host, port = config.port, client_id = %config.id))]
async fn run(
    config: Config,
    status: SharedStatus,
    commands: mpsc::SyncSender<Job>,
    mut stop: watch::Receiver<bool>,
) {
    tracing::info!(broker = %config.host, port = config.port, client_id = %config.id, controls = config.controls, "Starting MQTT bridge");
    let (client, mut events) = AsyncClient::new(config.options(), 32);
    let config = Arc::new(config);
    let gate = Arc::new(Semaphore::new(1));
    let changed = Arc::new(Notify::new());
    let mut timer = tokio::time::interval(Duration::from_secs(5));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut connected = false;
    let mut previous_discovery = None;
    let mut setup_needed = true;
    loop {
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            event = events.poll() => match event {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    connected = true; setup_needed = true; previous_discovery = None;
                    let mut s = status.write().expect("status lock");
                    let m = s.mqtt.as_mut().unwrap(); m.connected = true; m.last_error = None;
                    tracing::info!("MQTT connected; publishing Home Assistant discovery");
                    // Make the timer immediately perform subscriptions and initial publication.
                    timer.reset_immediately();
                }
                Ok(Event::Incoming(Incoming::Publish(message))) => {
                    if message.topic == config.birth_topic && message.payload.as_ref() == b"online" {
                        previous_discovery = None;
                        timer.reset_immediately();
                    } else if config.controls && message.topic.starts_with(&format!("{}/command/",config.prefix)) {
                        handle_command(message, config.clone(), status.clone(), commands.clone(), client.clone(), gate.clone(), changed.clone());
                    }
                }
                Ok(_) => (),
                Err(_) => {
                    connected = false;
                    {
                        let mut s = status.write().expect("status lock");
                        let m = s.mqtt.as_mut().unwrap();
                        if m.connected || m.last_error.is_none() { tracing::warn!(retry_seconds = 5, "MQTT connection unavailable"); }
                        m.connected = false;
                        // Do not log the connection error: broker errors may contain credentials.
                        m.last_error = Some("Broker connection unavailable; retrying".into());
                    }
                    tokio::select! { _ = stop.changed() => break, _ = tokio::time::sleep(Duration::from_secs(5)) => () }
                }
            },
            _ = timer.tick(), if connected => {
                let snapshot = status.read().expect("status lock").clone();
                let result = (|| -> Result<(), String> {
                    if setup_needed {
                        client.try_subscribe(&config.birth_topic,QoS::AtMostOnce).map_err(|_| "Cannot queue MQTT subscription")?;
                        if config.controls { client.try_subscribe(format!("{}/command/+",config.prefix),QoS::AtMostOnce).map_err(|_| "Cannot queue control subscription")?; }
                        setup_needed = false;
                    }
                    let document = discovery::payload(&config, &snapshot).to_string();
                    if previous_discovery.as_ref() != Some(&document) {
                        if !config.controls {
                            // Explicitly remove previously enabled control entities, then omit them.
                            let mut cleanup: serde_json::Value = serde_json::from_str(&document).expect("discovery JSON");
                            for (key,platform) in [("remote_ems","switch"),("ems_mode","select"),("backup_soc","number")] {
                                cleanup["components"][key] = json!({"platform":platform});
                            }
                            publish(&client,&format!("{}/device/{}/config",config.discovery_prefix,config.id),true,cleanup.to_string())?;
                        }
                        publish(&client,&format!("{}/device/{}/config",config.discovery_prefix,config.id),true,document.clone())?;
                        previous_discovery = Some(document);
                    }
                    publish_state(&client,&config,&snapshot)?;
                    // Publish online only after the latest state is queued, in order.
                    publish(&client,&format!("{}/availability",config.prefix),true,"online")
                })();
                if let Err(error) = &result { tracing::warn!(%error, "MQTT publication failed"); }
                status.write().expect("status lock").mqtt.as_mut().unwrap().last_error = result.err();
            },
            _ = changed.notified(), if connected => {
                let snapshot = status.read().expect("status lock").clone();
                let _ = publish_state(&client,&config,&snapshot);
            },
        }
    }
    // Flush offline before DISCONNECT (which suppresses the last will).
    // If queueing fails, dropping the socket lets the broker deliver the will instead.
    if connected
        && publish(
            &client,
            &format!("{}/availability", config.prefix),
            true,
            "offline",
        )
        .is_ok()
        && client.try_disconnect().is_ok()
    {
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match events.poll().await {
                    Ok(Event::Outgoing(Outgoing::Disconnect)) | Err(_) => break,
                    _ => (),
                }
            }
        })
        .await;
    }
    status
        .write()
        .expect("status lock")
        .mqtt
        .as_mut()
        .unwrap()
        .connected = false;
}

// Nonblocking queue operations are essential: this same task drives the event loop.
fn publish(
    client: &AsyncClient,
    topic: &str,
    retain: bool,
    payload: impl Into<Vec<u8>>,
) -> Result<(), String> {
    client
        .try_publish(topic, QoS::AtMostOnce, retain, payload)
        .map_err(|_| "MQTT publication queue is full or unavailable".into())
}
/// Keep diagnostics visible offline, but publish null measurements and controls when stale.
fn snapshot_payload(config: &Config, status: &Status) -> serde_json::Value {
    let online = fresh(config, status);
    json!({"server_version":status.server_version,"alerts":status.alerts,"online":online,"last_success_unix":status.last_success_unix,"target":status.target,"unit":status.unit,
        "device_day":status.device_day,
        "details":if online { serde_json::to_value(status.details.for_export(hardware::now(), status.device_day,
            status.readings.as_ref().is_some_and(|r| r.grid_sensor_connected))).unwrap() } else { serde_json::Value::Null },
        "readings":if online { serde_json::to_value(&status.readings).unwrap() } else { serde_json::Value::Null },
        "controls":if online { serde_json::to_value(&status.controls).unwrap() } else { serde_json::Value::Null }})
}
fn fresh(config: &Config, status: &Status) -> bool {
    status.fresh()
        && status
            .last_success_unix
            .is_some_and(|t| hardware::now().saturating_sub(t) <= config.stale_after)
}
fn publish_state(client: &AsyncClient, config: &Config, status: &Status) -> Result<(), String> {
    publish(
        client,
        &format!("{}/state", config.prefix),
        true,
        snapshot_payload(config, status).to_string(),
    )
}

/// Decode explicit control topics, rejecting retained or duplicate deliveries so stored
/// broker messages and retransmissions cannot replay hardware commands.
fn parse_command(config: &Config, message: &Publish) -> Result<Command, String> {
    if !config.controls {
        return Err("MQTT controls are disabled".into());
    }
    if message.retain || message.dup {
        return Err("Retained or duplicate commands are not executed".into());
    }
    if message.payload.len() > 64 {
        return Err("Command payload is too large".into());
    }
    let payload = std::str::from_utf8(&message.payload)
        .map_err(|_| "Command must be UTF-8")?
        .trim();
    let suffix = message
        .topic
        .strip_prefix(&format!("{}/command/", config.prefix))
        .ok_or("Unknown command topic")?;
    let command = match suffix {
        "remote_ems" => match payload {
            "ON" => Command::RemoteEms { enabled: true },
            "OFF" => Command::RemoteEms { enabled: false },
            _ => return Err("Remote EMS command must be ON or OFF".into()),
        },
        "ems_mode" => Command::EmsMode {
            mode: discovery::MODES
                .iter()
                .position(|mode| *mode == payload)
                .ok_or("Unknown remote EMS mode")? as u16,
        },
        "backup_soc" => Command::BackupSoc {
            percent: payload
                .parse()
                .map_err(|_| "Backup reserve must be a number")?,
        },
        _ => return Err("Unknown command topic".into()),
    };
    command.validate()?;
    Ok(command)
}

/// Allow one MQTT command awaiting confirmation at a time; reject contention immediately
/// instead of accumulating commands that may become stale before execution.
fn handle_command(
    message: Publish,
    config: Arc<Config>,
    status: SharedStatus,
    commands: mpsc::SyncSender<Job>,
    client: AsyncClient,
    gate: Arc<Semaphore>,
    changed: Arc<Notify>,
) {
    let command = parse_command(&config, &message);
    let permit = gate.try_acquire_owned();
    let result = match (command, permit) {
        (Ok(command), Ok(permit)) => {
            let ready = {
                let s = status.read().expect("status lock");
                fresh(&config, &s) && s.controls_ready()
            };
            if !ready {
                Err("Hardware is offline or stale; no command was sent".into())
            } else {
                let (reply, response) = tokio::sync::oneshot::channel();
                let job =
                    Job::new(command, reply, &status.read().expect("status lock")).map(|job| {
                        job.with_source(format!(
                            "mqtt topic={} bridge={} broker={}:{}",
                            message.topic, config.id, config.host, config.port
                        ))
                    });
                // Await hardware confirmation separately so broker events keep being polled.
                // try_send never waits for queue space; the oneshot reply bridges to the hardware thread.
                tokio::spawn(async move {
                    let _permit = permit;
                    let queued = job.and_then(|job| hardware::enqueue(&commands, job, &status).map_err(|_| "Hardware command queue is full or unavailable; command was not queued".to_owned()));
                    let result = match queued {
                        Ok(()) => match tokio::time::timeout(Duration::from_secs(60),response).await {
                            Ok(Ok(result)) => result,
                            _ => Err("Command confirmation unavailable; it may have executed. Check readback before retrying".into()),
                        },
                        Err(error) => Err(error),
                    };
                    command_result(&client, &config, &status, result);
                    changed.notify_one();
                });
                return;
            }
        }
        (Err(e), _) => Err(e),
        (_, Err(_)) => Err("An MQTT command is already in progress; no command was queued".into()),
    };
    command_result(&client, &config, &status, result);
}
fn command_result(
    client: &AsyncClient,
    config: &Config,
    status: &SharedStatus,
    result: Result<crate::model::Controls, String>,
) {
    status
        .write()
        .expect("status lock")
        .mqtt
        .as_mut()
        .unwrap()
        .last_command_error = result.as_ref().err().cloned();
    let payload = match result {
        Ok(controls) => {
            tracing::info!("MQTT command confirmed");
            json!({"confirmed":true,"confirmation":"setting_readback", "electrical_output_verified":false,"controls":controls})
        }
        Err(error) => {
            tracing::warn!(%error, "MQTT command failed or was rejected");
            json!({"confirmed":false,"error":error})
        }
    };
    let _ = publish(
        client,
        &format!("{}/command_result", config.prefix),
        false,
        payload.to_string(),
    );
}
