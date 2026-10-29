use super::*;
use crate::model::{Controls, Device};
use bytes::BytesMut;
use rumqttc::mqttbytes::{
    self,
    v4::{ConnAck, ConnectReturnCode, Packet, SubAck, SubscribeReasonCode},
};
use serde_json::Value;
use std::sync::RwLock;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn config(controls: bool) -> Config {
    config::test_config(&[
        ("SIGEN_MQTT_ID", "test_sigen"),
        (
            "SIGEN_MQTT_CONTROLS",
            if controls { "true" } else { "false" },
        ),
    ])
}
fn status() -> Status {
    let mut s = Status::new("192.168.1.50:502".into(), 1);
    s.online = true;
    s.control_valid_until = Some(std::time::Instant::now() + Duration::from_secs(45));
    s.last_success_unix = Some(hardware::now());
    s.device_day = Some(20000);
    s.details = crate::telemetry::test_details(hardware::now(), 20000);
    s.controls = Some(Controls {
        remote_ems_enabled: true,
        remote_ems_mode: 2,
        backup_soc_percent: 20.0,
    });
    s.device = Some(Device {
        model: "Sigen Test".into(),
        serial_number: "ABC123".into(),
        firmware_version: "V1".into(),
    });
    s
}
fn command(topic: &str, payload: &str) -> Publish {
    Publish::new(
        format!("sigen/test_sigen/command/{topic}"),
        QoS::AtMostOnce,
        payload,
    )
}

#[test]
fn discovery_groups_entities_and_has_stable_ids_classes_and_nonoptimistic_controls() {
    let c = config(true);
    let mut s = status();
    let doc = discovery::payload(&c, &s);
    assert_eq!(doc["device"]["identifiers"][0], "test_sigen");
    assert_eq!(doc["device"]["serial_number"], "ABC123");
    assert_eq!(doc["origin"]["name"], "sigen-server");
    assert_eq!(doc["availability_mode"], "all");
    let components = doc["components"].as_object().unwrap();
    assert_eq!(components.len(), 37);
    let mut ids = std::collections::HashSet::new();
    for entity in components.values() {
        assert!(ids.insert(entity["unique_id"].as_str().unwrap()));
        assert!(!entity["platform"].as_str().unwrap().is_empty());
    }
    assert_eq!(components["pv_power_watts"]["device_class"], "power");
    assert_eq!(components["pv_power_watts"]["unit_of_measurement"], "W");
    assert_eq!(components["battery_soc_percent"]["device_class"], "battery");
    // Available energy is an instantaneous amount, not a cumulative energy counter.
    assert_eq!(
        components["battery_charge_available_wh"]["state_class"],
        "measurement"
    );
    assert_eq!(
        components["battery_charge_available_wh"]["device_class"],
        "energy_storage"
    );
    for name in ["remote_ems", "ems_mode", "backup_soc"] {
        assert_eq!(components[name]["optimistic"], false);
        assert_eq!(components[name]["retain"], false);
        assert_eq!(components[name]["availability_mode"], "all");
    }
    assert_eq!(
        components["ems_mode"]["options"].as_array().unwrap().len(),
        7
    );
    assert_eq!(components["backup_soc"]["step"], 0.1);
    let offline_alert = &components["alert_hardware_offline"];
    assert_eq!(offline_alert["device_class"], "problem");
    // It must remain available while hardware is offline; don't inherit hardware availability.
    assert!(
        !offline_alert["availability"]
            .to_string()
            .contains("value_json.online")
    );
    s.device = None;
    assert_eq!(discovery::payload(&c, &s)["components"], doc["components"]);
    assert!(
        discovery::payload(&config(false), &s)["components"]
            .get("remote_ems")
            .is_none()
    );
}

#[test]
fn invalid_retained_and_duplicate_commands_are_never_accepted() {
    let c = config(true);
    assert!(matches!(
        parse_command(&c, &command("remote_ems", "ON")).unwrap(),
        Command::RemoteEms { enabled: true }
    ));
    assert!(matches!(
        parse_command(&c, &command("ems_mode", discovery::MODES[3])).unwrap(),
        Command::EmsMode { mode: 3 }
    ));
    assert!(matches!(
        parse_command(&c, &command("backup_soc", "12.5")).unwrap(),
        Command::BackupSoc { percent: 12.5 }
    ));
    for (topic, payload) in [
        ("backup_soc", "101"),
        ("backup_soc", "NaN"),
        ("backup_soc", "-1"),
        ("backup_soc", "inf"),
        ("ems_mode", "99"),
        ("remote_ems", "true"),
        ("arbitrary_register", "1"),
    ] {
        assert!(parse_command(&c, &command(topic, payload)).is_err());
    }
    let mut retained = command("remote_ems", "ON");
    retained.retain = true;
    assert!(parse_command(&c, &retained).is_err());
    retained.retain = false;
    retained.dup = true;
    assert!(parse_command(&c, &retained).is_err());
    assert!(parse_command(&config(false), &command("remote_ems", "ON")).is_err());
}

