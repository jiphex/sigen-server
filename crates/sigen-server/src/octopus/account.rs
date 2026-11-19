//! Authenticated account discovery and consumption, independent of Home Assistant.
use super::*;
use serde::de::DeserializeOwned;

// Deliberately no Debug: this holds credentials and an account identifier.
pub(super) struct AccountAccess {
    number: String,
    key: String,
}
#[derive(Deserialize)]
struct Account {
    properties: Vec<Property>,
}
#[derive(Deserialize)]
struct Property {
    moved_in_at: Option<Timestamp>,
    moved_out_at: Option<Timestamp>,
    electricity_meter_points: Vec<MeterPoint>,
}
#[derive(Deserialize)]
struct MeterPoint {
    mpan: String,
    #[serde(default)]
    is_export: bool,
    meters: Vec<AccountMeter>,
    agreements: Vec<Agreement>,
}
#[derive(Deserialize)]
struct AccountMeter {
    serial_number: String,
}
#[derive(Deserialize)]
struct Agreement {
    tariff_code: String,
    valid_from: Timestamp,
    valid_to: Option<Timestamp>,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
fn active(from: Timestamp, to: Option<Timestamp>, now: Timestamp) -> bool {
    from <= now && to.is_none_or(|end| now < end)
}
impl Reader {
    pub fn from_args(args: &crate::cli::Cli) -> Result<Option<Self>, String> {
        match (
            args.octopus_account.as_deref(),
            args.octopus_api_key_file.as_deref(),
        ) {
            (None, None) => Self::new(
                args.octopus_import_tariff.as_deref(),
                args.octopus_export_tariff.as_deref(),
            ),
            (Some(number), Some(path)) => {
                if !number.starts_with("A-") || number.len() <= 2 || !identifier(number) {
                    return Err("Invalid Octopus account number; expected A-...".into());
                }
                if args.ha_url.is_some()
                    || args.ha_token_file.is_some()
                    || args.octopus_import_tariff.is_some()
                    || args.octopus_export_tariff.is_some()
                {
                    return Err("Octopus account discovery cannot be combined with HA or manual tariff codes".into());
                }
                let key = std::fs::read_to_string(path)
                    .map_err(|_| "Cannot read Octopus API key file")?;
                let key = key.trim().to_owned();
                if key.is_empty() || !key.bytes().all(|c| c.is_ascii_graphic()) || key.contains(':')
                {
                    return Err("Invalid Octopus API key file".into());
                }
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(20))
                    .build()
                    .map_err(|_| "Cannot create Octopus HTTP client")?;
                Ok(Some(Self {
                    client,
                    tariffs: Vec::new(),
                    account: Some(AccountAccess {
                        number: number.into(),
                        key,
                    }),
                    origin: "https://api.octopus.energy".into(),
                }))
            }
            _ => Err("Octopus account number and API key file must both be configured".into()),
        }
    }
    async fn private_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, String> {
        let access = self
            .account
            .as_ref()
            .ok_or("Octopus account is not configured")?;
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .basic_auth(&access.key, Some(""))
            .query(query)
            .send()
            .await
            .map_err(|_| "Octopus account request failed")?;
        if !response.status().is_success() {
            return Err(format!(
                "Octopus account API returned HTTP {}",
                response.status().as_u16()
            ));
        }
        response
            .json()
            .await
            .map_err(|_| "Invalid Octopus account response".into())
    }
    async fn consumption(
        &self,
        point: &MeterPoint,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Consumption>, String> {
        if !identifier(&point.mpan) || point.meters.iter().any(|m| !identifier(&m.serial_number)) {
            return Err("Invalid Octopus meter identifier".into());
        }
        let mut readings = Vec::new();
        let mut serials = std::collections::HashSet::new();
        for serial in point
            .meters
            .iter()
            .map(|m| &m.serial_number)
            .filter(|s| serials.insert((*s).clone()))
        {
            let path = format!(
                "/v1/electricity-meter-points/{}/meters/{serial}/consumption/",
                point.mpan
            );
            let mut complete = false;
            for page in 1..=5 {
                let result: Page<Consumption> = self
                    .private_get(
                        &path,
                        &[
                            ("period_from", from.to_string()),
                            ("period_to", to.to_string()),
                            ("page_size", "1500".into()),
                            ("page", page.to_string()),
                            ("order_by", "period".into()),
                        ],
                    )
                    .await?;
                readings.extend(
                    result
                        .results
                        .into_iter()
                        .filter(|c| c.interval_start >= from && c.interval_end <= to),
                );
                if result.next.is_none() {
                    complete = true;
                    break;
                }
            }
            if !complete {
                return Err("Octopus consumption pagination limit exceeded".into());
            }
        }
        validate_consumption(&mut readings)?;
        Ok(readings)
    }
    pub(super) async fn read_account(&self) -> Result<Vec<Meter>, String> {
        let now = Timestamp::now();
        let days = bounds(now)?;
        let history_start = days[1]
            .to_zoned(TimeZone::get("Europe/London").map_err(|_| "London timezone unavailable")?)
            .checked_sub(7.days())
            .map_err(|_| "Invalid consumption date range")?
            .timestamp();
        let access = self
            .account
            .as_ref()
            .ok_or("Octopus account is not configured")?;
        let account: Account = self
            .private_get(&format!("/v1/accounts/{}/", access.number), &[])
            .await?;
        let mut meters = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for property in account.properties {
            if property.moved_out_at.is_some_and(|end| end <= now)
                || property.moved_in_at.is_some_and(|start| start > now)
            {
                continue;
            }
            for point in property.electricity_meter_points {
                if !seen.insert(point.mpan.clone()) {
                    return Err("Duplicate active Octopus meter point".into());
                }
                let current: Vec<_> = point
                    .agreements
                    .iter()
                    .filter(|a| active(a.valid_from, a.valid_to, now))
                    .collect();
                if current.is_empty() {
                    continue;
                }
                if current.len() != 1 {
                    return Err("Ambiguous active Octopus agreements".into());
                }
                let tariff = Tariff::new(&current[0].tariff_code, point.is_export)?;
                let mut rates = Vec::new();
                let mut standing = Vec::new();
                for agreement in &point.agreements {
                    let from = agreement.valid_from.max(history_start);
                    let to = agreement.valid_to.unwrap_or(days[3]).min(days[3]);
                    if from >= to {
                        continue;
                    }
                    let historical = Tariff::new(&agreement.tariff_code, point.is_export)?;
                    let (unit, charges) = tokio::try_join!(
                        self.prices(&historical, "standard-unit-rates", from, to),
                        self.prices(&historical, "standing-charges", from, to),
                    )?;
                    rates.extend(clip(unit, from, to)?);
                    standing.extend(clip(charges, from, to)?);
                }
                let mut result = meter(&tariff, rates.clone(), standing.clone(), &days, now)?;
                result.id = point.mpan.clone();
                // A day may span contracts; avoid labelling historical slots as the current contract.
                for schedule in &mut result.schedules {
                    let i = match schedule.day.as_str() {
                        "previous" => 0,
                        "current" => 1,
                        _ => 2,
                    };
                    let agreements: Vec<_> = point
                        .agreements
                        .iter()
                        .filter(|a| {
                            a.valid_from < days[i + 1] && a.valid_to.is_none_or(|end| end > days[i])
                        })
                        .collect();
                    schedule.tariff_code = if agreements.len() == 1 {
                        Some(agreements[0].tariff_code.clone())
                    } else {
                        None
                    };
                }
                match self.consumption(&point, history_start, now).await {
                    Ok(consumption) => {
                        result.daily_costs =
                            daily_costs(&consumption, &rates, &standing, history_start, days[1])?;
                        result.completed_day = result.daily_costs.last().cloned();
                        result.consumption = consumption;
                        if result
                            .completed_day
                            .as_ref()
                            .is_none_or(|d| d.start != days[0])
                        {
                            result.consumption_error = Some("Latest complete consumption day is delayed or has gaps; incomplete days are not priced".into());
                        }
                    }
                    Err(error) => result.consumption_error = Some(error),
                }
                meters.push(result);
            }
        }
        if meters.is_empty() {
            return Err("No active Octopus electricity agreements found".into());
        }
        Ok(meters)
    }
}
fn clip(prices: Vec<Price>, from: Timestamp, to: Timestamp) -> Result<Vec<Price>, String> {
    let mut clipped = Vec::new();
    for mut p in prices {
        if !p.value_inc_vat.is_finite() || p.valid_to.is_some_and(|end| end <= p.valid_from) {
            return Err("Invalid Octopus price interval".into());
        }
        p.valid_from = p.valid_from.max(from);
        p.valid_to = Some(p.valid_to.unwrap_or(to).min(to));
        if p.valid_from < p.valid_to.unwrap() {
            clipped.push(p);
        }
    }
    Ok(clipped)
}
fn validate_consumption(readings: &mut [Consumption]) -> Result<(), String> {
    readings.sort_by_key(|c| c.interval_start);
    if readings.iter().any(|c| {
        !c.consumption.is_finite() || c.consumption < 0.0 || c.interval_start >= c.interval_end
    }) || readings
        .windows(2)
        .any(|w| w[0].interval_end > w[1].interval_start)
    {
        return Err("Invalid or overlapping Octopus consumption intervals".into());
    }
    Ok(())
}
fn daily_costs(
    consumption: &[Consumption],
    prices: &[Price],
    standing: &[Price],
    from: Timestamp,
    to: Timestamp,
) -> Result<Vec<CompletedDay>, String> {
    let tz = TimeZone::get("Europe/London").map_err(|_| "London timezone unavailable")?;
    let mut start = from;
    let mut costs = Vec::new();
    while start < to {
        let end = start
            .to_zoned(tz.clone())
            .checked_add(1.days())
            .map_err(|_| "Invalid consumption day")?
            .timestamp();
        let mut cursor = start;
        let mut total = 0.0;
        let mut cost = 0.0;
        let charges: Vec<_> = standing
            .iter()
            .filter(|p| p.valid_from <= start && p.valid_to.is_none_or(|t| t >= end))
            .collect();
        let mut valid = charges.len() == 1;
        for c in consumption
            .iter()
            .filter(|c| c.interval_start >= start && c.interval_start < end)
        {
            let rate: Vec<_> = prices
                .iter()
                .filter(|p| {
                    p.valid_from <= c.interval_start
                        && p.valid_to.is_none_or(|t| t >= c.interval_end)
                })
                .collect();
            if c.interval_start != cursor || c.interval_end > end || rate.len() != 1 {
                valid = false;
                break;
            }
            total += c.consumption;
            // Octopus documents half-even rounding of consumption to 0.01 kWh for billing.
            cost +=
                (c.consumption * 100.0).round_ties_even() / 100.0 * rate[0].value_inc_vat / 100.0;
            cursor = c.interval_end;
        }
        if valid && cursor == end {
            let charge = charges[0].value_inc_vat / 100.0;
            if charge.is_finite() && charge >= 0.0 && cost.is_finite() {
                costs.push(CompletedDay {
                    start,
                    consumption_kwh: Some(total),
                    cost_gbp: ((cost + charge) * 100.0).round() / 100.0,
                    standing_charge_gbp: Some(charge),
                    estimated: true,
                });
            }
        }
        start = end;
    }
    Ok(costs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn intervals(start: Timestamp, end: Timestamp, amount: f64) -> Vec<Consumption> {
        let mut cursor = start;
        let mut result = Vec::new();
        while cursor < end {
            let next = cursor.checked_add(30.minutes()).unwrap();
            result.push(Consumption {
                interval_start: cursor,
                interval_end: next,
                consumption: amount,
            });
            cursor = next;
        }
        result
    }
    fn fixed(start: Timestamp, end: Timestamp, value: f64) -> Price {
        Price {
            valid_from: start,
            valid_to: Some(end),
            value_inc_vat: value,
            payment_method: None,
        }
    }
    #[test]
    fn complete_dst_days_have_correct_usage_and_one_standing_charge() {
        for (date, count) in [("2026-03-29T12:00:00Z", 46), ("2026-10-25T12:00:00Z", 50)] {
            let days = bounds(date.parse().unwrap()).unwrap();
            let usage = intervals(days[1], days[2], 1.0);
            assert_eq!(usage.len(), count);
            let costs = daily_costs(
                &usage,
                &[fixed(days[1], days[2], 25.0)],
                &[fixed(days[1], days[2], 45.0)],
                days[1],
                days[2],
            )
            .unwrap();
            assert_eq!(costs.len(), 1);
            assert_eq!(costs[0].consumption_kwh, Some(count as f64));
            assert_eq!(costs[0].cost_gbp, count as f64 * 0.25 + 0.45);
            assert!(costs[0].estimated);
        }
    }
    #[test]
    fn incomplete_days_missing_prices_and_overlap_never_become_complete_bills() {
        let days = bounds("2026-11-19T12:00:00Z".parse().unwrap()).unwrap();
        let mut usage = intervals(days[0], days[1], 1.0);
        let unit = [fixed(days[0], days[1], 25.0)];
        let standing = [fixed(days[0], days[1], 45.0)];
        usage.remove(4);
        assert!(
            daily_costs(&usage, &unit, &standing, days[0], days[1])
                .unwrap()
                .is_empty()
        );
        usage = intervals(days[0], days[1], 1.0);
        assert!(
            daily_costs(&usage, &[], &standing, days[0], days[1])
                .unwrap()
                .is_empty()
        );
        assert!(
            daily_costs(&usage, &unit, &[], days[0], days[1])
                .unwrap()
                .is_empty()
        );
        usage.push(usage[0].clone());
        assert!(validate_consumption(&mut usage).is_err());
    }
    #[test]
    fn applies_interval_rates_including_negative_prices_and_half_even_rounding() {
        let days = bounds("2026-11-19T12:00:00Z".parse().unwrap()).unwrap();
        let usage = intervals(days[0], days[1], 0.015);
        let split = days[0].checked_add(12.hours()).unwrap();
        let rates = [fixed(days[0], split, -10.0), fixed(split, days[1], 30.0)];
        let costs = daily_costs(
            &usage,
            &rates,
            &[fixed(days[0], days[1], 45.0)],
            days[0],
            days[1],
        )
        .unwrap();
        assert_eq!(costs[0].cost_gbp, 0.55); // 24 * 0.02 * (-0.10 + 0.30) + 0.45.
    }
    #[test]
    fn clips_price_ranges_to_historical_agreements() {
        let start: Timestamp = "2026-11-18T00:00:00Z".parse().unwrap();
        let split = start.checked_add(12.hours()).unwrap();
        let end = start.checked_add(24.hours()).unwrap();
        let old = clip(vec![fixed(start, end, 10.0)], start, split).unwrap();
        let new = clip(vec![fixed(start, end, 20.0)], split, end).unwrap();
        assert_eq!(old[0].valid_to, Some(split));
        assert_eq!(new[0].valid_from, split);
    }
    #[tokio::test]
    async fn account_discovery_authentication_pagination_and_consumption_work_without_ha() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let days = bounds(Timestamp::now()).unwrap();
        let usage = intervals(days[0], days[1], 1.0);
        let date = days[0].to_string();
        let expected_usage = usage.len() as f64;
        let server = tokio::spawn(async move {
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut bytes = [0; 4096];
                    let n = stream.read(&mut bytes).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&bytes[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let private = path.contains("/accounts/") || path.contains("/consumption/");
                assert_eq!(
                    request
                        .to_lowercase()
                        .contains("authorization: basic c2tfdgvzddo="),
                    private
                );
                let body = if path.contains("/accounts/") {
                    json!({"properties":[{"moved_in_at":null,"moved_out_at":null,"electricity_meter_points":[{
                        "mpan":"1234567890123","is_export":false,"meters":[{"serial_number":"TEST123"}],
                        "agreements":[{"tariff_code":"E-1R-TEST-J","valid_from":date,"valid_to":null}]
                    }]}]})
                } else if path.contains("/consumption/") {
                    let second = path.contains("page=2");
                    json!({"results":if second {&usage[24..]} else {&usage[..24]},"next":if second {None} else {Some("https://untrusted.example/steal-key")}})
                } else {
                    let value = if path.contains("/standing-charges/") {45.0} else {25.0};
                    json!({"results":[{"valid_from":date,"valid_to":null,"value_inc_vat":value,"payment_method":null}],"next":null})
                }.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let reader = Reader {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            tariffs: Vec::new(),
            origin,
            account: Some(AccountAccess {
                number: "A-TEST".into(),
                key: "sk_test".into(),
            }),
        };
        let meters = tokio::time::timeout(Duration::from_secs(10), reader.read())
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        assert_eq!(meters.len(), 1);
        assert_eq!(meters[0].tariff_code.as_deref(), Some("E-1R-TEST-J"));
        assert_eq!(
            meters[0].completed_day.as_ref().unwrap().consumption_kwh,
            Some(expected_usage)
        );
        assert!(meters[0].consumption_error.is_none());
    }
    #[tokio::test]
    async fn authentication_errors_do_not_export_keys_urls_or_response_bodies() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 11\r\nConnection: close\r\n\r\nsecret-body").await.unwrap();
        });
        let reader = Reader {
            client: reqwest::Client::new(),
            origin,
            tariffs: Vec::new(),
            account: Some(AccountAccess {
                number: "A-TEST".into(),
                key: "private-test-key".into(),
            }),
        };
        let error = match reader
            .private_get::<Account>("/v1/accounts/A-TEST/", &[])
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("Expected authentication failure"),
        };
        assert_eq!(error, "Octopus account API returned HTTP 401");
        server.await.unwrap();
    }
}
