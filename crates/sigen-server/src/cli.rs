use clap::{ArgAction, Args, Parser};
use std::{net::SocketAddr, path::PathBuf};

/// Sigenergy status dashboard, controls, Prometheus exporter and MQTT bridge.
// No Debug derive: the parsed options include broker credentials.
#[derive(Parser)]
#[command(name = "sigen-server", version, about)]
pub struct Cli {
    /// Inverter Modbus endpoint (IP:port)
    #[arg(long, env = "SIGEN_ADDRESS", value_name = "IP:PORT")]
    pub address: SocketAddr,
    /// HTTP listening address
    #[arg(
        long,
        env = "SIGEN_BIND",
        default_value = "127.0.0.1:3000",
        value_name = "IP:PORT"
    )]
    pub bind: SocketAddr,
    /// Append-only JSONL log of all Modbus write transmissions
    #[arg(
        long,
        env = "SIGEN_WRITE_AUDIT_FILE",
        default_value = "sigen-writes.jsonl"
    )]
    pub write_audit_file: PathBuf,
    /// SQLite file for the persistent event journal (parent directory must exist)
    #[arg(
        long,
        env = "SIGEN_JOURNAL_FILE",
        default_value = "sigen-events.sqlite3"
    )]
    pub journal_file: PathBuf,
    /// Additional trusted HTTP Host authorities (hostname[:port]); repeat or comma-separate
    #[arg(long, env = "SIGEN_ALLOWED_HOSTS", value_delimiter = ',', value_parser = parse_authority)]
    pub allowed_host: Vec<String>,
    /// Inverter unit ID
    #[arg(long, env = "SIGEN_UNIT_ID", default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=246))]
    pub unit_id: u8,
    /// Delay between complete hardware polls
    #[arg(long, env = "SIGEN_POLL_SECONDS", default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub poll_seconds: u64,
    /// Modbus telemetry read function
    #[arg(long, env = "SIGEN_READ_FUNCTION", default_value_t = 4, value_parser = clap::value_parser!(u8).range(3..=4))]
    pub read_function: u8,
    /// DANGER: enable Modbus writes that can change whole-plant hardware settings
    #[arg(long, env = "SIGEN_DANGEROUSLY_ENABLE_MODBUS_WRITES", default_value_t = false, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub dangerously_enable_modbus_writes: bool,
    /// File storing alert on/off settings
    #[arg(long, env = "SIGEN_ALERTS_FILE", default_value = "sigen-alerts.json")]
    pub alerts_file: PathBuf,
    #[command(flatten)]
    pub mqtt: MqttArgs,
    #[command(flatten)]
    pub pushover: PushoverArgs,
}

fn parse_authority(value: &str) -> Result<String, String> {
    let authority = value
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| "Expected hostname[:port]")?;
    if value.contains('@') || authority.host().is_empty() || value.contains('*') {
        return Err("Expected an exact hostname[:port], without credentials or wildcards".into());
    }
    Ok(value.to_ascii_lowercase())
}

#[derive(Args, Default)]
#[command(next_help_heading = "Pushover")]
#[group(multiple = true)]
pub struct PushoverArgs {
    /// Application API token; enables notifications (prefer the environment variable)
    #[arg(
        long,
        env = "SIGEN_PUSHOVER_TOKEN",
        hide_env_values = true,
        requires = "pushover_user"
    )]
    pub pushover_token: Option<String>,
    /// Pushover user or group key (prefer the environment variable)
    #[arg(
        long,
        env = "SIGEN_PUSHOVER_USER",
        hide_env_values = true,
        requires = "pushover_token"
    )]
    pub pushover_user: Option<String>,
    /// Optional destination device name; otherwise all user devices
    #[arg(long, env = "SIGEN_PUSHOVER_DEVICE", requires = "pushover_token")]
    pub pushover_device: Option<String>,
    /// Send a recovery notification when an alert clears
    #[arg(long, env = "SIGEN_PUSHOVER_RECOVERY", default_value_t = true, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub pushover_recovery: bool,
}

