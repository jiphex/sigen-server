const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const staticDir = path.join(__dirname, '../static');
const demoSource = fs.readFileSync(path.join(staticDir, 'demo.js'), 'utf8');
const appSource = fs.readFileSync(path.join(staticDir, 'app.js'), 'utf8');

function simulator() {
  const context = vm.createContext({});
  vm.runInContext(demoSource, context);
  return context.createDemo();
}

function element(tag = 'div', ownerDocument) {
  return {
    tag, nodeType: 1, childNodes: [], attributes: {}, dataset: {}, listeners: {}, hidden: false,
    textWrites: 0, isConnected: true, ownerDocument,
    get children() { return this.childNodes.filter(node => node.nodeType === 1); },
    get textContent() { return this.childNodes.map(node => node.textContent).join(''); },
    set textContent(text) { this.textWrites++; this.childNodes = [{nodeType: 3, textContent: text}]; },
    classList: {toggle() {}, add() {}},
    setAttribute(key, value) { this.attributes[key] = value; },
    removeAttribute(key) { delete this.attributes[key]; },
    append(...nodes) {
      for (const node of nodes) {
        const item = typeof node === 'string' ? {nodeType: 3, textContent: node} : node;
        item.parent = this;
        this.childNodes.push(item);
      }
    },
    prepend(node) { node.parent = this; this.childNodes.unshift(node); },
    remove() { this.parent.childNodes = this.parent.childNodes.filter(node => node !== this); },
    replaceChildren(...nodes) { this.childNodes = []; this.append(...nodes); },
    addEventListener(name, handler) { this.listeners[name] = handler; },
    focus() { this.ownerDocument.activeElement = this; },
    closest(selector) {
      if (selector === 'form' && this.tag === 'form') return this;
      if (selector === 'fieldset:disabled' && this.tag === 'fieldset' && this.disabled) return this;
      return this.parent?.closest(selector) || null;
    },
    querySelector() { return this.button ??= element('button', this.ownerDocument); },
    querySelectorAll() { return this.children.flatMap(child => [child, ...child.querySelectorAll()]).filter(child => child.tag === 'input'); }
  };
}

