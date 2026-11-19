const {test} = require('node:test');
const assert = require('node:assert/strict');
const {costValues} = require('../static/energy.js');
const now = Date.parse('2026-11-19T12:00:00Z');
function fixture() {
  const sample = {value: 10, updated_unix: now/1000, device_day: Math.floor(now/86400000)};
  const s = {energy:{available:true,last_success_unix:now/1000}, online:true,last_success_unix:now/1000,
    device_day: sample.device_day, readings:{grid_sensor_connected:true,grid_power_watts:2000},
    details:{pv_daily_kwh:sample,load_daily_kwh:sample}};
  const meter = {is_export:false,schedules:[{day:'current',rates:[{start:'2026-11-19T00:00:00Z',end:'2026-11-20T00:00:00Z',value_inc_vat:0.25}]}]};
  return {s,meter};
}
test('prices live import and flat-day energy comparisons without adding standing charges', () => {
  const {s,meter} = fixture();
  const v = costValues(s,meter,now);
  assert.equal(v.hourly,0.5);
  assert.equal(v.solar,2.5);
  assert.equal(v.load,2.5);
});
test('variable tariffs allow live rates but cannot price daily totals', () => {
  const {s,meter} = fixture();
  meter.schedules[0].rates = [
    {start:'2026-11-19T00:00:00Z',end:'2026-11-19T06:00:00Z',value_inc_vat:0.1},
    {start:'2026-11-19T06:00:00Z',end:'2026-11-20T00:00:00Z',value_inc_vat:0.3}];
  const v = costValues(s,meter,now);
  assert.equal(v.hourly,0.6);
  assert.equal(v.solar,null);
});
test('stale tariffs, stale hardware and missing grid sensors suppress inferred costs', () => {
  const {s,meter} = fixture();
  s.energy.last_success_unix -= 181;
  assert.equal(costValues(s,meter,now).hourly,null);
  assert.equal(costValues(s,meter,now).load,null);
  s.energy.last_success_unix = now/1000;
  s.last_success_unix -= 121;
  assert.equal(costValues(s,meter,now).hourly,null);
  s.last_success_unix = now/1000;
  s.readings.grid_sensor_connected = false;
  assert.equal(costValues(s,meter,now).hourly,null);
});
test('export uses its own tariff; negative prices retain their sign', () => {
  const {s,meter} = fixture();
  s.readings.grid_power_watts = -1000;
  assert.equal(costValues(s,meter,now).hourly,null);
  meter.is_export = true;
  meter.schedules[0].rates[0].value_inc_vat = -0.05;
  assert.equal(costValues(s,meter,now).hourly,-0.05);
  assert.equal(costValues(s,meter,now).solar,null);
});
test('partial days, rate gaps, conditional rates and old daily samples suppress comparisons', () => {
  const {s,meter} = fixture();
  const r = meter.schedules[0].rates[0];
  r.start = '2026-11-19T01:00:00Z';
  assert.equal(costValues(s,meter,now).solar,null);
  r.start = '2026-11-19T00:00:00Z';
  r.is_intelligent_adjusted = true;
  assert.equal(costValues(s,meter,now).solar,null);
  r.is_intelligent_adjusted = false;
  s.details.pv_daily_kwh.updated_unix -= 601;
  assert.equal(costValues(s,meter,now).solar,null);
});
test('flat-day comparisons support a 25-hour London day at the DST boundary', () => {
  const {s,meter} = fixture();
  const dstNow = Date.parse('2026-10-25T12:00:00Z');
  s.energy.last_success_unix = s.last_success_unix = dstNow/1000;
  s.device_day = s.details.pv_daily_kwh.device_day = Math.floor(dstNow/86400000);
  s.details.pv_daily_kwh.updated_unix = dstNow/1000;
  meter.schedules[0].rates = [{start:'2026-10-24T23:00:00Z',end:'2026-10-26T00:00:00Z',value_inc_vat:0.25}];
  assert.equal(costValues(s,meter,dstNow).solar,2.5);
});

test('page renders missing configuration, stale history, schedules and ambiguous meters safely', () => {
  const vm = require('node:vm');
  const fs = require('node:fs');
  class Node {
    constructor(tag) { this.tag = tag; this.children = []; this.dataset = {}; this.textContent = ''; }
    append(...nodes) { this.children.push(...nodes); }
    replaceChildren(...nodes) { this.children = nodes; }
    setAttribute() {}
    querySelectorAll() { return []; }
    set innerHTML(_) { throw new Error('HTML injection'); }
    get text() { return this.textContent + ' ' + this.children.map(n => n.text).join(' '); }
  }
  const nodes = Object.fromEntries(['energy-meters','energy-health','energy-error'].map(id => [id,new Node('div')]));
  const context = vm.createContext({Intl, Date, console});
  vm.runInContext(fs.readFileSync(require.resolve('../static/energy.js'), 'utf8'),context);
  context.document = {getElementById:id => nodes[id],createElement:tag => new Node(tag)};
  context.s = {energy:null};
  vm.runInContext('renderEnergy(s)',context);
  assert.match(nodes['energy-health'].text,/not configured/);
  const {s,meter} = fixture();
  meter.id = 'meter'; meter.tariff_code = '<script>example</script>';
  meter.completed_day = {start:'2026-11-18T00:00:00Z',cost_gbp:3.06,consumption_kwh:10.575,standing_charge_gbp:0.45};
  s.energy.meters = [meter, {...meter,id:'second-meter'}];
  s.energy.available = false;
  context.s = s;
  vm.runInContext('renderEnergy(s)',context);
  const text = nodes['energy-meters'].text;
  assert.match(text,/£3.06/);
  assert.match(text,/includes £0.45 standing charge/);
  assert.match(text,/Multiple tariffs/);
  assert.match(text,/No export tariff/);
  assert.match(text,/Today · 1 rate intervals/);
  assert.match(text,/<script>example<\/script>/);
  assert.match(text,/tariff data unavailable or stale/);
  s.energy.source = 'octopus';
  vm.runInContext('renderEnergy(s)',context);
  assert.match(nodes['energy-health'].text,/Octopus API/);
  assert.match(nodes['energy-meters'].text,/account-specific Intelligent charging adjustments are unavailable/);
  s.energy.source = 'octopus_account';
  meter.completed_day.estimated = true;
  meter.daily_costs = [meter.completed_day];
  meter.consumption = [{interval_start:'2026-11-18T00:00:00Z', interval_end:'2026-11-18T00:30:00Z', consumption:0.124}];
  meter.consumption_error = 'Delayed readings';
  vm.runInContext('renderEnergy(s)',context);
  assert.match(nodes['energy-meters'].text,/meters and tariffs discovered automatically/);
  assert.match(nodes['energy-meters'].text,/Latest complete day · estimated/);
  assert.match(nodes['energy-meters'].text,/Completed-day consumption and estimated costs/);
  assert.match(nodes['energy-meters'].text,/0.124 kWh/);
  assert.match(nodes['energy-meters'].text,/Delayed readings/);
});