#[derive(Args, Default)]
#[command(next_help_heading = "MQTT")]
#[group(multiple = true)]
pub struct MqttArgs {
    /// Broker hostname or IP; enables MQTT
    #[arg(long, env = "SIGEN_MQTT_HOST", help_heading = "MQTT")]
    pub mqtt_host: Option<String>,
    /// Broker port (default: 1883, or 8883 with TLS)
    #[arg(long, env = "SIGEN_MQTT_PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub mqtt_port: Option<u16>,
    /// Stable MQTT client and Home Assistant device ID
    #[arg(long, env = "SIGEN_MQTT_ID")]
    pub mqtt_id: Option<String>,
    /// MQTT state/command topic prefix (default: sigen/<id>)
    #[arg(long, env = "SIGEN_MQTT_TOPIC_PREFIX")]
    pub mqtt_topic_prefix: Option<String>,
    /// Home Assistant discovery prefix
    #[arg(
        long,
        env = "SIGEN_MQTT_DISCOVERY_PREFIX",
        default_value = "homeassistant"
    )]
    pub mqtt_discovery_prefix: Option<String>,
    /// Home Assistant birth/status topic
    #[arg(
        long,
        env = "SIGEN_MQTT_HA_STATUS_TOPIC",
        default_value = "homeassistant/status"
    )]
    pub mqtt_ha_status_topic: Option<String>,
    /// Broker username
    #[arg(long, env = "SIGEN_MQTT_USERNAME", hide_env_values = true)]
    pub mqtt_username: Option<String>,
    /// Broker password (prefer SIGEN_MQTT_PASSWORD over a command-line value)
    #[arg(
        long,
        env = "SIGEN_MQTT_PASSWORD",
        hide_env_values = true,
        requires = "mqtt_username"
    )]
    pub mqtt_password: Option<String>,
    /// Enable verified TLS; use --mqtt-tls=false to override the environment
    #[arg(long, env = "SIGEN_MQTT_TLS", default_value_t = false, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub mqtt_tls: bool,
    /// PEM CA file for a private TLS broker
    #[arg(long, env = "SIGEN_MQTT_CA_FILE")]
    pub mqtt_ca_file: Option<PathBuf>,
    /// Enable Home Assistant hardware controls
    #[arg(long, env = "SIGEN_MQTT_CONTROLS", default_value_t = false, action = ArgAction::Set, num_args = 0..=1, default_missing_value = "true", require_equals = true)]
    pub mqtt_controls: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        // Isolate parser tests from the developer's environment without mutating it.
        let matches = Cli::command()
            .mut_args(|arg| arg.env(None::<&str>))
            .try_get_matches_from(args)?;
        Cli::from_arg_matches(&matches)
    }
    #[test]
    fn defaults_help_and_version() {
        Cli::command().debug_assert();
        let args = parse(&["sigen-server", "--address", "127.0.0.1:502"]).unwrap();
        assert_eq!(args.bind.to_string(), "127.0.0.1:3000");
        assert_eq!(args.unit_id, 1);
        assert_eq!(args.poll_seconds, 15);
        assert!(!args.dangerously_enable_modbus_writes);
        assert!(!args.mqtt.mqtt_tls);
        assert!(args.mqtt.mqtt_host.is_none());
        assert_eq!(
            parse(&["sigen-server", "--help"]).err().unwrap().kind(),
            clap::error::ErrorKind::DisplayHelp
        );
        assert_eq!(
            parse(&["sigen-server", "--version"]).err().unwrap().kind(),
            clap::error::ErrorKind::DisplayVersion
        );
    }
    #[test]
    fn ranges_and_boolean_overrides() {
        assert!(
            parse(&[
                "sigen-server",
                "--address",
                "127.0.0.1:502",
                "--dangerously-enable-modbus-writes"
            ])
            .unwrap()
            .dangerously_enable_modbus_writes
        );
        // The former opt-out must never provide a way to enable writes.
        assert!(
            parse(&[
                "sigen-server",
                "--address",
                "127.0.0.1:502",
                "--read-only=false"
            ])
            .is_err()
        );
        for (flag, value) in [
            ("--unit-id", "0"),
            ("--unit-id", "247"),
            ("--poll-seconds", "0"),
            ("--poll-seconds", "3601"),
            ("--read-function", "5"),
            ("--mqtt-port", "0"),
        ] {
            assert!(parse(&["sigen-server", "--address", "127.0.0.1:502", flag, value]).is_err());
        }
        let args = parse(&[
            "sigen-server",
            "--address",
            "127.0.0.1:502",
            "--mqtt-host",
            "localhost",
            "--mqtt-tls",
            "--mqtt-controls=false",
        ])
        .unwrap();
        assert!(args.mqtt.mqtt_tls);
        assert!(!args.mqtt.mqtt_controls);
        assert!(parse(&["sigen-server"]).is_err());
    }
}
