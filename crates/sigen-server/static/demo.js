// Browser-only simulation. This module never contacts the server or hardware.
function createDemo() {
  let scenario = 'solar';
  const settings = {remote_ems_enabled: false, remote_ems_mode: 1, backup_soc_percent: 20};
  const started = Date.now();
  const names = {
    hardware_offline: 'Hardware offline or stale', inverter_fault: 'Inverter fault',
    battery_alarm: 'Battery alarm', grid_disconnected: 'Grid disconnected',
    control_failed: 'Control failed'
  };
  const enabled = Object.fromEntries(Object.keys(names).map(id => [id, true]));
  let lastSuccess = Math.floor(Date.now() / 1000);
  let lastReadings = null;
  let lastDetails = null;
  let pollErrors = 0;
  const since = {};
  function snapshot() {
    const now = Math.floor(Date.now() / 1000);
    const deviceDay = Math.floor((now - new Date().getTimezoneOffset() * 60) / 86400);
    const online = scenario !== 'offline';
    if (online) lastSuccess = now;
    else pollErrors++;
    const wave = Math.sin((Date.now() - started) / 12000);
    const pv = scenario === 'night' ? 0 : Math.round(5200 + wave * 350);
    const fault = scenario === 'alarm';
    const mode = settings.remote_ems_enabled ? settings.remote_ems_mode : null;
    let battery = scenario === 'night' ? -1800 : 1600;
    if (mode === 1) battery = 0;
    if (mode === 3 || mode === 4) battery = 3000;
    if (mode === 5 || mode === 6) battery = -3000;
    const soc = scenario === 'night' ? 48.2 : 73.6;
    if (battery < 0 && soc <= settings.backup_soc_percent) battery = 0;
    const conditions = {hardware_offline: !online, inverter_fault: false,
      battery_alarm: fault, grid_disconnected: false, control_failed: false};
    const status = {
      online, last_success_unix: lastSuccess,
      device_day: deviceDay,
      poll_errors: pollErrors, control_errors: 0,
      last_error: online ? null : 'Demo: the simulated Modbus connection has timed out.',
      device: {model: 'SigenStor · DEMO', serial_number: 'DEMO-0001', firmware_version: 'Simulated'},
      controls: online ? {...settings} : null,
      readings: {
        pv_power_watts: pv, grid_power_watts: 1800 + battery - pv,
        inverter_power_watts: pv - battery, battery_power_watts: battery,
        plant_power_watts: pv - battery,
        battery_soc_percent: soc, battery_soh_percent: 99.4,
        battery_temperature_celsius: 26.5 + wave * .4,
        battery_min_temperature_celsius: 25.8 + wave * .4,
        battery_max_temperature_celsius: 27.2 + wave * .4,
        battery_cell_voltage_volts: 3.32,
        battery_min_cell_voltage_volts: 3.30,
        battery_max_cell_voltage_volts: 3.34,
        battery_charge_limit_watts: 5000, battery_discharge_limit_watts: 5000,
        battery_charge_available_wh: (100 - soc) * 200,
        battery_discharge_available_wh: Math.max(0, soc - settings.backup_soc_percent) * 200,
        inverter_temperature_celsius: 38.2 + wave * .6,
        grid_frequency_hertz: 50 + wave * .02, grid_connected: true, grid_sensor_connected: true,
        alarms: [fault ? 1 : 0, 0, 0, 0, 0],
        ems_work_mode: settings.remote_ems_enabled ? 7 : 0,
        inverter_state: 'Running'
      },
      alerts: {items: Object.entries(names).map(([id, name]) => {
        const active = enabled[id] && conditions[id];
        if (!active) delete since[id];
        else since[id] ??= now;
        return {id, name, enabled: enabled[id], active,
          known: !enabled[id] || online || id === 'hardware_offline', since_unix: since[id] || null};
      })},
      pushover: null, mqtt: null
    };
    if (online) {
      const sample = value => ({value, updated_unix: now, device_day: deviceDay, last_error: null, unsupported: false});
      lastDetails = {
        pv_daily_kwh: sample(scenario === 'night' ? 22.46 : 14.82),
        load_daily_kwh: sample(11.37),
        battery_daily: sample({charge_kwh: 6.24, discharge_kwh: 4.18}),
        grid_import: sample('183456'), grid_export: sample('256789'),
        electrical: sample({output_type: 2, phase_voltage_volts: [230.4, 231.1, 229.8], phase_current_amps: [4.2, 3.8, 4.5], power_factor: 0.998}),
        grid_phases: sample({active_watts: [status.readings.grid_power_watts / 3 - 80, status.readings.grid_power_watts / 3 + 30, status.readings.grid_power_watts / 3 + 50]})
      };
    }
    status.details = lastDetails;
    if (online) lastReadings = status.readings;
    else status.readings = lastReadings;
    return status;
  }
  return {
    snapshot,
    setScenario(value) {
      if (!['solar', 'night', 'alarm', 'offline'].includes(value)) throw new Error('Unknown demo scenario');
      scenario = value;
    },
    send(command) {
      if (scenario === 'offline') throw new Error('The demo device is offline. Select a connected scenario to try the controls.');
      switch (command.action) {
        case 'remote_ems':
          if (command.enabled && !settings.remote_ems_enabled) settings.remote_ems_mode = 1;
          settings.remote_ems_enabled = command.enabled;
          break;
        case 'ems_mode':
          if (!settings.remote_ems_enabled) throw new Error('Enable remote EMS first.');
          if (!Number.isInteger(command.mode) || command.mode < 0 || command.mode > 6) throw new Error('EMS mode must be 0–6.');
          settings.remote_ems_mode = command.mode;
          break;
        case 'backup_soc':
          if (!Number.isFinite(command.percent) || command.percent < 0 || command.percent > 100) throw new Error('Backup SOC must be between 0 and 100 percent.');
          settings.backup_soc_percent = command.percent;
          break;
        default: throw new Error('Unknown demo command');
      }
      return {controls: {...settings}};
    },
    setAlert(id, value) {
      if (!Object.hasOwn(enabled, id)) throw new Error('Unknown demo alert');
      enabled[id] = value;
    }
  };
}