#[test]
fn stale_and_offline_snapshots_do_not_publish_measurements_or_controls() {
    let c = config(true);
    let mut s = status();
    assert!(snapshot_payload(&c, &s)["online"].as_bool().unwrap());
    assert_eq!(
        snapshot_payload(&c, &s)["controls"]["backup_soc_percent"],
        20.0
    );
    s.last_success_unix = Some(hardware::now() - c.stale_after - 1);
    let payload = snapshot_payload(&c, &s);
    assert_eq!(payload["online"], false);
    assert!(payload["controls"].is_null());
    assert!(payload["readings"].is_null());
    assert!(payload["details"].is_null());
    s.last_success_unix = None;
    assert!(!fresh(&c, &s));
    s.last_success_unix = Some(hardware::now());
    s.online = false;
    assert!(!fresh(&c, &s));
    assert!(snapshot_payload(&c, &s)["details"].is_null());
}

#[test]
fn mqtt_includes_optional_groups_with_independent_freshness_and_exact_counters() {
    let c = config(false);
    let mut s = status();
    let payload = snapshot_payload(&c, &s);
    assert_eq!(payload["device_day"], 20000);
    assert_eq!(payload["details"]["pv_daily_kwh"]["value"], 14.82);
    assert_eq!(
        payload["details"]["battery_daily"]["value"]["charge_kwh"],
        6.24
    );
    assert_eq!(
        payload["details"]["grid_import"]["value"],
        "18446744073709551615"
    );
    assert_eq!(
        payload["details"]["electrical"]["value"]["phase_current_amps"][0],
        -1.0
    );
    s.details.electrical.updated_unix = Some(hardware::now() - 121);
    s.details.load_daily_kwh.device_day = Some(19999);
    s.details.pv_daily_kwh.unsupported = true;
    s.details.battery_daily.last_error = Some("read failed".into());
    let payload = snapshot_payload(&c, &s);
    for name in [
        "electrical",
        "load_daily_kwh",
        "pv_daily_kwh",
        "battery_daily",
        "grid_phases",
    ] {
        assert!(payload["details"][name]["value"].is_null(), "{name}");
    }
    assert_eq!(payload["details"]["pv_daily_kwh"]["unsupported"], true);
    assert!(payload["details"]["electrical"]["updated_unix"].is_number());
    assert_eq!(payload["details"]["grid_export"]["value"], "0");
    // Filtering publications must not erase the dashboard's last-known readings.
    assert!(s.details.electrical.value.is_some());
}

struct Broker {
    stream: TcpStream,
    buffer: BytesMut,
}
impl Broker {
    async fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut broker = Self {
            stream,
            buffer: BytesMut::new(),
        };
        let Packet::Connect(connect) = broker.read().await else {
            panic!("expected CONNECT")
        };
        assert!(connect.clean_session);
        assert_eq!(connect.client_id, "test_sigen");
        let will = connect.last_will.unwrap();
        assert_eq!(will.topic, "sigen/test_sigen/availability");
        assert_eq!(will.message.as_ref(), b"offline");
        assert!(will.retain);
        broker
            .send(Packet::ConnAck(ConnAck::new(
                ConnectReturnCode::Success,
                false,
            )))
            .await;
        broker
    }
    async fn read(&mut self) -> Packet {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                match Packet::read(&mut self.buffer, 128 * 1024) {
                    Ok(packet) => return packet,
                    Err(mqttbytes::Error::InsufficientBytes(_)) => (),
                    Err(e) => panic!("MQTT decoding: {e}"),
                }
                assert_ne!(
                    self.stream.read_buf(&mut self.buffer).await.unwrap(),
                    0,
                    "unexpected EOF"
                );
            }
        })
        .await
        .expect("MQTT broker receive timeout")
    }
    async fn send(&mut self, packet: Packet) {
        let mut buffer = BytesMut::new();
        packet.write(&mut buffer, 128 * 1024).unwrap();
        self.stream.write_all(&buffer).await.unwrap();
    }
    async fn next_publish(&mut self) -> Publish {
        loop {
            match self.read().await {
                Packet::Subscribe(subscribe) => {
                    self.send(Packet::SubAck(SubAck::new(
                        subscribe.pkid,
                        vec![
                            SubscribeReasonCode::Success(QoS::AtMostOnce);
                            subscribe.filters.len()
                        ],
                    )))
                    .await;
                }
                Packet::Publish(p) => return p,
                Packet::PingReq => self.send(Packet::PingResp).await,
                packet => panic!("unexpected packet: {packet:?}"),
            }
        }
    }
    async fn topic(&mut self, topic: &str) -> Publish {
        loop {
            let p = self.next_publish().await;
            if p.topic == topic {
                return p;
            }
        }
    }
}