function dashboard(search, fetch) {
  const document = {};
  const nodes = Object.fromEntries([...fs.readFileSync(path.join(staticDir, 'index.html'), 'utf8')
    .matchAll(/<([a-z][\w-]*)\b[^>]*\bid="([^"]+)"/g)].map(match => [match[2], element(match[1], document)]));
  Object.assign(document, {getElementById: id => nodes[id], body: element('body', document),
    createElement: tag => element(tag, document), createElementNS: (_, tag) => element(tag, document),
    createTextNode: text => ({nodeType: 3, textContent: text})});
  document.activeElement = document.body;
  nodes['control-result'].textContent = 'Waiting for data…';
  nodes['demo-banner'].hidden = true;
  nodes['demo-metrics'].hidden = true;
  let tick;
  const context = vm.createContext({
    URLSearchParams, window: {location: {search}}, fetch,
    setInterval(callback) { tick = callback; },
    document
  });
  vm.runInContext(demoSource, context);
  vm.runInContext(appSource, context);
  return {nodes, context, document, tick: () => tick()};
}

test('scenarios show generation, discharge, alarms and frozen offline readings', () => {
  const demo = simulator();
  const solar = demo.snapshot();
  assert.ok(solar.online && solar.readings.pv_power_watts > 0);
  demo.setScenario('night');
  const night = demo.snapshot();
  assert.equal(night.readings.pv_power_watts, 0);
  assert.ok(night.readings.battery_power_watts < 0);
  demo.setScenario('alarm');
  const alarm = demo.snapshot();
  assert.ok(alarm.alerts.items.find(a => a.id === 'battery_alarm').active);
  assert.equal(alarm.readings.alarms[0], 1);
  demo.setAlert('battery_alarm', false);
  assert.equal(demo.snapshot().alerts.items.find(a => a.id === 'battery_alarm').active, false);
  const readings = demo.snapshot().readings;
  demo.setScenario('offline');
  const offline = demo.snapshot();
  assert.equal(offline.online, false);
  assert.equal(offline.controls, null);
  assert.equal(offline.readings, readings);
  assert.ok(offline.alerts.items.find(a => a.id === 'hardware_offline').active);
  assert.throws(() => demo.send({action: 'backup_soc', percent: 30}), /offline/);
});

test('simulated controls honor standby, validation, reserve and reset', () => {
  const demo = simulator();
  assert.throws(() => demo.send({action: 'ems_mode', mode: 3}), /Enable remote EMS/);
  demo.send({action: 'remote_ems', enabled: true});
  assert.equal(demo.snapshot().controls.remote_ems_mode, 1);
  assert.equal(demo.snapshot().readings.battery_power_watts, 0);
  demo.send({action: 'ems_mode', mode: 3});
  assert.ok(demo.snapshot().readings.battery_power_watts > 0);
  demo.send({action: 'ems_mode', mode: 6});
  assert.ok(demo.snapshot().readings.battery_power_watts < 0);
  demo.send({action: 'backup_soc', percent: 90});
  assert.equal(demo.snapshot().readings.battery_power_watts, 0);
  assert.throws(() => demo.send({action: 'backup_soc', percent: 101}), /between/);
  assert.throws(() => demo.send({action: 'ems_mode', mode: 7}), /0–6/);
  assert.equal(simulator().snapshot().controls.remote_ems_enabled, false);
});

test('demo Prometheus output matches live exporter metadata and contains finite samples', () => {
  const context = vm.createContext({});
  vm.runInContext(demoSource, context);
  const status = context.createDemo().snapshot();
  const output = context.renderDemoMetrics(status);
  const rust = fs.readFileSync(path.join(__dirname, '../src/metrics.rs'), 'utf8');
  for (const [, field, help] of rust.matchAll(/gauge!\(\s*(\w+),\s*"([^"]+)"\s*\)/g)) {
    assert.ok(output.includes(`# HELP sigen_${field} ${help}\n# TYPE sigen_${field} gauge\n`), field);
    assert.ok(output.includes(`sigen_${field} ${status.readings[field]}\n`), field);
  }
  for (const [, name, help, type] of rust.matchAll(/"(sigen_\w+)",\s*"([^"]+)",\s*"(gauge|counter)"/g)) {
    if (name.startsWith('sigen_pushover_') || name === 'sigen_mqtt_connected') continue;
    assert.ok(output.includes(`# HELP ${name} ${help}\n# TYPE ${name} ${type}\n`), name);
  }
  for (const line of output.trim().split('\n')) {
    if (line.startsWith('#')) continue;
    assert.match(line, /^sigen_\w+(?:\{(?:alert|group)="[\w]+"\})? -?\d+(?:\.\d+)?$/);
  }
  assert.ok(output.endsWith('\n'));
});

