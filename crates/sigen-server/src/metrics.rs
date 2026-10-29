use crate::{hardware::now, model::Status};
use std::fmt::Write;

pub fn render(s: &Status) -> String {
    let mut out = String::new();
    out.push_str("# HELP sigen_alert_enabled Whether an alert is enabled.\n# TYPE sigen_alert_enabled gauge\n# HELP sigen_alert_active Whether an enabled alert is active.\n# TYPE sigen_alert_active gauge\n# HELP sigen_alert_known Whether the alert condition can currently be evaluated.\n# TYPE sigen_alert_known gauge\n");
    for alert in &s.alerts.items {
        for (metric, value) in [
            ("enabled", alert.enabled),
            ("active", alert.active),
            ("known", alert.known),
        ] {
            writeln!(
                out,
                "sigen_alert_{metric}{{alert=\"{}\"}} {}",
                alert.id.key(),
                u8::from(value)
            )
            .unwrap();
        }
    }
    if let Some(p) = &s.pushover {
        metric(
            &mut out,
            "sigen_pushover_sent_total",
            "Notifications accepted by Pushover.",
            "counter",
            p.sent_total,
        );
        metric(
            &mut out,
            "sigen_pushover_failed_total",
            "Dropped or permanently failed Pushover notifications.",
            "counter",
            p.failed_total,
        );
        if let Some(time) = p.last_sent_unix {
            metric(
                &mut out,
                "sigen_pushover_last_success_timestamp_seconds",
                "Last notification accepted by Pushover.",
                "gauge",
                time,
            );
        }
    }
    if let Some(mqtt) = &s.mqtt {
        metric(
            &mut out,
            "sigen_mqtt_connected",
            "Whether the server is connected to the MQTT broker.",
            "gauge",
            u8::from(mqtt.connected),
        );
    }
    metric(
        &mut out,
        "sigen_up",
        "Whether hardware readings are online and fresh.",
        "gauge",
        u8::from(s.fresh()),
    );
    metric(
        &mut out,
        "sigen_poll_errors_total",
        "Failed hardware polls since server start.",
        "counter",
        s.poll_errors,
    );
    metric(
        &mut out,
        "sigen_control_errors_total",
        "Failed hardware controls since server start.",
        "counter",
        s.control_errors,
    );
    if let Some(timestamp) = s.last_success_unix {
        metric(
            &mut out,
            "sigen_last_success_timestamp_seconds",
            "Unix time of last successful poll.",
            "gauge",
            timestamp,
        );
        metric(
            &mut out,
            "sigen_sample_age_seconds",
            "Age of the last successful poll.",
            "gauge",
            now().saturating_sub(timestamp),
        );
    }
    // Omit stale hardware values on failure rather than making them appear current.
    if s.fresh() {
        if let Some(r) = &s.readings {
            macro_rules! gauge {
                ($field:ident, $help:literal) => {
                    metric(
                        &mut out,
                        concat!("sigen_", stringify!($field)),
                        $help,
                        "gauge",
                        r.$field,
                    );
                };
            }
            gauge!(
                grid_power_watts,
                "Grid power: positive import, negative export."
            );
            gauge!(plant_power_watts, "Plant active power.");
            gauge!(pv_power_watts, "Plant solar generation power.");
            gauge!(
                battery_power_watts,
                "Battery power: positive charge, negative discharge."
            );
            gauge!(battery_soc_percent, "Battery state of charge.");
            gauge!(battery_soh_percent, "Battery state of health.");
            gauge!(
                battery_temperature_celsius,
                "Average battery cell temperature."
            );
            gauge!(
                battery_min_temperature_celsius,
                "Minimum battery cluster temperature."
            );
            gauge!(
                battery_max_temperature_celsius,
                "Maximum battery cluster temperature."
            );
            gauge!(battery_cell_voltage_volts, "Average battery cell voltage.");
            gauge!(
                battery_min_cell_voltage_volts,
                "Minimum battery cell voltage."
            );
            gauge!(
                battery_max_cell_voltage_volts,
                "Maximum battery cell voltage."
            );
            gauge!(
                battery_charge_limit_watts,
                "Available maximum battery charging power."
            );
            gauge!(
                battery_discharge_limit_watts,
                "Available maximum battery discharging power."
            );
            gauge!(
                battery_charge_available_wh,
                "Available battery charging energy."
            );
            gauge!(
                battery_discharge_available_wh,
                "Available battery discharging energy."
            );
            gauge!(inverter_power_watts, "Inverter active power.");
            gauge!(
                inverter_temperature_celsius,
                "Inverter internal temperature."
            );
            gauge!(grid_frequency_hertz, "Grid frequency.");
            gauge!(ems_work_mode, "Raw plant EMS work mode.");
            metric(
                &mut out,
                "sigen_grid_connected",
                "Whether the plant is on grid.",
                "gauge",
                u8::from(r.grid_connected),
            );
            metric(
                &mut out,
                "sigen_grid_sensor_connected",
                "Whether the grid meter is connected.",
                "gauge",
                u8::from(r.grid_sensor_connected),
            );
            out.push_str("# HELP sigen_alarm_bits Raw alarm bitfields by group (1 through 5).\n# TYPE sigen_alarm_bits gauge\n");
            for (index, bits) in r.alarms.iter().enumerate() {
                writeln!(out, "sigen_alarm_bits{{group=\"{}\"}} {bits}", index + 1).unwrap();
            }
        }
        if let Some(c) = &s.controls {
            metric(
                &mut out,
                "sigen_remote_ems_enabled",
                "Remote EMS enabled readback.",
                "gauge",
                u8::from(c.remote_ems_enabled),
            );
            metric(
                &mut out,
                "sigen_remote_ems_mode",
                "Remote EMS control mode readback.",
                "gauge",
                c.remote_ems_mode,
            );
            metric(
                &mut out,
                "sigen_backup_soc_percent",
                "Battery backup reserve readback.",
                "gauge",
                c.backup_soc_percent,
            );
        }
    }
    out
}
fn metric(out: &mut String, name: &str, help: &str, kind: &str, value: impl std::fmt::Display) {
    writeln!(
        out,
        "# HELP {name} {help}\n# TYPE {name} {kind}\n{name} {value}"
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_metrics_expose_health_and_age_without_fabricating_measurements() {
        let mut status = Status::new("127.0.0.1:502".into(), 1);
        status.last_success_unix = Some(1);
        status.poll_errors = 3;
        let output = render(&status);
        assert!(output.contains("sigen_up 0\n"));
        assert!(
            output.contains("# TYPE sigen_poll_errors_total counter\nsigen_poll_errors_total 3\n")
        );
        assert!(output.contains("sigen_last_success_timestamp_seconds 1\n"));
        assert!(output.contains("sigen_sample_age_seconds "));
        assert!(!output.contains("sigen_pv_power_watts"));
        assert!(output.ends_with('\n'));
    }
}
