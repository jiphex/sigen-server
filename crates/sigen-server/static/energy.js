'use strict';
const gbp = value => new Intl.NumberFormat('en-GB', {style: 'currency', currency: 'GBP'}).format(value);
const pence = value => `${(value * 100).toFixed(4).replace(/0+$/, '').replace(/\.$/, '')}p`;
const londonDate = value => new Intl.DateTimeFormat('en-CA', {timeZone: 'Europe/London', year: 'numeric', month: '2-digit', day: '2-digit'}).format(new Date(value));
const timeLabel = value => new Intl.DateTimeFormat('en-GB', {timeZone: 'Europe/London', day: '2-digit', month: 'short', hour: '2-digit', minute: '2-digit', timeZoneName: 'short'}).format(new Date(value));
function costValues(s, meter, now = Date.now()) {
  const fresh = s.energy?.available && s.energy.last_success_unix != null && now / 1000 - s.energy.last_success_unix < 180;
  const rates = meter.schedules.flatMap(day => day.rates);
  const rate = fresh ? rates.find(r => Date.parse(r.start) <= now && now < Date.parse(r.end)) : null;
  const hardwareFresh = s.online && s.last_success_unix != null && now / 1000 - s.last_success_unix < 120;
  const power = hardwareFresh && s.readings?.grid_sensor_connected ? s.readings.grid_power_watts : null;
  const directionMatches = Number.isFinite(power) && (meter.is_export ? power < 0 : power >= 0);
  const hourly = rate && directionMatches ? Math.abs(power) / 1000 * rate.value_inc_vat : null;
  const day = meter.schedules.find(d => d.day === 'current');
  const today = londonDate(now);
  const dayRates = day?.rates ?? [];
  const flat = fresh && dayRates.length > 0 && londonDate(dayRates[0].start) === today
    && new Date(dayRates[0].start).getTime() <= now
    && new Intl.DateTimeFormat('en-GB', {timeZone:'Europe/London', hour:'2-digit', minute:'2-digit', hourCycle:'h23'}).format(new Date(dayRates[0].start)) === '00:00'
    && londonDate(dayRates.at(-1).end) !== today
    && dayRates.every((r, i) => r.value_inc_vat === dayRates[0].value_inc_vat && !r.is_intelligent_adjusted && (!i || r.start === dayRates[i-1].end));
  function comparison(sample) {
    if (meter.is_export || !flat || !hardwareFresh || sample?.value == null || sample.unsupported || sample.last_error
      || sample.updated_unix == null || now / 1000 - sample.updated_unix > 600
      || s.device_day == null || sample.device_day !== s.device_day || londonDate(now) !== new Date(s.device_day * 86400000).toISOString().slice(0,10)) return null;
    return sample.value * dayRates[0].value_inc_vat;
  }
  return {fresh, rate, hourly, solar: comparison(s.details?.pv_daily_kwh), load: comparison(s.details?.load_daily_kwh)};
}
function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function card(title, value, note) {
  const node = element('article');
  node.append(element('h3', title), element('p', value, 'value'), element('small', note));
  return node;
}
function renderEnergy(s) {
  const root = document.getElementById('energy-meters');
  const energy = s.energy;
  const openSchedules = new Set(Array.from(root.querySelectorAll('details[open]')).map(d => d.dataset.schedule));
  const focusedSchedule = document.activeElement?.closest('details')?.dataset.schedule;
  document.getElementById('energy-health').textContent = !energy ? 'Home Assistant tariffs are not configured.'
    : `Home Assistant · ${energy.last_success_unix ? 'last updated ' + timeLabel(energy.last_success_unix * 1000) : 'waiting for first update'}`;
  document.getElementById('energy-error').textContent = energy?.last_error ?? '';
  root.replaceChildren();
  if (!energy) {
    root.append(element('p', 'Configure the Home Assistant connection on the server to see rates and completed-day costs.'));
    return;
  }
  if (!energy.meters.length) root.append(element('p', 'No electricity tariff data is available yet.'));
  const imports = energy.meters.filter(m => !m.is_export);
  const exports = energy.meters.filter(m => m.is_export);
  for (const meter of energy.meters) {
    const v = costValues(s, meter);
    const unique = (meter.is_export ? exports : imports).length === 1;
    const section = element('section', null, 'panel');
    section.append(element('h2', `${meter.is_export ? 'Export' : 'Import'} · ${meter.tariff_code ?? 'Tariff unknown'}`));
    section.append(element('p', `Meter ${meter.id}${v.fresh ? '' : ' · tariff data unavailable or stale'}`));
    const cards = element('div', null, 'cards energy-cards');
    cards.append(card('Current rate', v.rate ? `${pence(v.rate.value_inc_vat)}/kWh` : '—', v.rate ? `${timeLabel(v.rate.start)} – ${timeLabel(v.rate.end)}${v.rate.is_intelligent_adjusted ? ' · Intelligent adjustment; depends on charging eligibility' : ''}${v.rate.is_capped ? ' · capped' : ''}` : 'No fresh rate covering the current time'));
    cards.append(card('Standing charge', v.fresh && meter.standing_charge_gbp_per_day != null ? gbp(meter.standing_charge_gbp_per_day) : '—', 'Per day · separate from unit rate'));
    cards.append(card(meter.is_export ? 'Live export revenue' : 'Live import cost', unique && v.hourly != null ? `${gbp(v.hourly)}/h` : '—', !unique ? 'Multiple tariffs: plant power cannot be assigned to a meter' : 'At current grid power · excludes standing charge'));
    const completed = meter.completed_day;
    cards.append(card('Latest completed day', completed ? gbp(completed.cost_gbp) : '—', completed ? `${londonDate(completed.start)}${completed.consumption_kwh != null ? ' · ' + completed.consumption_kwh.toFixed(3) + ' kWh' : ''}${completed.standing_charge_gbp != null ? ' · includes ' + gbp(completed.standing_charge_gbp) + ' standing charge' : ' · standing charge breakdown unavailable'}` : 'No completed-day cost available'));
    if (!meter.is_export) {
      cards.append(card('Today’s solar equivalent', unique && v.solar != null ? gbp(v.solar) : '—', 'Inverter generation at today’s flat import rate · comparison only'));
      cards.append(card('Today’s load equivalent', unique && v.load != null ? gbp(v.load) : '—', 'Plant load at today’s flat import rate · comparison only'));
    }
    section.append(cards);
    for (const schedule of meter.schedules) {
      const details = element('details');
      details.dataset.schedule = `${meter.id}:${schedule.day}`;
      details.open = openSchedules.has(details.dataset.schedule);
      const names = {previous: 'Yesterday', current: 'Today', next: 'Tomorrow'};
      details.append(element('summary', `${names[schedule.day] ?? schedule.day} · ${schedule.rates.length} rate intervals`));
      const scroll = element('div', null, 'table-scroll'); scroll.tabIndex = 0;
      scroll.setAttribute('role','region'); scroll.setAttribute('aria-label', `${names[schedule.day]} tariff rates`);
      const table = element('table'); table.append(element('caption', 'Rates including VAT · Europe/London'));
      const head = element('thead'), header = element('tr');
      for (const label of ['From', 'Until', 'Rate / kWh', 'Notes']) { const th = element('th', label); th.scope = 'col'; header.append(th); }
      head.append(header); table.append(head);
      const body = element('tbody');
      for (const r of schedule.rates) {
        const row = element('tr');
        for (const text of [timeLabel(r.start), timeLabel(r.end), pence(r.value_inc_vat), [r.is_capped && 'Capped', r.is_intelligent_adjusted && 'Intelligent adjusted · conditional'].filter(Boolean).join(' · ') || 'Standard']) row.append(element('td', text));
        body.append(row);
      }
      table.append(body); scroll.append(table); details.append(scroll); section.append(details);
    }
    root.append(section);
    if (focusedSchedule) {
      const target = Array.from(section.querySelectorAll('details')).find(d => d.dataset.schedule === focusedSchedule);
      target?.querySelector('summary').focus({preventScroll: true});
    }
  }
  if (!exports.length) root.append(element('p', 'No export tariff is available. Export revenue cannot be calculated.'));
}
if (typeof document !== 'undefined') {
  let busy = false, paused = false, generation = 0;
  async function refreshEnergy() {
    if (busy) return;
    busy = true;
    const started = generation;
    const button = document.getElementById('refresh'); button.disabled = true;
    try {
      const response = await fetch('/api/status', {signal: AbortSignal.timeout(10000)});
      if (!response.ok) throw new Error(`Status request returned HTTP ${response.status}`);
      const snapshot = await response.json();
      if (generation === started) renderEnergy(snapshot);
    } catch (error) {
      if (generation !== started) return;
      document.getElementById('energy-meters').replaceChildren();
      document.getElementById('energy-health').textContent = 'Costs unavailable';
      document.getElementById('energy-error').textContent = error.message;
    } finally { busy = false; button.disabled = false; }
  }
  document.getElementById('refresh').addEventListener('click', refreshEnergy);
  document.getElementById('updates-toggle').addEventListener('click', () => {
    paused = !paused; generation++;
    document.getElementById('updates-toggle').textContent = paused ? 'Resume updates' : 'Pause updates';
    document.getElementById('updates-status').textContent = paused ? 'Updates paused. Displayed costs are a frozen snapshot.' : 'Automatic updates run every 15 seconds.';
    if (!paused) refreshEnergy();
  });
  refreshEnergy();
  setInterval(() => { if (!paused) refreshEnergy(); }, 15000);
}
if (typeof module !== 'undefined') module.exports = {costValues};