#[tokio::test]
async fn broker_receives_retained_discovery_state_birth_republication_and_clean_shutdown() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = config(false);
    c.host = "127.0.0.1".into();
    c.port = listener.local_addr().unwrap().port();
    let shared = Arc::new(RwLock::new(status()));
    let (commands, receiver) = mpsc::sync_channel(4);
    let (stop, stopped) = watch::channel(false);
    let task = start(c, shared, commands, stopped);
    let mut broker = Broker::accept(&listener).await;
    let discovery = broker.topic("homeassistant/device/test_sigen/config").await;
    assert!(discovery.retain);
    // First config explicitly removes controls previously enabled in an earlier run.
    let cleanup: Value = serde_json::from_slice(&discovery.payload).unwrap();
    assert_eq!(
        cleanup["components"]["remote_ems"],
        json!({"platform":"switch"})
    );
    let config = broker.topic("homeassistant/device/test_sigen/config").await;
    let config: Value = serde_json::from_slice(&config.payload).unwrap();
    assert!(config["components"].get("remote_ems").is_none());
    let state = broker.topic("sigen/test_sigen/state").await;
    assert!(state.retain);
    let state: Value = serde_json::from_slice(&state.payload).unwrap();
    assert_eq!(state["online"], true);
    assert_eq!(state["details"]["load_daily_kwh"]["value"], 11.37);
    assert_eq!(
        state["details"]["grid_import"]["value"],
        "18446744073709551615"
    );
    assert_eq!(
        broker
            .topic("sigen/test_sigen/availability")
            .await
            .payload
            .as_ref(),
        b"online"
    );
    broker
        .send(Packet::Publish(Publish::new(
            "homeassistant/status",
            QoS::AtMostOnce,
            "online",
        )))
        .await;
    assert!(
        broker
            .topic("homeassistant/device/test_sigen/config")
            .await
            .retain
    );
    // Wait until periodic/birth traffic is drained by waiting for availability.
    broker.topic("sigen/test_sigen/availability").await;
    stop.send(true).unwrap();
    let offline = broker.topic("sigen/test_sigen/availability").await;
    assert!(offline.retain);
    assert_eq!(offline.payload.as_ref(), b"offline");
    assert!(matches!(broker.read().await, Packet::Disconnect));
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn broker_commands_use_hardware_queue_and_publish_only_confirmed_readback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = config(true);
    c.host = "127.0.0.1".into();
    c.port = listener.local_addr().unwrap().port();
    let shared = Arc::new(RwLock::new(status()));
    let (commands, receiver) = mpsc::sync_channel::<Job>(4);
    let (stop, stopped) = watch::channel(false);
    let task = start(c, shared.clone(), commands, stopped);
    let mut broker = Broker::accept(&listener).await;
    broker.topic("sigen/test_sigen/availability").await;
    let mut retained = command("backup_soc", "99");
    retained.retain = true;
    broker.send(Packet::Publish(retained)).await;
    let rejected = broker.topic("sigen/test_sigen/command_result").await;
    let rejected: Value = serde_json::from_slice(&rejected.payload).unwrap();
    assert_eq!(rejected["confirmed"], false);
    assert!(receiver.try_recv().is_err());
    broker
        .send(Packet::Publish(command("backup_soc", "30")))
        .await;
    let hardware_status = shared.clone();
    let worker = std::thread::spawn(move || {
        let job = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(job.source.starts_with(
            "mqtt topic=sigen/test_sigen/command/backup_soc bridge=test_sigen broker="
        ));
        assert!(matches!(job.command, Command::BackupSoc { percent: 30.0 }));
        let controls = Controls {
            remote_ems_enabled: true,
            remote_ems_mode: 2,
            backup_soc_percent: 30.0,
        };
        hardware_status.write().unwrap().controls = Some(controls.clone());
        job.reply.send(Ok(controls)).unwrap();
    });
    let result = broker.topic("sigen/test_sigen/command_result").await;
    assert!(!result.retain);
    let result: Value = serde_json::from_slice(&result.payload).unwrap();
    assert_eq!(result["confirmed"], true);
    assert_eq!(result["controls"]["backup_soc_percent"], 30.0);
    let state = broker.topic("sigen/test_sigen/state").await;
    let state: Value = serde_json::from_slice(&state.payload).unwrap();
    assert_eq!(state["controls"]["backup_soc_percent"], 30.0);
    worker.join().unwrap();
    stop.send(true).unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn broker_disconnect_reconnects_and_republishes_discovery() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut c = config(true);
    c.host = "127.0.0.1".into();
    c.port = listener.local_addr().unwrap().port();
    let shared = Arc::new(RwLock::new(status()));
    let (commands, _receiver) = mpsc::sync_channel(4);
    let (stop, stopped) = watch::channel(false);
    let task = start(c, shared.clone(), commands, stopped);
    let mut broker = Broker::accept(&listener).await;
    broker.topic("sigen/test_sigen/availability").await;
    drop(broker);
    let mut broker = Broker::accept(&listener).await;
    assert!(
        broker
            .topic("homeassistant/device/test_sigen/config")
            .await
            .retain
    );
    broker.topic("sigen/test_sigen/availability").await;
    assert!(shared.read().unwrap().mqtt.as_ref().unwrap().connected);
    stop.send(true).unwrap();
    task.await.unwrap();
}