test('demo UI refreshes, controls and alert toggles never fetch real endpoints', async () => {
  let requests = 0;
  const ui = dashboard('?demo=1', () => { requests++; throw new Error('Unexpected network request'); });
  assert.equal(ui.nodes['demo-banner'].hidden, false);
  assert.match(ui.nodes.connection.textContent, /DEMO/);
  assert.equal(ui.nodes['live-links'].hidden, true);
  assert.equal(ui.nodes['demo-metrics'].hidden, false);
  ui.nodes['demo-metrics-link'].listeners.click();
  assert.equal(ui.nodes['demo-metrics'].open, true);
  assert.match(ui.nodes['demo-metrics-output'].textContent, /# DEMO MODE/);
  assert.equal(ui.nodes.controls.disabled, false);
  await vm.runInContext("send({action: 'remote_ems', enabled: true})", ui.context);
  await vm.runInContext("send({action: 'ems_mode', mode: 3})", ui.context);
  await vm.runInContext("send({action: 'backup_soc', percent: 35})", ui.context);
  assert.equal(ui.nodes.backup.value, 35);
  assert.match(ui.nodes['demo-metrics-output'].textContent, /sigen_backup_soc_percent 35\n/);
  assert.match(ui.nodes['control-result'].textContent, /The demo setting has been applied/);
  ui.nodes['demo-scenario'].value = 'alarm';
  ui.nodes['demo-scenario'].listeners.change();
  assert.match(ui.nodes['alert-summary'].textContent, /^1 active alert: Battery alarm$/);
  assert.equal(ui.nodes['alert-summary'].className, 'has-active-alerts');
  assert.equal(ui.nodes['active-alerts-link'].hidden, false);
  assert.equal(ui.nodes['active-alerts-link'].textContent, '1 active alert');
  assert.equal(ui.nodes.alerts.children[2].className, 'alert-row alert-row-active');
  assert.match(ui.nodes['demo-metrics-output'].textContent, /sigen_alert_active\{alert="battery_alarm"\} 1\n/);
  await vm.runInContext("saveAlert('battery_alarm', false)", ui.context);
  assert.equal(ui.nodes['alert-summary'].textContent, 'No active alerts.');
  assert.equal(ui.nodes['alert-summary'].className, '');
  assert.equal(ui.nodes['active-alerts-link'].hidden, true);
  assert.equal(ui.nodes.alerts.children[2].className, 'alert-row');
  assert.match(ui.nodes['demo-metrics-output'].textContent, /sigen_alert_enabled\{alert="battery_alarm"\} 0\n/);
  ui.nodes['demo-scenario'].value = 'offline';
  ui.nodes['demo-scenario'].listeners.change();
  assert.equal(ui.nodes.controls.disabled, true);
  assert.match(ui.nodes['demo-metrics-output'].textContent, /sigen_up 0\n/);
  assert.doesNotMatch(ui.nodes['demo-metrics-output'].textContent, /sigen_pv_power_watts/);
  assert.doesNotMatch(ui.nodes['demo-metrics-output'].textContent, /sigen_backup_soc_percent/);
  await ui.tick();
  assert.equal(requests, 0);
});

test('normal URLs retain live status requests and do not enable demo mode', async () => {
  for (const search of ['', '?demo=0']) {
    const calls = [];
    const ui = dashboard(search, async (url, options) => {
      calls.push([url, options]);
      return {ok: true, json: async () => simulator().snapshot()};
    });
    await ui.tick();
    assert.equal(ui.nodes['demo-banner'].hidden, true);
    assert.equal(ui.nodes['demo-metrics'].hidden, true);
    assert.ok(calls.length > 0);
    assert.ok(calls.every(([url]) => url === '/api/status'));
    await vm.runInContext("send({action: 'backup_soc', percent: 35})", ui.context);
    assert.ok(calls.some(([url, options]) => url === '/api/control' && options.method === 'POST'));
    await vm.runInContext("saveAlert('battery_alarm', false)", ui.context);
    assert.ok(calls.some(([url, options]) => url === '/api/alerts' && options.method === 'POST'));
  }
});

test('polling preserves alert checkbox focus and avoids repeating unchanged announcements', async () => {
  const ui = dashboard('?demo=1', () => { throw new Error('Unexpected fetch'); });
  const row = ui.nodes.alerts.children[2];
  const checkbox = row.children[0].children[0];
  checkbox.focus();
  const summaryWrites = ui.nodes['alert-summary'].textWrites;
  const connectionWrites = ui.nodes.connection.textWrites;
  await ui.tick();
  assert.equal(ui.nodes.alerts.children[2], row);
  assert.equal(ui.document.activeElement, checkbox);
  assert.equal(checkbox.attributes['aria-describedby'], row.children[1].id);
  assert.equal(ui.nodes['alert-summary'].textWrites, summaryWrites);
  assert.equal(ui.nodes.connection.textWrites, connectionWrites);
  ui.nodes['demo-scenario'].value = 'alarm';
  ui.nodes['demo-scenario'].listeners.change();
  assert.match(row.children[1].textContent, /Active since/);
  const writes = ui.nodes['alert-summary'].textWrites;
  vm.runInContext(`showAlerts({items: latest.alerts.items.map(a => ({...a,
    active: a.id === 'inverter_fault', known: a.id !== 'inverter_fault'}))})`, ui.context);
  assert.equal(ui.nodes['alert-summary'].textWrites, writes + 1);
  assert.match(ui.nodes['alert-summary'].textContent, /Inverter fault \(last known\)/);
});

test('users can pause automatic updates and keyboard links move focus to their destinations', async () => {
  let requests = 0;
  const ui = dashboard('', async () => {
    requests++;
    return {ok: true, json: async () => simulator().snapshot()};
  });
  await ui.tick();
  ui.nodes['updates-toggle'].listeners.click();
  const pausedRequests = requests;
  await ui.tick();
  assert.equal(requests, pausedRequests);
  assert.equal(ui.nodes['updates-status'].textContent, 'Automatic updates are paused.');
  assert.equal(ui.nodes['updates-toggle'].textContent, 'Resume updates');
  ui.nodes['updates-toggle'].listeners.click();
  assert.equal(requests, pausedRequests + 1);
  await ui.tick();
  const demo = dashboard('?demo=1', () => { throw new Error('Unexpected fetch'); });
  demo.nodes['demo-metrics-link'].listeners.click();
  assert.equal(demo.document.activeElement, demo.nodes['demo-metrics-heading']);
  demo.nodes['demo-scenario'].value = 'alarm';
  demo.nodes['demo-scenario'].listeners.change();
  demo.nodes['active-alerts-link'].listeners.click();
  assert.equal(demo.document.activeElement, demo.nodes['alerts-heading']);
  demo.nodes['active-alerts-link'].focus();
  demo.nodes['demo-scenario'].value = 'solar';
  demo.nodes['demo-scenario'].listeners.change();
  assert.equal(demo.document.activeElement, demo.nodes['alerts-heading']);
});

test('control completion restores lost focus without taking focus from another control', async () => {
  let finish;
  const ui = dashboard('', (url) => {
    if (url === '/api/control') return new Promise(resolve => { finish = resolve; });
    return Promise.resolve({ok: true, json: async () => simulator().snapshot()});
  });
  await ui.tick();
  for (const movedElsewhere of [false, true]) {
    ui.nodes.remote.focus();
    const request = vm.runInContext("send({action: 'backup_soc', percent: 35})", ui.context);
    if (movedElsewhere) ui.nodes['demo-link'].focus();
    else ui.document.body.focus(); // Native disabling can move focus to the body.
    finish({ok: true, json: async () => ({controls: simulator().snapshot().controls})});
    await request;
    assert.equal(ui.document.activeElement, movedElsewhere ? ui.nodes['demo-link'] : ui.nodes.remote);
  }
});

test('unknown battery charge is hidden and offline readings are identified as last known', () => {
  const ui = dashboard('?demo=1', () => { throw new Error('Unexpected fetch'); });
  assert.equal(ui.nodes['soc-bar'].hidden, false);
  ui.nodes['demo-scenario'].value = 'offline';
  ui.nodes['demo-scenario'].listeners.change();
  assert.match(ui.nodes['soc-bar'].attributes['aria-valuetext'], /last known reading/);
  vm.runInContext('display({...latest, readings: null})', ui.context);
  assert.equal(ui.nodes['soc-bar'].hidden, true);
});

test('pending alert saves retain keyboard focus and prevent additional toggle changes', async () => {
  let finish;
  const ui = dashboard('', (url) => {
    if (url === '/api/alerts') return new Promise(resolve => { finish = resolve; });
    return Promise.resolve({ok: true, json: async () => simulator().snapshot()});
  });
  await ui.tick();
  const toggle = ui.nodes.alerts.children[2].children[0].children[0];
  toggle.focus();
  toggle.checked = false;
  const request = vm.runInContext("saveAlert('battery_alarm', false)", ui.context);
  assert.equal(ui.document.activeElement, toggle);
  assert.equal(toggle.disabled, undefined);
  assert.equal(toggle.attributes['aria-disabled'], 'true');
  toggle.checked = true;
  await vm.runInContext("saveAlert('battery_alarm', true)", ui.context);
  assert.equal(toggle.checked, false);
  finish({ok: true, json: async () => ({})});
  await request;
  assert.equal(ui.document.activeElement, toggle);
  assert.equal(toggle.attributes['aria-disabled'], undefined);
});
