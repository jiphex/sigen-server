# Sigen server dashboard

Import [sigen-server.json](sigen-server.json) into Grafana using **Dashboards →
New → Import → Upload dashboard JSON file**. It uses Grafana's Classic dashboard
JSON model and built-in Prometheus, stat, time-series, and state-timeline panels;
no additional plugins or recording rules are required.

Select your **Prometheus** datasource, then the scrape **Job** and **Instance**
from the dropdowns. Each dashboard shows one server at a time. Job and instance
values are discovered from `sigen_up`, so the scrape job need not be named `sigen`.
Set the datasource's scrape interval to match your Prometheus configuration
(15 seconds in the server documentation) so error-rate queries use a suitable
`$__rate_interval`. The default view is the last 24 hours, refreshing every 30 seconds.

The dashboard includes:

- Exporter and hardware availability, current solar/grid/battery power, and SOC.
- Power history, battery reserve, temperatures, cell voltages, health, limits,
  and available energy.
- Device-local daily energy totals and cumulative grid counters, plus grid
  import/export increases over the selected time range.
- Inverter phase voltage/current, plant grid phase power, frequency, power
  factor, wiring type, and grid sensor connectivity.
- EMS settings as readback values, alert state/known/enabled history, and raw
  alarm bitfields.
- Poll/control failures, sample freshness, MQTT availability, journal integrity,
  and Pushover delivery metrics.

## Interpretation

Positive grid power means import; negative means export. Positive battery power
means charging; negative means discharging. Plant active power is not household
load consumption. Daily PV and battery energy are inverter measurements; daily
load and grid energy counters are plant measurements.

Daily energy values are gauges that reset on the **device's local day**. The
dashboard displays them directly and never applies `rate` or `increase` to them.
Grid import/export totals are counters; their selected-range increases account
for observed resets and use Prometheus extrapolation. Sparse samples or missing
scrapes can reduce accuracy, so these estimates are not billing totals.

Current-value panels use instant queries at the selected range end. They do not
fall back to an earlier successful reading. Except for scrape availability,
current values are also filtered by a successful Prometheus scrape. Missing
measurements display **Unavailable**, and historical gaps are not joined or
filled with zeros. Unsupported phases, stale optional telemetry, disabled MQTT
or Pushover, and journal metrics on older versions can all produce missing data.

The server omits stale hardware readings. Alerts can remain active while their
condition is unknown, so read the **Alert state** and **Alert condition known**
panels together. Alarm masks are raw numbers; their size does not indicate
severity. Journal record/loss counts and its sticky integrity flag reset on
server restart. Pushover acceptance is not proof of delivery to a phone.

The event journal itself is not stored in Prometheus; use the server's timeline
or `sigen-journal` for detailed events. The running server version is currently
available through the web dashboard and MQTT, rather than a Prometheus metric.

To provision from files, place the JSON in your Grafana dashboard provider's
directory. The datasource remains selectable through the dashboard variable;
there is no installation-specific datasource UID embedded in the file.
