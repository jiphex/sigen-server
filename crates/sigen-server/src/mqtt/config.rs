use crate::cli::MqttArgs;
use rumqttc::{LastWill, MqttOptions, QoS, Transport};
use std::time::Duration;

// Deliberately no Debug implementation: credentials must not enter logs.
pub struct Config {
    pub host: String,
    pub port: u16,
    pub id: String,
    pub prefix: String,
    pub discovery_prefix: String,
    pub birth_topic: String,
    pub controls: bool,
    pub stale_after: u64,
    username: Option<String>,
    password: Option<String>,
    tls: bool,
    ca: Option<Vec<u8>>,
}
impl Config {
    pub fn from_args(
        args: &MqttArgs,
        target: &str,
        unit: u8,
        poll_seconds: u64,
    ) -> Result<Option<Self>, String> {
        let Some(host) = args.mqtt_host.clone() else {
            return Ok(None);
        };
        if host.is_empty() || host.contains(['/', '\0', ' ']) {
            return Err("SIGEN_MQTT_HOST must be a hostname or IP address".into());
        }
        let tls = args.mqtt_tls;
        let port = args.mqtt_port.unwrap_or(if tls { 8883 } else { 1883 });
        if port == 0 {
            return Err("MQTT port must be 1..=65535".into());
        }
        let default_id = format!(
            "sigen_{}_{}",
            target
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>(),
            unit
        );
        let id = args.mqtt_id.clone().unwrap_or(default_id);
        if id.is_empty()
            || id.len() > 128
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(
                "SIGEN_MQTT_ID must contain 1–128 letters, numbers, underscores or hyphens".into(),
            );
        }
        let prefix = args
            .mqtt_topic_prefix
            .clone()
            .unwrap_or_else(|| format!("sigen/{id}"));
        let discovery_prefix = args
            .mqtt_discovery_prefix
            .clone()
            .unwrap_or_else(|| "homeassistant".into());
        let birth_topic = args
            .mqtt_ha_status_topic
            .clone()
            .unwrap_or_else(|| "homeassistant/status".into());
        for (name, value) in [
            ("SIGEN_MQTT_TOPIC_PREFIX", &prefix),
            ("SIGEN_MQTT_DISCOVERY_PREFIX", &discovery_prefix),
            ("SIGEN_MQTT_HA_STATUS_TOPIC", &birth_topic),
        ] {
            if value.len() > 512
                || value.split('/').any(|level| {
                    level.is_empty()
                        || !level
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
            {
                return Err(format!(
                    "{name} must contain nonempty topic levels with letters, numbers, underscores or hyphens"
                ));
            }
        }
        // Avoid a birth message or discovery publication being interpreted as a command.
        if birth_topic.starts_with(&format!("{prefix}/"))
            || prefix == discovery_prefix
            || prefix.starts_with(&format!("{discovery_prefix}/"))
        {
            return Err(
                "MQTT state, discovery and Home Assistant birth topic namespaces must be separate"
                    .into(),
            );
        }
        let username = args.mqtt_username.clone();
        let password = args.mqtt_password.clone();
        if password.is_some() && username.is_none() {
            return Err("SIGEN_MQTT_PASSWORD requires SIGEN_MQTT_USERNAME".into());
        }
        let ca = match &args.mqtt_ca_file {
            Some(path) if tls => {
                Some(std::fs::read(path).map_err(|_| "Cannot read SIGEN_MQTT_CA_FILE")?)
            }
            Some(_) => return Err("SIGEN_MQTT_CA_FILE requires SIGEN_MQTT_TLS=true".into()),
            None => None,
        };
        Ok(Some(Self {
            host,
            port,
            id,
            prefix,
            discovery_prefix,
            birth_topic,
            controls: args.mqtt_controls,
            stale_after: poll_seconds + 30,
            username,
            password,
            tls,
            ca,
        }))
    }
    pub fn options(&self) -> MqttOptions {
        let mut options = MqttOptions::new(&self.id, &self.host, self.port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_clean_session(true);
        options.set_max_packet_size(128 * 1024, 128 * 1024);
        options.set_last_will(LastWill::new(
            format!("{}/availability", self.prefix),
            "offline",
            QoS::AtLeastOnce,
            true,
        ));
        if let Some(username) = &self.username {
            options.set_credentials(username, self.password.as_deref().unwrap_or(""));
        }
        if self.tls {
            options.set_transport(match &self.ca {
                Some(ca) => Transport::tls(ca.clone(), None, None),
                None => Transport::tls_with_default_config(),
            });
        }
        options
    }
}
#[cfg(test)]
pub use tests::config as test_config;

#[cfg(test)]
mod tests {
    use super::*;
    fn args(extra: &[(&str, &str)]) -> MqttArgs {
        let mut args = MqttArgs {
            mqtt_host: Some("localhost".into()),
            ..MqttArgs::default()
        };
        for (key, value) in extra {
            match *key {
                "SIGEN_MQTT_ID" => args.mqtt_id = Some(value.to_string()),
                "SIGEN_MQTT_TLS" => args.mqtt_tls = *value == "true",
                "SIGEN_MQTT_CONTROLS" => args.mqtt_controls = *value == "true",
                "SIGEN_MQTT_PORT" => args.mqtt_port = Some(value.parse().unwrap()),
                "SIGEN_MQTT_PASSWORD" => args.mqtt_password = Some(value.to_string()),
                "SIGEN_MQTT_TOPIC_PREFIX" => args.mqtt_topic_prefix = Some(value.to_string()),
                "SIGEN_MQTT_CA_FILE" => args.mqtt_ca_file = Some(value.into()),
                _ => panic!("unknown test option"),
            }
        }
        args
    }
    pub fn config(extra: &[(&str, &str)]) -> Config {
        Config::from_args(&args(extra), "192.168.1.50:502", 1, 15)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn optional_config_stable_identity_and_tls_defaults() {
        assert!(
            Config::from_args(&MqttArgs::default(), "a", 1, 15)
                .unwrap()
                .is_none()
        );
        let c = config(&[]);
        assert_eq!(c.id, "sigen_192_168_1_50_502_1");
        assert_eq!(c.port, 1883);
        assert!(!c.controls);
        let secure = config(&[("SIGEN_MQTT_TLS", "true"), ("SIGEN_MQTT_ID", "my_battery")]);
        assert_eq!(secure.port, 8883);
        assert_eq!(secure.prefix, "sigen/my_battery");
        assert!(secure.options().last_will().unwrap().retain);
        assert!(secure.options().clean_session());
    }
    #[test]
    fn invalid_credentials_topics_and_flags_are_rejected() {
        for (key, value) in [
            ("SIGEN_MQTT_ID", "bad/id"),
            ("SIGEN_MQTT_PORT", "0"),
            ("SIGEN_MQTT_PASSWORD", "secret"),
            ("SIGEN_MQTT_TOPIC_PREFIX", "bad/+/topic"),
            ("SIGEN_MQTT_TOPIC_PREFIX", "homeassistant/device"),
            ("SIGEN_MQTT_CA_FILE", "/missing"),
        ] {
            assert!(
                Config::from_args(&args(&[(key, value)]), "a", 1, 15).is_err(),
                "{key}"
            );
        }
    }
}