function renderDemoMetrics(status) {
  const lines = ['# DEMO MODE: these metrics contain simulated data. The live /metrics endpoint is unchanged.'];
  function metric(name, help, type, value) {
    lines.push(`# HELP ${name} ${help}`, `# TYPE ${name} ${type}`, `${name} ${typeof value === 'boolean' ? Number(value) : value}`);
  }
  for (const [field, help] of [
    ['enabled', 'Whether an alert is enabled.'],
    ['active', 'Whether an enabled alert is active.'],
    ['known', 'Whether the alert condition can currently be evaluated.']
  ]) {
    const name = `sigen_alert_${field}`;
    lines.push(`# HELP ${name} ${help}`, `# TYPE ${name} gauge`);
    for (const alert of status.alerts.items) lines.push(`${name}{alert="${alert.id}"} ${Number(alert[field])}`);
  }
  metric('sigen_up', 'Whether hardware readings are online and fresh.', 'gauge', status.online);
  metric('sigen_poll_errors_total', 'Failed hardware polls since server start.', 'counter', status.poll_errors);
  metric('sigen_control_errors_total', 'Failed hardware controls since server start.', 'counter', status.control_errors);
  metric('sigen_last_success_timestamp_seconds', 'Unix time of last successful poll.', 'gauge', status.last_success_unix);
  metric('sigen_sample_age_seconds', 'Age of the last successful poll.', 'gauge', Math.max(0, Math.floor(Date.now() / 1000) - status.last_success_unix));
  // Match the live exporter: stale readings and controls are omitted.
  if (status.online) {
    const d = status.details || {};
    const now = Math.floor(Date.now() / 1000);
    const current = (sample, daily = false, age = 600) => sample && !sample.unsupported && !sample.last_error && sample.updated_unix != null
      && Math.max(0, now - sample.updated_unix) <= age && (!daily || (status.device_day != null && sample.device_day === status.device_day)) ? sample.value : null;
    for (const [name, sample, help] of [
      ['sigen_pv_daily_energy_kwh', d.pv_daily_kwh, 'Inverter solar generation for the device-local day.'],
      ['sigen_load_daily_energy_kwh', d.load_daily_kwh, 'Plant load consumption for the device-local day.']
    ]) {
      const value = current(sample, true);
      if (value != null) metric(name, help, 'gauge', value);
    }
    const b = current(d.battery_daily, true);
    if (b) {
      metric('sigen_battery_daily_charge_energy_kwh', 'Inverter battery charging energy for the device-local day.', 'gauge', b.charge_kwh);
      metric('sigen_battery_daily_discharge_energy_kwh', 'Inverter battery discharging energy for the device-local day.', 'gauge', b.discharge_kwh);
    }
    for (const [name, sample, help] of [
      ['sigen_grid_import_energy_kwh_total', d.grid_import, 'Plant imported grid energy since the new statistics interface was initialized.'],
      ['sigen_grid_export_energy_kwh_total', d.grid_export, 'Plant exported grid energy since the new statistics interface was initialized.']
    ]) {
      const value = current(sample);
      if (value != null && /^\d+$/.test(value)) {
        const exact = BigInt(value);
        metric(name, help, 'counter', `${exact / 100n}.${String(exact % 100n).padStart(2, '0')}`);
      }
    }
    const phases = (name, help, values, valid) => {
      if (!valid.some(Boolean)) return;
      lines.push(`# HELP ${name} ${help}`, `# TYPE ${name} gauge`);
      ['a', 'b', 'c'].forEach((phase, i) => { if (valid[i]) lines.push(`${name}{phase="${phase}"} ${values[i]}`); });
    };
    const e = current(d.electrical, false, 120);
    if (e) {
      metric('sigen_inverter_output_type', 'Raw inverter output wiring type.', 'gauge', e.output_type);
      metric('sigen_inverter_power_factor', 'Inverter power factor.', 'gauge', e.power_factor);
      phases('sigen_inverter_phase_voltage_volts', 'Inverter phase voltage where wiring defines a phase-neutral measurement.', e.phase_voltage_volts, [0, 1, 2].map(i => e.output_type === 2 || (e.output_type === 0 && i === 0)));
      phases('sigen_inverter_phase_current_amps', 'Inverter phase current.', e.phase_current_amps, [0, 1, 2].map(i => [1, 2].includes(e.output_type) || (e.output_type === 0 && i === 0)));
    }
    const p = current(d.grid_phases, false, 120);
    if (p && status.readings.grid_sensor_connected) phases('sigen_grid_phase_power_watts', 'Plant grid sensor phase power: positive import, negative export.', p.active_watts, [true, true, true]);
    for (const [field, help] of [
      ['grid_power_watts', 'Grid power: positive import, negative export.'],
      ['plant_power_watts', 'Plant active power.'],
      ['pv_power_watts', 'Plant solar generation power.'],
      ['battery_power_watts', 'Battery power: positive charge, negative discharge.'],
      ['battery_soc_percent', 'Battery state of charge.'],
      ['battery_soh_percent', 'Battery state of health.'],
      ['battery_temperature_celsius', 'Average battery cell temperature.'],
      ['battery_min_temperature_celsius', 'Minimum battery cluster temperature.'],
      ['battery_max_temperature_celsius', 'Maximum battery cluster temperature.'],
      ['battery_cell_voltage_volts', 'Average battery cell voltage.'],
      ['battery_min_cell_voltage_volts', 'Minimum battery cell voltage.'],
      ['battery_max_cell_voltage_volts', 'Maximum battery cell voltage.'],
      ['battery_charge_limit_watts', 'Available maximum battery charging power.'],
      ['battery_discharge_limit_watts', 'Available maximum battery discharging power.'],
      ['battery_charge_available_wh', 'Available battery charging energy.'],
      ['battery_discharge_available_wh', 'Available battery discharging energy.'],
      ['inverter_power_watts', 'Inverter active power.'],
      ['inverter_temperature_celsius', 'Inverter internal temperature.'],
      ['grid_frequency_hertz', 'Grid frequency.'],
      ['ems_work_mode', 'Raw plant EMS work mode.'],
      ['grid_connected', 'Whether the plant is on grid.'],
      ['grid_sensor_connected', 'Whether the grid meter is connected.']
    ]) metric(`sigen_${field}`, help, 'gauge', status.readings[field]);
    lines.push('# HELP sigen_alarm_bits Raw alarm bitfields by group (1 through 5).', '# TYPE sigen_alarm_bits gauge');
    status.readings.alarms.forEach((bits, index) => lines.push(`sigen_alarm_bits{group="${index + 1}"} ${bits}`));
    for (const [field, help] of [
      ['remote_ems_enabled', 'Remote EMS enabled readback.'],
      ['remote_ems_mode', 'Remote EMS control mode readback.'],
      ['backup_soc_percent', 'Battery backup reserve readback.']
    ]) metric(`sigen_${field}`, help, 'gauge', status.controls[field]);
  }
  return lines.join('\n') + '\n';
}
