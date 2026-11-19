//! Compact cost exports; consumption histories and account credentials stay out of MQTT.
use super::{Config, fresh};
use crate::model::Status;
use serde_json::{Value, json};

fn key(id: &str) -> String {
    // Lossless encoding avoids collisions between punctuation in tariff/meter IDs.
    format!(
        "energy_{}",
        id.bytes().map(|b| format!("{b:02x}")).collect::<String>()
    )
}
pub fn payload(config: &Config, status: &Status) -> Value {
    let Some(energy) = &status.energy else {
        return Value::Null;
    };
    let now = crate::hardware::now();
    let available = energy.available
        && energy
            .last_success_unix
            .is_some_and(|t| now.saturating_sub(t) < 180);
    let timestamp = jiff::Timestamp::from_second(now as i64).expect("current timestamp");
    let mut meters = serde_json::Map::new();
    for meter in &energy.meters {
        let rate = if available {
            meter
                .schedules
                .iter()
                .flat_map(|s| &s.rates)
                .find(|r| r.start <= timestamp && timestamp < r.end)
        } else {
            None
        };
        let unique = energy
            .meters
            .iter()
            .filter(|m| m.is_export == meter.is_export)
            .count()
            == 1;
        let hourly = rate.and_then(|r| {
            let readings = status.readings.as_ref()?;
            let power = readings.grid_power_watts?;
            (fresh(config, status)
                && unique
                && readings.grid_sensor_connected
                && power.is_finite()
                && if meter.is_export {
                    power < 0.0
                } else {
                    power >= 0.0
                })
            .then_some(power.abs() / 1000.0 * r.value_inc_vat)
        });
        meters.insert(key(&meter.id),json!({
            "meter_id":meter.id,"is_export":meter.is_export,"tariff_code":meter.tariff_code,
            "current_rate_gbp_per_kwh":rate.map(|r|r.value_inc_vat),
            "standing_charge_gbp_per_day":if available {meter.standing_charge_gbp_per_day} else {None},
            "live_gbp_per_hour":hourly,
            "is_intelligent_adjusted":rate.map(|r|r.is_intelligent_adjusted),
            "completed_day":if available {meter.completed_day.as_ref()} else {None},
        }));
    }
    json!({"source":energy.source,"available":available,"last_success_unix":energy.last_success_unix,"meters":meters})
}
pub fn discovery(
    config: &Config,
    status: &Status,
    components: &mut serde_json::Map<String, Value>,
) {
    let Some(energy) = &status.energy else {
        return;
    };
    for meter in &energy.meters {
        let key = key(&meter.id);
        let path = format!("value_json.energy.meters.{key}");
        let direction = if meter.is_export { "Export" } else { "Import" };
        for (suffix, name, unit, field) in [
            ("rate", "rate", "GBP/kWh", "current_rate_gbp_per_kwh"),
            (
                "standing_charge",
                "standing charge",
                "GBP/day",
                "standing_charge_gbp_per_day",
            ),
            (
                "live_cost",
                if meter.is_export {
                    "live revenue"
                } else {
                    "live cost"
                },
                "GBP/h",
                "live_gbp_per_hour",
            ),
            (
                "completed_cost",
                if meter.is_export {
                    "completed-day earnings"
                } else {
                    "completed-day cost"
                },
                "GBP",
                "completed_day.cost_gbp",
            ),
        ] {
            let field_available = if suffix == "completed_cost" {
                format!("{path}.completed_day is not none")
            } else {
                format!("{path}.{field} is not none")
            };
            let mut sensor = json!({"platform":"sensor","name":format!("{direction} {name} ({})",meter.id),
                "unique_id":format!("{}_{key}_{suffix}",config.id),"unit_of_measurement":unit,"expire_after":90,
                "value_template":format!("{{{{ {path}.{field} if value_json.energy is not none and '{key}' in value_json.energy.meters and {field_available} else none }}}}"),
                "availability":[{"topic":format!("{}/availability",config.prefix)},
                    {"topic":format!("{}/state",config.prefix),"value_template":format!("{{{{ 'online' if value_json.energy is not none and value_json.energy.available and '{key}' in value_json.energy.meters and {field_available} else 'offline' }}}}")}],
                "availability_mode":"all"});
            if suffix == "completed_cost" {
                sensor["device_class"] = json!("monetary");
                // No total/total_increasing: this is a dated daily value, not a lifetime counter.
                sensor["json_attributes_topic"] = json!(format!("{}/state", config.prefix));
                sensor["json_attributes_template"] = json!(format!(
                    "{{{{ {path}.completed_day | tojson if value_json.energy is not none and '{key}' in value_json.energy.meters and {field_available} else '{{}}' }}}}"
                ));
            } else {
                sensor["state_class"] = json!("measurement");
            }
            components.insert(format!("{key}_{suffix}"), sensor);
        }
    }
}
