//! Read-only Octopus tariff snapshots from Home Assistant. Never export raw HA attributes.
use crate::hardware::SharedStatus;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub source: &'static str,
    pub available: bool,
    pub last_success_unix: Option<u64>,
    pub last_error: Option<String>,
    pub meters: Vec<Meter>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Meter {
    pub id: String,
    pub is_export: bool,
    pub tariff_code: Option<String>,
    pub current_rate_gbp_per_kwh: f64,
    pub standing_charge_gbp_per_day: Option<f64>,
    pub schedules: Vec<Schedule>,
    pub completed_day: Option<CompletedDay>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub consumption: Vec<Consumption>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub daily_costs: Vec<CompletedDay>,
    pub consumption_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rate {
    pub start: jiff::Timestamp,
    pub end: jiff::Timestamp,
    pub value_inc_vat: f64,
    #[serde(default)]
    pub is_capped: bool,
    #[serde(default)]
    pub is_intelligent_adjusted: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Schedule {
    pub day: String,
    pub tariff_code: Option<String>,
    pub rates: Vec<Rate>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Consumption {
    pub interval_start: jiff::Timestamp,
    pub interval_end: jiff::Timestamp,
    pub consumption: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct CompletedDay {
    pub start: jiff::Timestamp,
    pub consumption_kwh: Option<f64>,
    pub cost_gbp: f64,
    pub estimated: bool,
    pub standing_charge_gbp: Option<f64>,
}
#[derive(Deserialize)]
struct Entity {
    entity_id: String,
    state: String,
    attributes: serde_json::Value,
}

// Credentials deliberately have no Debug implementation.
pub struct Reader {
    client: reqwest::Client,
    endpoint: reqwest::Url,
}
impl Reader {
    pub fn new(
        url: Option<&str>,
        token_file: Option<&std::path::Path>,
    ) -> Result<Option<Self>, String> {
        let (url, file) = match (url, token_file) {
            (None, None) => return Ok(None),
            (Some(url), Some(file)) => (url, file),
            _ => return Err("Home Assistant URL and token file must both be configured".into()),
        };
        let mut endpoint = reqwest::Url::parse(url).map_err(|_| "Invalid Home Assistant URL")?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || endpoint.path() != "/"
        {
            return Err("Home Assistant URL must be an HTTPS origin without credentials, path, query or fragment".into());
        }
        endpoint.set_path("/api/states");
        let token =
            std::fs::read_to_string(file).map_err(|_| "Cannot read Home Assistant token file")?;
        if token.trim().is_empty() {
            return Err("Home Assistant token file is empty".into());
        }
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token.trim()))
                .map_err(|_| "Invalid Home Assistant token")?;
        authorization.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, authorization);
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "Cannot create Home Assistant HTTP client")?;
        Ok(Some(Self { client, endpoint }))
    }
    async fn read(&self) -> Result<Vec<Meter>, String> {
        let response = self
            .client
            .get(self.endpoint.clone())
            .send()
            .await
            .map_err(|_| "Home Assistant request failed")?;
        if !response.status().is_success() {
            return Err(format!(
                "Home Assistant returned HTTP {}",
                response.status().as_u16()
            ));
        }
        let entities = response
            .json::<Vec<Entity>>()
            .await
            .map_err(|_| "Invalid Home Assistant response")?;
        parse(&entities)
    }
}
fn number(entity: &Entity) -> Option<f64> {
    entity.state.parse::<f64>().ok().filter(|n| n.is_finite())
}
fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}
fn parse(entities: &[Entity]) -> Result<Vec<Meter>, String> {
    let mut meters = Vec::new();
    for entity in entities {
        let Some(id) = entity
            .entity_id
            .strip_prefix("sensor.octopus_energy_electricity_")
            .and_then(|s| s.strip_suffix("_current_rate"))
        else {
            continue;
        };
        let Some(rate) = number(entity) else {
            continue;
        };
        if entity.attributes["unit_of_measurement"] != "GBP/kWh" {
            continue;
        }
        let find = |domain: &str, suffix: &str| {
            entities.iter().find(|e| {
                e.entity_id == format!("{domain}.octopus_energy_electricity_{id}_{suffix}")
            })
        };
        let mut schedules = Vec::new();
        for day in ["previous", "current", "next"] {
            if let Some(e) = find("event", &format!("{day}_day_rates")) {
                if matches!(e.state.as_str(), "unknown" | "unavailable") {
                    continue;
                }
                let rates: Vec<Rate> = serde_json::from_value(e.attributes["rates"].clone())
                    .map_err(|_| "Invalid Octopus rate schedule")?;
                if rates
                    .iter()
                    .any(|r| !r.value_inc_vat.is_finite() || r.start >= r.end)
                    || rates.windows(2).any(|w| w[0].end > w[1].start)
                {
                    return Err("Invalid Octopus rate intervals".into());
                }
                schedules.push(Schedule {
                    day: day.into(),
                    tariff_code: text(&e.attributes, "tariff_code"),
                    rates,
                });
            }
        }
        let completed_day = find("sensor", "previous_accumulative_cost").and_then(|e| {
            Some(CompletedDay {
                start: text(&e.attributes, "last_reset")?.parse().ok()?,
                consumption_kwh: find("sensor", "previous_accumulative_consumption")
                    .filter(|c| {
                        c.attributes["last_reset"] == e.attributes["last_reset"]
                            && c.attributes["unit_of_measurement"] == "kWh"
                    })
                    .and_then(number),
                cost_gbp: if e.attributes["unit_of_measurement"] == "GBP" {
                    number(e)?
                } else {
                    return None;
                },
                estimated: false,
                standing_charge_gbp: e.attributes["standing_charge"].as_f64(),
            })
        });
        meters.push(Meter {
            id: id.into(),
            is_export: entity.attributes["is_export"].as_bool().unwrap_or(false),
            tariff_code: text(&entity.attributes, "tariff"),
            current_rate_gbp_per_kwh: rate,
            standing_charge_gbp_per_day: find("sensor", "current_standing_charge")
                .filter(|e| e.attributes["unit_of_measurement"] == "GBP")
                .and_then(number),
            schedules,
            completed_day,
            consumption: Vec::new(),
            daily_costs: Vec::new(),
            consumption_error: None,
        });
    }
    if meters.is_empty() {
        return Err("No available Octopus electricity rate entities found".into());
    }
    Ok(meters)
}
pub enum Source {
    HomeAssistant(Reader),
    Octopus(crate::octopus::Reader),
}
impl Source {
    pub fn from_args(args: &crate::cli::Cli) -> Result<Option<Self>, String> {
        let ha = Reader::new(args.ha_url.as_deref(), args.ha_token_file.as_deref())?;
        let octopus = crate::octopus::Reader::from_args(args)?;
        match (ha, octopus) {
            (Some(_), Some(_)) => Err("Choose either Home Assistant or Octopus tariffs".into()),
            (Some(reader), None) => Ok(Some(Self::HomeAssistant(reader))),
            (None, Some(reader)) => Ok(Some(Self::Octopus(reader))),
            (None, None) => Ok(None),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::HomeAssistant(_) => "home_assistant",
            Self::Octopus(reader) => {
                if reader.has_account() {
                    "octopus_account"
                } else {
                    "octopus"
                }
            }
        }
    }
    async fn read(&self) -> Result<Vec<Meter>, String> {
        match self {
            Self::HomeAssistant(reader) => reader.read().await,
            Self::Octopus(reader) => reader.read().await,
        }
    }
}
pub fn start(
    reader: Source,
    status: SharedStatus,
    mut stop: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    status.write().expect("status lock").energy = Some(Status {
        source: reader.name(),
        ..Status::default()
    });
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut success = None;
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break,
                _ = interval.tick() => {
                    let result = tokio::select! { biased; _ = stop.changed() => break, result = reader.read() => result };
                    let mut s = status.write().expect("status lock");
                    let energy = s.energy.as_mut().expect("energy status");
                    match result {
                        Ok(meters) => {
                            energy.meters = meters;
                            energy.last_success_unix = Some(crate::hardware::now());
                            energy.last_error = None;
                            success = Some(Instant::now());
                        }
                        Err(error) => { energy.last_error = Some(error); }
                    }
                    energy.available = success.is_some_and(|t| t.elapsed() < Duration::from_secs(180));
                }
            }
        }
        if let Some(energy) = &mut status.write().expect("status lock").energy {
            energy.available = false;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Vec<Entity> {
        serde_json::from_value(json!([
            {"entity_id":"sensor.octopus_energy_electricity_test_current_rate","state":"-0.01",
             "attributes":{"unit_of_measurement":"GBP/kWh","tariff":"TEST","is_export":false}},
            {"entity_id":"event.octopus_energy_electricity_test_current_day_rates","state":"2026-11-19T00:00:00Z",
             "attributes":{"rates":[{"start":"2026-11-19T00:00:00Z","end":"2026-11-19T00:30:00Z","value_inc_vat":-0.01,"is_intelligent_adjusted":true}]}},
            {"entity_id":"sensor.octopus_energy_electricity_test_previous_accumulative_cost","state":"3.06",
             "attributes":{"unit_of_measurement":"GBP","last_reset":"2026-11-18T00:00:00Z","standing_charge":0.45}},
            {"entity_id":"sensor.octopus_energy_electricity_test_previous_accumulative_consumption","state":"10.575",
             "attributes":{"unit_of_measurement":"kWh","last_reset":"2026-11-18T00:00:00Z"}}
        ])).unwrap()
    }
    #[test]
    fn preserves_negative_rates_adjustments_and_completed_day_cost() {
        let meters = parse(&fixture()).unwrap();
        let meter = &meters[0];
        assert_eq!(meter.current_rate_gbp_per_kwh, -0.01);
        assert!(meter.schedules[0].rates[0].is_intelligent_adjusted);
        let day = meter.completed_day.as_ref().unwrap();
        assert_eq!(day.cost_gbp, 3.06); // Includes standing charge; do not add it twice.
        assert_eq!(day.consumption_kwh, Some(10.575));
        assert_eq!(meter.standing_charge_gbp_per_day, None);
    }
    #[test]
    fn rejects_invalid_intervals_and_unavailable_current_rates() {
        let mut entities = fixture();
        entities[1].attributes["rates"][0]["end"] = json!("2026-11-18T00:00:00Z");
        assert!(parse(&entities).is_err());
        entities[0].state = "unavailable".into();
        assert!(parse(&entities).is_err());
    }
    #[test]
    fn does_not_pair_consumption_from_a_different_day() {
        let mut entities = fixture();
        entities[3].attributes["last_reset"] = json!("2026-11-17T00:00:00Z");
        assert_eq!(
            parse(&entities).unwrap()[0]
                .completed_day
                .as_ref()
                .unwrap()
                .consumption_kwh,
            None
        );
    }
    #[test]
    fn configuration_is_optional_but_requires_both_settings() {
        assert!(Reader::new(None, None).unwrap().is_none());
        assert!(Reader::new(Some("https://example.com"), None).is_err());
        assert!(
            Reader::new(
                Some("http://example.com"),
                Some(std::path::Path::new("/missing"))
            )
            .is_err()
        );
    }
}
