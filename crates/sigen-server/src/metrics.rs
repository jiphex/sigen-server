use crate::{hardware::now, model::Status};
use std::fmt::Write;

/// Export health even when offline, but omit stale measurements instead of emitting zeros.
pub fn render(s: &Status) -> String {
    let mut out = String::new();
    if let Some(journal) = &s.journal {
        let health = journal.health();
        for (name, help, kind, value) in [
            (
                "sigen_journal_written_total",
                "Event journal records committed this process.",
                "counter",
                health.written_total,
            ),
            (
                "sigen_journal_dropped_total",
                "Event journal records lost this process.",
                "counter",
                health.dropped_total,
            ),
            (
                "sigen_journal_errors",
                "Whether this process has encountered event journal data loss.",
                "gauge",
                u64::from(health.last_error.is_some()),
            ),
        ] {
            metric(&mut out, name, help, kind, value);
        }
    }
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
        optional_metrics(&mut out, s);
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
            if let Some(power) = r.grid_power_watts.filter(|_| r.grid_sensor_connected) {
                metric(
                    &mut out,
                    "sigen_grid_power_watts",
                    "Grid power: positive import, negative export.",
                    "gauge",
                    power,
                );
            }
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
            out.push_str("# HELP sigen_plant_alarm_bits Aggregate plant alarm bitfields.\n# TYPE sigen_plant_alarm_bits gauge\n");
            for (index, bits) in r.plant_alarms.iter().enumerate() {
                writeln!(
                    out,
                    "sigen_plant_alarm_bits{{group=\"{}\"}} {bits}",
                    index + 1
                )
                .unwrap();
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

fn optional_metrics(out: &mut String, s: &Status) {
    let d = s.details.for_export(
        now(),
        s.device_day,
        s.readings.as_ref().is_some_and(|r| r.grid_sensor_connected),
    );
    for (sample, name, help) in [
        (
            &d.pv_daily_kwh,
            "sigen_pv_daily_energy_kwh",
            "Inverter solar generation for the device-local day.",
        ),
        (
            &d.load_daily_kwh,
            "sigen_load_daily_energy_kwh",
            "Plant load consumption for the device-local day.",
        ),
    ] {
        if let Some(value) = sample.value {
            metric(out, name, help, "gauge", value);
        }
    }
    if let Some(b) = d.battery_daily.value {
        metric(
            out,
            "sigen_battery_daily_charge_energy_kwh",
            "Inverter battery charging energy for the device-local day.",
            "gauge",
            b.charge_kwh,
        );
        metric(
            out,
            "sigen_battery_daily_discharge_energy_kwh",
            "Inverter battery discharging energy for the device-local day.",
            "gauge",
            b.discharge_kwh,
        );
    }
    for (sample, name, help) in [
        (
            &d.grid_import,
            "sigen_grid_import_energy_kwh_total",
            "Plant imported grid energy since the new statistics interface was initialized.",
        ),
        (
            &d.grid_export,
            "sigen_grid_export_energy_kwh_total",
            "Plant exported grid energy since the new statistics interface was initialized.",
        ),
    ] {
        if let Some(value) = sample.value.as_ref().and_then(|s| s.parse::<u64>().ok()) {
            metric(
                out,
                name,
                help,
                "counter",
                format!("{}.{:02}", value / 100, value % 100),
            );
        }
    }
    if let Some(e) = d.electrical.value {
        metric(
            out,
            "sigen_inverter_output_type",
            "Raw inverter output wiring type.",
            "gauge",
            e.output_type,
        );
        metric(
            out,
            "sigen_inverter_power_factor",
            "Inverter power factor.",
            "gauge",
            e.power_factor,
        );
        phases(
            out,
            "sigen_inverter_phase_voltage_volts",
            "Inverter phase voltage where wiring defines a phase-neutral measurement.",
            e.phase_voltage_volts,
            std::array::from_fn(|i| e.output_type == 2 || (e.output_type == 0 && i == 0)),
        );
        phases(
            out,
            "sigen_inverter_phase_current_amps",
            "Inverter phase current.",
            e.phase_current_amps,
            std::array::from_fn(|i| {
                matches!(e.output_type, 1 | 2) || (e.output_type == 0 && i == 0)
            }),
        );
    }
    if let Some(p) = d.grid_phases.value {
        phases(
            out,
            "sigen_grid_phase_power_watts",
            "Plant grid sensor phase power: positive import, negative export.",
            p.active_watts,
            [true; 3],
        );
    }
}

fn phases(out: &mut String, name: &str, help: &str, values: [f64; 3], valid: [bool; 3]) {
    if !valid.iter().any(|v| *v) {
        return;
    }
    writeln!(out, "# HELP {name} {help}\n# TYPE {name} gauge").unwrap();
    for ((phase, value), valid) in ["a", "b", "c"].into_iter().zip(values).zip(valid) {
        if valid {
            writeln!(out, "{name}{{phase=\"{phase}\"}} {value}").unwrap();
        }
    }
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

#[cfg(test)]
mod optional_tests {
    use super::*;
    fn status() -> Status {
        let mut s = Status::new("127.0.0.1:502".into(), 1);
        s.online = true;
        s.control_valid_until =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
        s.device_day = Some(20000);
        s.details = crate::telemetry::test_details(now(), 20000);
        s
    }
    #[test]
    fn optional_metrics_use_energy_units_types_precision_and_phase_labels() {
        let mut s = status();
        let out = render(&s);
        assert!(
            out.contains(
                "# TYPE sigen_pv_daily_energy_kwh gauge\nsigen_pv_daily_energy_kwh 14.82\n"
            )
        );
        assert!(out.contains("sigen_load_daily_energy_kwh 11.37\n"));
        assert!(out.contains("sigen_battery_daily_charge_energy_kwh 6.24\n"));
        assert!(out.contains("sigen_battery_daily_discharge_energy_kwh 4.18\n"));
        assert!(out.contains("# TYPE sigen_grid_import_energy_kwh_total counter\nsigen_grid_import_energy_kwh_total 184467440737095516.15\n"));
        assert!(out.contains("sigen_grid_export_energy_kwh_total 0.00\n"));
        assert!(out.contains("sigen_inverter_phase_voltage_volts{phase=\"c\"} 232\n"));
        assert!(out.contains("sigen_inverter_phase_current_amps{phase=\"a\"} -1\n"));
        assert!(out.contains("sigen_inverter_power_factor 0.999\n"));
        // Without a connected grid sensor, plant phase powers are not exported.
        assert!(!out.contains("sigen_grid_phase_power_watts"));
        s.details.electrical.value.as_mut().unwrap().output_type = 0;
        let out = render(&s);
        assert!(out.contains("sigen_inverter_phase_voltage_volts{phase=\"a\"}"));
        assert!(!out.contains("sigen_inverter_phase_voltage_volts{phase=\"b\"}"));
        assert!(!out.contains("sigen_inverter_phase_current_amps{phase=\"c\"}"));
    }
    #[test]
    fn optional_metrics_omit_expired_failed_unsupported_previous_day_and_offline_samples() {
        let mut s = status();
        s.details.pv_daily_kwh.updated_unix = Some(now() - 601);
        s.details.load_daily_kwh.device_day = Some(19999);
        s.details.battery_daily.last_error = Some("failed".into());
        s.details.grid_import.unsupported = true;
        s.details.electrical.updated_unix = Some(now() - 121);
        let out = render(&s);
        for name in [
            "sigen_pv_daily_energy_kwh",
            "sigen_load_daily_energy_kwh",
            "sigen_battery_daily_charge_energy_kwh",
            "sigen_grid_import_energy_kwh_total",
            "sigen_inverter_power_factor",
        ] {
            assert!(!out.contains(name), "{name}");
        }
        assert!(out.contains("sigen_grid_export_energy_kwh_total 0.00\n"));
        s.online = false;
        assert!(!render(&s).contains("sigen_grid_export_energy_kwh_total"));
    }
}
