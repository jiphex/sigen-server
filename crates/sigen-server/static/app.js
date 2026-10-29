const $ = id => document.getElementById(id);
const demo = new URLSearchParams(window.location.search).get('demo') === '1' ? createDemo() : null;
let busy = false, latest = null, savingAlert = false, updatesPaused = false;
let alertSignature = '';
const emsNames = {0:'Maximum self-consumption',1:'AI',2:'Time of use',5:'Full feed-in',7:'Remote EMS',9:'Custom'};
const fmt = (n, unit, digits=1) => `${Number(n).toFixed(digits)} ${unit}`;
function setText(id, text) {
  if ($(id).textContent !== text) $(id).textContent = text;
}
function showError(id, message) {
  // Keep alert regions mounted, and announce only changes to their message.
  setText(id, message || '');
}
function restoreFocus(target) {
  if (target?.isConnected && !target.disabled && !target.closest('fieldset:disabled')
      && document.activeElement === document.body) target.focus({preventScroll: true});
}
function controls(c) {
  if (!c || busy) return;
  // Keep edits intact while the user is interacting with a form.
  if (!document.activeElement.closest('form')) {
    $('remote').value = String(c.remote_ems_enabled);
    $('mode').value = String(c.remote_ems_mode);
    $('backup').value = c.backup_soc_percent;
  }
  $('mode-form').querySelector('button').disabled = !c.remote_ems_enabled;
}
function display(s) {
  latest = s;
  if (demo) $('demo-metrics-output').textContent = renderDemoMetrics(s);
  showAlerts(s.alerts);
  $('pushover-status').textContent = s.pushover ? `Pushover enabled · ${s.pushover.sent_total} sent${s.pushover.last_error ? ' · ' + s.pushover.last_error : ''}` : 'Pushover disabled';
  $('mqtt-status').textContent = s.mqtt ? `MQTT ${s.mqtt.connected ? 'connected' : 'disconnected'} · ${s.mqtt.topic_prefix}${s.mqtt.last_error ? ' · ' + s.mqtt.last_error : ''}${s.mqtt.last_command_error ? ' · ' + s.mqtt.last_command_error : ''}` : 'MQTT disabled';
  setText('connection', s.online ? (demo ? 'Connected · DEMO' : 'Connected') : 'Offline / starting');
  $('connection').className = s.online ? 'online' : '';
  document.body.classList.toggle('stale', !s.online);
  $('device').textContent = s.device ? `${s.device.model} · ${s.device.serial_number} · ${s.device.firmware_version}` : `${s.target} · inverter ${s.unit}`;
  showError('error', s.last_error);
  const age = s.last_success_unix ? Math.max(0, Math.floor(Date.now()/1000)-s.last_success_unix) : null;
  $('freshness').textContent = age === null ? 'Waiting for data…' : `${s.online ? 'Updated' : 'Stale data from'} ${age}s ago.`;
  $('controls').disabled = busy || !s.online || !s.controls;
  controls(s.controls);
  if (s.online && s.controls && $('control-result').textContent === 'Waiting for data…') {
    $('control-result').textContent = 'Plant controls are ready.';
  }
  const r = s.readings;
  $('soc-bar').hidden = !r;
  if (!r) return;
  $('solar').textContent = fmt(r.pv_power_watts/1000,'kW',2);
  $('grid').textContent = fmt(Math.abs(r.grid_power_watts)/1000,'kW',2);
  $('grid-direction').textContent = r.grid_power_watts >= 0 ? 'Importing from grid' : 'Exporting to grid';
  $('soc').textContent = fmt(r.battery_soc_percent,'%');
  $('soc-bar').value = r.battery_soc_percent;
  $('soc-bar').setAttribute('aria-valuetext', `${fmt(r.battery_soc_percent, '%')}${s.online ? '' : ' (last known reading)'}`);
  $('battery-direction').textContent = `${r.battery_power_watts >= 0 ? 'Charging' : 'Discharging'} · ${fmt(Math.abs(r.battery_power_watts)/1000,'kW',2)}`;
  $('inverter').textContent = fmt(r.inverter_power_watts/1000,'kW',2);
  $('running').textContent = r.inverter_state;
  $('battery-temp').textContent = fmt(r.battery_temperature_celsius,'°C');
  $('soh').textContent = fmt(r.battery_soh_percent,'%');
  $('inverter-temp').textContent = fmt(r.inverter_temperature_celsius,'°C');
  $('frequency').textContent = fmt(r.grid_frequency_hertz,'Hz',2);
  $('grid-state').textContent = r.grid_connected ? 'On grid' : 'Off grid';
  $('alarms').textContent = r.alarms.every(n=>n===0) ? 'None' : r.alarms.map(n=>`0x${n.toString(16).padStart(4,'0')}`).join(' · ');
  $('ems').textContent = emsNames[r.ems_work_mode] ?? `Unknown (${r.ems_work_mode})`;
}
async function refresh() {
  if (demo) { display(demo.snapshot()); return; }
  try {
    const response = await fetch('/api/status', {cache:'no-store'});
    if (!response.ok) throw new Error(`Status request failed (${response.status})`);
    display(await response.json());
  } catch(e) {
    setText('connection', 'Server unreachable');
    $('connection').className = '';
    $('controls').disabled = true;
    document.body.classList.add('stale');
    showError('error', e.message);
    if (latest?.readings) $('soc-bar').setAttribute('aria-valuetext', `${fmt(latest.readings.battery_soc_percent, '%')} (last known reading)`);
  }
}
async function send(command) {
  if (busy) return;
  const focused = document.activeElement;
  busy = true; $('controls').disabled = true;
  $('control-result').className = '';
  $('control-result').textContent = 'The setting is being applied…';
  try {
    let result;
    if (demo) {
      result = demo.send(command);
    } else {
      const response = await fetch('/api/control', {method:'POST',headers:{'Content-Type':'application/json','X-Sigen-Control':'1'},body:JSON.stringify(command)});
      result = await response.json();
      if (!response.ok) throw new Error(result.error || `Control failed (${response.status})`);
    }
    $('control-result').textContent = demo ? 'The demo setting has been applied. No hardware command was sent.' : 'The device has confirmed the setting.';
    busy = false;
    if(latest) latest.controls = result.controls;
    controls(result.controls);
  } catch(e) {
    $('control-result').textContent = e.message;
    $('control-result').className = 'failed';
  } finally { busy = false; await refresh(); restoreFocus(focused); }
}
$('remote-form').addEventListener('submit',e=>{e.preventDefault();send({action:'remote_ems',enabled:$('remote').value==='true'});});
$('mode-form').addEventListener('submit',e=>{e.preventDefault();send({action:'ems_mode',mode:Number($('mode').value)});});
$('backup-form').addEventListener('submit',e=>{e.preventDefault();send({action:'backup_soc',percent:Number($('backup').value)});});
$('updates-toggle').addEventListener('click', () => {
  updatesPaused = !updatesPaused;
  setText('updates-toggle', updatesPaused ? 'Resume updates' : 'Pause updates');
  setText('updates-status', updatesPaused ? 'Automatic updates are paused.' : 'Automatic updates run every 3 seconds.');
  if (!updatesPaused) refresh();
});
$('active-alerts-link').addEventListener('click', () => { $('alerts-heading').focus(); });
if (demo) {
  document.title = 'Sigen status · DEMO';
  $('demo-banner').hidden = false;
  $('demo-metrics').hidden = false;
  $('demo-metrics-link').addEventListener('click', () => {
    $('demo-metrics').open = true;
    $('demo-metrics-heading').focus();
  });
  $('demo-link').hidden = true;
  $('demo-scenario').addEventListener('change', () => {
    demo.setScenario($('demo-scenario').value);
    refresh();
  });
  // API and metrics links refer to the real server, never the simulation.
  $('live-links').hidden = true;
}
refresh();
setInterval(() => { if (!updatesPaused) return refresh(); }, 3000);

