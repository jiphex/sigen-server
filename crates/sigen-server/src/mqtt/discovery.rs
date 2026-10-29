use super::config::Config;
use crate::model::Status;
use serde_json::{Value, json};

pub const MODES: [&str; 7] = [
    "PCS remote control",
    "Standby",
    "Maximum self-consumption",
    "Charge - grid first",
    "Charge - solar first",
    "Discharge - solar first",
    "Discharge - battery first",
];

/// One retained device discovery document; all components have stable unique IDs.
pub fn payload(config: &Config, status: &Status) -> Value {
    let mut components = serde_json::Map::new();
    let availability = json!([
        {"topic":format!("{}/availability",config.prefix)},
        {"topic":format!("{}/state",config.prefix),"value_template":"{{ 'online' if value_json.online else 'offline' }}"}
    ]);
    let sensor_fields = [
        ("grid_power_watts", "Grid power", "W", "power"),
        ("plant_power_watts", "Plant power", "W", "power"),
        ("pv_power_watts", "Solar power", "W", "power"),
        ("battery_power_watts", "Battery power", "W", "power"),
        ("battery_soc_percent", "Battery charge", "%", "battery"),
        ("battery_soh_percent", "Battery health", "%", ""),
        (
            "battery_temperature_celsius",
            "Battery temperature",
            "°C",
            "temperature",
        ),
        (
            "battery_min_temperature_celsius",
            "Battery minimum temperature",
            "°C",
            "temperature",
        ),
        (
            "battery_max_temperature_celsius",
            "Battery maximum temperature",
            "°C",
            "temperature",
        ),
        (
            "battery_cell_voltage_volts",
            "Battery cell voltage",
            "V",
            "voltage",
        ),
        (
            "battery_min_cell_voltage_volts",
            "Battery minimum cell voltage",
            "V",
            "voltage",
        ),
        (
            "battery_max_cell_voltage_volts",
            "Battery maximum cell voltage",
            "V",
            "voltage",
        ),
        (
            "battery_charge_limit_watts",
            "Battery charge limit",
            "W",
            "power",
        ),
        (
            "battery_discharge_limit_watts",
            "Battery discharge limit",
            "W",
            "power",
        ),
        (
            "battery_charge_available_wh",
            "Battery available charge energy",
            "Wh",
            "energy_storage",
        ),
        (
            "battery_discharge_available_wh",
            "Battery available discharge energy",
            "Wh",
            "energy_storage",
        ),
        ("inverter_power_watts", "Inverter power", "W", "power"),
        (
            "inverter_temperature_celsius",
            "Inverter temperature",
            "°C",
            "temperature",
        ),
        ("grid_frequency_hertz", "Grid frequency", "Hz", "frequency"),
    ];
    for (key, name, unit, class) in sensor_fields {
        let mut sensor = json!({"platform":"sensor","name":name,"unique_id":format!("{}_{}",config.id,key),
            "value_template":format!("{{{{ value_json.readings.{key} | default(none) }}}}"),
            "unit_of_measurement":unit,"state_class":"measurement","expire_after":90});
        if !class.is_empty() {
            sensor["device_class"] = json!(class);
        }
        components.insert(key.into(), sensor);
    }
    for (key, name) in [
        ("plant_state", "Plant state"),
        ("inverter_state", "Inverter state"),
        ("ems_work_mode", "EMS work mode"),
    ] {
        components.insert(key.into(), json!({"platform":"sensor","name":name,"unique_id":format!("{}_{}",config.id,key),
            "value_template":format!("{{{{ value_json.readings.{key} | default(none) }}}}"),"expire_after":90}));
    }
    for (key, name, class) in [
        ("grid_connected", "Grid connection", "connectivity"),
        (
            "grid_sensor_connected",
            "Grid meter connection",
            "connectivity",
        ),
    ] {
        components.insert(key.into(),json!({"platform":"binary_sensor","name":name,"unique_id":format!("{}_{}",config.id,key),
            "device_class":class,"value_template":format!("{{{{ 'ON' if value_json.readings is not none and value_json.readings.{key} else 'OFF' }}}}")}));
    }
    for index in 0..5 {
        let key = format!("alarm_{}", index + 1);
        components.insert(key.clone(),json!({"platform":"sensor","name":format!("Alarm group {}",index+1),"unique_id":format!("{}_{}",config.id,key),
            "entity_category":"diagnostic","value_template":format!("{{{{ value_json.readings.alarms[{index}] if value_json.readings is not none else none }}}}"),"expire_after":90}));
    }
    for id in crate::alerts::Id::ALL {
        let key = format!("alert_{}", id.key());
        let index = crate::alerts::Id::ALL
            .iter()
            .position(|item| *item == id)
            .unwrap();
        let name = status.alerts.items[index].name;
        components.insert(key.clone(),json!({"platform":"binary_sensor","name":name,"device_class":"problem",
            "unique_id":format!("{}_{}",config.id,key),
            "value_template":format!("{{{{ 'ON' if value_json.alerts.items[{index}].active else 'OFF' }}}}"),
            "availability":[{"topic":format!("{}/availability",config.prefix)},
                {"topic":format!("{}/state",config.prefix),"value_template":format!("{{{{ 'online' if value_json.alerts.items[{index}].known else 'offline' }}}}")}],
            "availability_mode":"all"}));
    }
    // Extra availability prevents stale controls after an uncertain write.
    let controls_availability = json!([
        {"topic":format!("{}/availability",config.prefix)},
        {"topic":format!("{}/state",config.prefix),"value_template":"{{ 'online' if value_json.online and value_json.controls is not none else 'offline' }}"}
    ]);
    if config.controls {
        components.insert("remote_ems".into(),json!({"platform":"switch","name":"Plant remote EMS","unique_id":format!("{}_remote_ems",config.id),
            "command_topic":format!("{}/command/remote_ems",config.prefix),
            "value_template":"{{ 'ON' if value_json.controls is not none and value_json.controls.remote_ems_enabled else 'OFF' }}",
            "payload_on":"ON","payload_off":"OFF","optimistic":false,"retain":false,"entity_category":"config",
            "availability":controls_availability,"availability_mode":"all"}));
        let mut mode_availability = controls_availability.clone();
        mode_availability.as_array_mut().unwrap().push(json!({"topic":format!("{}/state",config.prefix),
            "value_template":"{{ 'online' if value_json.controls is not none and value_json.controls.remote_ems_enabled else 'offline' }}"}));
        components.insert("ems_mode".into(),json!({"platform":"select","name":"Plant remote EMS mode","unique_id":format!("{}_ems_mode",config.id),
            "command_topic":format!("{}/command/ems_mode",config.prefix),"options":MODES,"optimistic":false,"retain":false,"entity_category":"config",
            "value_template":format!("{{{{ {}[value_json.controls.remote_ems_mode] if value_json.controls is not none and value_json.controls.remote_ems_mode < 7 else none }}}}",serde_json::to_string(&MODES).unwrap()),
            "availability":mode_availability,"availability_mode":"all"}));
        components.insert("backup_soc".into(),json!({"platform":"number","name":"Plant backup reserve","unique_id":format!("{}_backup_soc",config.id),
            "command_topic":format!("{}/command/backup_soc",config.prefix),"value_template":"{{ value_json.controls.backup_soc_percent if value_json.controls is not none else none }}",
            "unit_of_measurement":"%","min":0,"max":100,"step":0.1,"mode":"box","optimistic":false,"retain":false,"entity_category":"config",
            "availability":controls_availability,"availability_mode":"all"}));
    }
    // When controls are disabled, the complete replacement document omits them.
    // The publisher sends explicit removals before this replacement document.
    let mut device = json!({"identifiers":[config.id],"name":format!("Sigen {}",config.id),"manufacturer":"Sigenergy"});
    if let Some(info) = &status.device {
        device["model"] = json!(info.model);
        device["serial_number"] = json!(info.serial_number);
        device["sw_version"] = json!(info.firmware_version);
    }
    json!({"device":device,"origin":{"name":"sigen-server","sw_version":env!("CARGO_PKG_VERSION")},
        "state_topic":format!("{}/state",config.prefix),"qos":0,
        "availability":availability,"availability_mode":"all","components":components})
}
