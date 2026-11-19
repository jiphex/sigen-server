//! Octopus tariff and account API. Prices arrive in pence, unlike Home Assistant's GBP.
mod account;
use crate::energy::{CompletedDay, Consumption, Meter, Rate, Schedule};
use account::AccountAccess;
use jiff::{Timestamp, ToSpan, tz::TimeZone};
use serde::Deserialize;
use std::time::Duration;

pub struct Reader {
    client: reqwest::Client,
    tariffs: Vec<Tariff>,
    account: Option<AccountAccess>,
    origin: String,
}
#[derive(Clone)]
struct Tariff {
    code: String,
    product: String,
    export: bool,
}
impl Tariff {
    fn new(code: &str, export: bool) -> Result<Self, String> {
        let product = code
            .strip_prefix("E-1R-")
            .and_then(|s| s.rsplit_once('-'))
            .filter(|(product, region)| {
                !product.is_empty()
                    && region.len() == 1
                    && region.as_bytes()[0].is_ascii_uppercase()
            })
            .map(|(product, _)| product)
            .filter(|s| {
                s.bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
            })
            .ok_or("Expected a single-register electricity tariff code: E-1R-PRODUCT-REGION")?;
        Ok(Self {
            code: code.into(),
            product: product.into(),
            export,
        })
    }
}
#[derive(Deserialize)]
struct Page<T> {
    results: Vec<T>,
    next: Option<String>,
}
#[derive(Clone, Deserialize)]
struct Price {
    value_inc_vat: f64,
    valid_from: Timestamp,
    valid_to: Option<Timestamp>,
    payment_method: Option<String>,
}
fn bounds(now: Timestamp) -> Result<Vec<Timestamp>, String> {
    let tz = TimeZone::get("Europe/London").map_err(|_| "London timezone unavailable")?;
    let date = now.to_zoned(tz.clone()).date();
    (-1..=2)
        .map(|offset| {
            date.checked_add(offset.days())
                .and_then(|d| d.to_zoned(tz.clone()))
                .map(|d| d.timestamp())
                .map_err(|_| "Invalid tariff date range".into())
        })
        .collect()
}
impl Reader {
    pub fn has_account(&self) -> bool {
        self.account.is_some()
    }
    pub fn new(import: Option<&str>, export: Option<&str>) -> Result<Option<Self>, String> {
        let mut tariffs = Vec::new();
        for (code, export) in [(import, false), (export, true)] {
            if let Some(code) = code {
                tariffs.push(Tariff::new(code, export)?);
            }
        }
        if tariffs.is_empty() {
            return Ok(None);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "Cannot create Octopus HTTP client")?;
        Ok(Some(Self {
            client,
            tariffs,
            account: None,
            origin: "https://api.octopus.energy".into(),
        }))
    }
    async fn prices(
        &self,
        tariff: &Tariff,
        kind: &str,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Price>, String> {
        let url = format!(
            "{}/v1/products/{}/electricity-tariffs/{}/{kind}/",
            self.origin, tariff.product, tariff.code
        );
        let mut prices = Vec::new();
        for page in 1..=5 {
            // Construct every page against the fixed origin; never follow API-supplied URLs.
            let response = self
                .client
                .get(&url)
                .query(&[
                    ("period_from", from.to_string()),
                    ("period_to", to.to_string()),
                    ("page_size", "1500".into()),
                    ("page", page.to_string()),
                ])
                .send()
                .await
                .map_err(|_| "Octopus tariff request failed")?;
            if !response.status().is_success() {
                return Err(format!(
                    "Octopus returned HTTP {} for {kind}",
                    response.status().as_u16()
                ));
            }
            let result = response
                .json::<Page<Price>>()
                .await
                .map_err(|_| "Invalid Octopus tariff response")?;
            prices.extend(result.results.into_iter().filter(|p| {
                p.payment_method
                    .as_deref()
                    .is_none_or(|m| m == "DIRECT_DEBIT")
            }));
            if result.next.is_none() {
                return Ok(prices);
            }
        }
        Err("Octopus tariff pagination limit exceeded".into())
    }
    pub async fn read(&self) -> Result<Vec<Meter>, String> {
        if self.account.is_some() {
            return self.read_account().await;
        }
        let now = Timestamp::now();
        let days = bounds(now)?;
        let mut meters = Vec::new();
        for tariff in &self.tariffs {
            let (rates, standing) = tokio::try_join!(
                self.prices(tariff, "standard-unit-rates", days[0], days[3]),
                self.prices(tariff, "standing-charges", days[1], days[2]),
            )?;
            meters.push(meter(tariff, rates, standing, &days, now)?);
        }
        Ok(meters)
    }
}
fn meter(
    tariff: &Tariff,
    prices: Vec<Price>,
    standing: Vec<Price>,
    days: &[Timestamp],
    now: Timestamp,
) -> Result<Meter, String> {
    let mut schedules = Vec::new();
    for (index, day) in ["previous", "current", "next"].iter().enumerate() {
        let start = days[index];
        let end = days[index + 1];
        let mut rates = Vec::new();
        for price in &prices {
            if !price.value_inc_vat.is_finite()
                || price.valid_to.is_some_and(|end| end <= price.valid_from)
            {
                return Err("Invalid Octopus rate interval".into());
            }
            let from = price.valid_from.max(start);
            let to = price.valid_to.unwrap_or(end).min(end);
            if from < to {
                rates.push(Rate {
                    start: from,
                    end: to,
                    value_inc_vat: price.value_inc_vat / 100.0,
                    is_capped: false,
                    is_intelligent_adjusted: false,
                });
            }
        }
        rates.sort_by_key(|r| r.start);
        if rates.windows(2).any(|w| w[0].end > w[1].start) {
            return Err("Overlapping Octopus tariff rates".into());
        }
        // Future Agile rates may not have been published yet; retain an empty schedule.
        schedules.push(Schedule {
            day: (*day).into(),
            tariff_code: Some(tariff.code.clone()),
            rates,
        });
    }
    let current = schedules
        .iter()
        .flat_map(|s| &s.rates)
        .find(|r| r.start <= now && now < r.end)
        .ok_or("Octopus has no rate covering the current time")?
        .value_inc_vat;
    let active: Vec<_> = standing
        .iter()
        .filter(|p| p.valid_from <= now && p.valid_to.is_none_or(|end| now < end))
        .collect();
    if active.len() > 1
        || active
            .iter()
            .any(|p| !p.value_inc_vat.is_finite() || p.value_inc_vat < 0.0)
    {
        return Err("Invalid Octopus standing charge".into());
    }
    Ok(Meter {
        id: tariff.code.clone(),
        is_export: tariff.export,
        tariff_code: Some(tariff.code.clone()),
        current_rate_gbp_per_kwh: current,
        standing_charge_gbp_per_day: active.first().map(|p| p.value_inc_vat / 100.0),
        schedules,
        completed_day: None,
        consumption: Vec::new(),
        daily_costs: Vec::new(),
        consumption_error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn price(value: f64, start: &str, end: Option<&str>) -> Price {
        Price {
            value_inc_vat: value,
            valid_from: start.parse().unwrap(),
            valid_to: end.map(|s| s.parse().unwrap()),
            payment_method: None,
        }
    }
    #[test]
    fn cli_accepts_either_source_but_rejects_mixed_configuration() {
        use clap::Parser;
        let base = ["sigen-server", "--address", "127.0.0.1:502"];
        let mut direct = base.to_vec();
        direct.extend([
            "--octopus-import-tariff",
            "E-1R-TEST-J",
            "--octopus-export-tariff",
            "E-1R-EXPORT-J",
        ]);
        assert!(crate::cli::Cli::try_parse_from(&direct).is_ok());
        let mut account = base.to_vec();
        account.extend([
            "--octopus-account",
            "A-TEST",
            "--octopus-api-key-file",
            "/private/tmp/key",
        ]);
        assert!(crate::cli::Cli::try_parse_from(&account).is_ok());
        let mut missing = base.to_vec();
        missing.extend(["--octopus-account", "A-TEST"]);
        assert!(crate::cli::Cli::try_parse_from(&missing).is_err());
        account.extend(["--octopus-import-tariff", "E-1R-TEST-J"]);
        assert!(crate::cli::Cli::try_parse_from(&account).is_err());
        direct.extend([
            "--ha-url",
            "https://example.com",
            "--ha-token-file",
            "/private/tmp/token",
        ]);
        assert!(crate::cli::Cli::try_parse_from(&direct).is_err());
    }
    #[test]
    fn validates_tariff_codes_and_rejects_dual_register_or_path_injection() {
        let tariff = Tariff::new("E-1R-AGILE-24-10-01-J", false).unwrap();
        assert_eq!(tariff.product, "AGILE-24-10-01");
        for code in [
            "E-2R-GO-J",
            "E-1R-../test-J",
            "E-1R-TEST-ZZ",
            "G-1R-TEST-J",
            "E-1R--J",
        ] {
            assert!(Tariff::new(code, false).is_err(), "{code}");
        }
    }
    #[test]
    fn converts_pence_and_clips_open_ended_rates_to_london_days() {
        let now: Timestamp = "2026-11-19T12:00:00Z".parse().unwrap();
        let days = bounds(now).unwrap();
        let tariff = Tariff::new("E-1R-TEST-J", false).unwrap();
        let meter = meter(
            &tariff,
            vec![price(24.6348, "2026-03-12T00:00:00Z", None)],
            vec![price(45.0678, "2026-03-12T00:00:00Z", None)],
            &days,
            now,
        )
        .unwrap();
        assert!((meter.current_rate_gbp_per_kwh - 0.246348).abs() < 1e-10);
        assert!((meter.standing_charge_gbp_per_day.unwrap() - 0.450678).abs() < 1e-10);
        assert!(meter.completed_day.is_none());
        assert_eq!(meter.schedules[1].rates[0].start, days[1]);
        assert_eq!(meter.schedules[1].rates[0].end, days[2]);
    }
    #[test]
    fn dst_boundaries_use_23_and_25_hour_days() {
        for (date, hours) in [("2026-03-29T12:00:00Z", 23), ("2026-10-25T12:00:00Z", 25)] {
            let days = bounds(date.parse().unwrap()).unwrap();
            assert_eq!(days[2].as_second() - days[1].as_second(), hours * 3600);
        }
    }
    #[test]
    fn sorts_negative_rates_and_accepts_unpublished_tomorrow() {
        let now: Timestamp = "2026-11-19T12:00:00Z".parse().unwrap();
        let days = bounds(now).unwrap();
        let tariff = Tariff::new("E-1R-TEST-J", true).unwrap();
        let meter = meter(
            &tariff,
            vec![
                price(-2.0, "2026-11-19T12:00:00Z", Some("2026-11-20T00:00:00Z")),
                price(10.0, "2026-11-19T00:00:00Z", Some("2026-11-19T12:00:00Z")),
            ],
            vec![],
            &days,
            now,
        )
        .unwrap();
        assert_eq!(meter.current_rate_gbp_per_kwh, -0.02);
        assert!(meter.is_export);
        assert!(meter.schedules[2].rates.is_empty());
        assert!(meter.standing_charge_gbp_per_day.is_none());
    }
    #[test]
    fn rejects_overlaps_and_absent_current_rates() {
        let now: Timestamp = "2026-11-19T12:00:00Z".parse().unwrap();
        let days = bounds(now).unwrap();
        let tariff = Tariff::new("E-1R-TEST-J", false).unwrap();
        let rates = vec![
            price(20.0, "2026-11-19T00:00:00Z", None),
            price(30.0, "2026-11-19T12:00:00Z", None),
        ];
        assert!(meter(&tariff, rates, vec![], &days, now).is_err());
        assert!(meter(&tariff, vec![], vec![], &days, now).is_err());
    }
}