function alertStatusIcon(status) {
  const paths = {
    off: 'M9 9l6 6m0-6-6 6M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0',
    active: 'M10.3 3.9 2.1 18.1A2 2 0 0 0 3.8 21h16.4a2 2 0 0 0 1.7-2.9L13.7 3.9a2 2 0 0 0-3.4 0ZM12 9v4m0 4h.01',
    ok: 'm8 12 3 3 5-6M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0',
    waiting: 'M12 6v6l4 2M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0'
  };
  const ns = 'http://www.w3.org/2000/svg';
  const icon = document.createElementNS(ns, 'svg');
  for (const [name, value] of Object.entries({
    class: `alert-icon alert-icon-${status}`, viewBox: '0 0 24 24',
    width: '16', height: '16', fill: 'none', stroke: 'currentColor',
    'stroke-width': '1.75', 'stroke-linecap': 'round', 'stroke-linejoin': 'round',
    'aria-hidden': 'true', focusable: 'false'
  })) icon.setAttribute(name, value);
  const path = document.createElementNS(ns, 'path');
  path.setAttribute('d', paths[status]);
  icon.append(path);
  return icon;
}
function showAlerts(alerts) {
  if (!alerts || savingAlert) return;
  const active = alerts.items.filter(a => a.active);
  const signature = JSON.stringify(active.map(a => [a.id, a.name, a.known]));
  if (signature !== alertSignature) {
    $('alert-summary').textContent = active.length ? `${active.length} active alert${active.length === 1 ? '' : 's'}` : 'No active alerts.';
    $('alert-summary').className = active.length ? 'has-active-alerts' : '';
    if (active.length) {
      $('alert-summary').prepend(alertStatusIcon('active'));
      const names = document.createElement('span');
      names.className = 'sr-only';
      names.textContent = ': ' + active.map(a => `${a.name}${a.known ? '' : ' (last known)'}`).join('; ');
      $('alert-summary').append(names);
    }
    alertSignature = signature;
  }
  // Do not remove a focused navigation target when its alert resolves.
  if (!active.length && document.activeElement === $('active-alerts-link')) $('alerts-heading').focus();
  $('active-alerts-link').hidden = active.length === 0;
  setText('active-alerts-link', `${active.length} active alert${active.length === 1 ? '' : 's'}`);
  const existing = new Map(Array.from($('alerts').children, row => [row.dataset.alertId, row]));
  for (const alert of alerts.items) {
    let row = existing.get(alert.id);
    if (!row) {
      row = document.createElement('div');
      row.dataset.alertId = alert.id;
      const label = document.createElement('label');
      const toggle = document.createElement('input');
      toggle.type = 'checkbox';
      toggle.setAttribute('aria-describedby', `alert-state-${alert.id}`);
      toggle.addEventListener('change', () => saveAlert(alert.id, toggle.checked));
      label.append(toggle, document.createTextNode(alert.name));
      const state = document.createElement('span');
      state.id = `alert-state-${alert.id}`;
      row.append(label, state);
      $('alerts').append(row);
    }
    existing.delete(alert.id);
    row.className = alert.active ? 'alert-row alert-row-active' : 'alert-row';
    row.children[0].children[0].checked = alert.enabled;
    const state = row.children[1];
    const text = !alert.enabled ? 'Off' : alert.active ? `Active${alert.known ? '' : ' (last known)'}` : alert.known ? 'OK' : 'Waiting for data';
    const since = alert.since_unix ? ` since ${new Date(alert.since_unix * 1000).toLocaleString()}` : '';
    state.title = since.trim();
    const iconStatus = !alert.enabled ? 'off' : alert.active ? 'active' : alert.known ? 'ok' : 'waiting';
    if (state.dataset.status !== text + since) {
      state.textContent = text;
      state.prepend(alertStatusIcon(iconStatus));
      if (since) {
        const timestamp = document.createElement('span');
        timestamp.className = 'sr-only';
        timestamp.textContent = since;
        state.append(timestamp);
      }
      state.dataset.status = text + since;
    }
  }
  for (const row of existing.values()) row.remove();
}
async function saveAlert(id, enabled) {
  if (savingAlert) {
    const toggle = Array.from($('alerts').children).find(row => row.dataset.alertId === id)?.children[0].children[0];
    if (toggle) toggle.checked = toggle.dataset.pendingChecked === 'true';
    return;
  }
  savingAlert = true;
  $('alerts').setAttribute('aria-busy', 'true');
  $('alerts').querySelectorAll('input').forEach(input => {
    input.dataset.pendingChecked = String(input.checked);
    input.setAttribute('aria-disabled', 'true');
  });
  showError('alert-error', '');
  try {
    if (demo) {
      demo.setAlert(id, enabled);
    } else {
      const response = await fetch('/api/alerts', {method:'POST', headers:{'Content-Type':'application/json','X-Sigen-Control':'1'}, body:JSON.stringify({id,enabled})});
      const result = await response.json();
      if (!response.ok) throw new Error(result.error || 'The server could not save the alert settings.');
    }
  } catch (error) {
    showError('alert-error', error.message);
  } finally {
    savingAlert = false;
    $('alerts').setAttribute('aria-busy', 'false');
    $('alerts').querySelectorAll('input').forEach(input => input.removeAttribute('aria-disabled'));
    await refresh();
  }
}
