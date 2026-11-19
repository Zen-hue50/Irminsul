'use strict';

// Irminsul dashboard. No framework, remote assets, cookies, or background workers.
const $ = (id) => document.getElementById(id);
const esc = (v) => String(v ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const names = { overview: 'Overview', applications: 'Applications', activity: 'Activity', settings: 'Settings' };
const tones = { Productivity: 'code', Browsing: 'web', Communication: 'chat', Entertainment: 'media', Utilities: 'tools', Other: 'other', 'Desktop & excluded': 'none' };
const appIcons = new Set(['code', 'globe', 'message', 'music', 'terminal', 'folder', 'brush', 'film', 'app', 'book']);
const nums = new Intl.NumberFormat('en');
const hourMs = 3_600_000;
const minuteMs = 60_000;
const icons = (name) => `<svg class="icon" aria-hidden="true"><use href="#i-${name}"/></svg>`;
const tone = (category) => Object.hasOwn(tones, category) ? tones[category] : 'other';
const dateFrom = (iso) => new Date(`${iso}T12:00:00Z`);
const isoDate = (date) => date.toISOString().slice(0, 10);
const shiftDate = (iso, by) => { const date = dateFrom(iso); date.setUTCDate(date.getUTCDate() + by); return isoDate(date); };
const fmtDate = (iso, options) => dateFrom(iso).toLocaleDateString('en-US', { ...options, timeZone: 'UTC' });
const fullDate = (iso) => fmtDate(iso, { weekday: 'short', month: 'short', day: 'numeric', year: 'numeric' });
const shortDate = (iso) => fmtDate(iso, { month: 'short', day: 'numeric' });
const hourText = (h) => `${String(h).padStart(2, '0')}:00`;
const pct = (fraction) => `${Math.round(fraction * 100)}%`;
const match = window.matchMedia('(prefers-color-scheme: dark)');

function durationParts(ms) {
  if (!ms) return [[0, 'm']];
  if (ms < minuteMs) return [[Math.max(1, Math.floor(ms / 1000)), 's']];
  const hours = Math.floor(ms / hourMs), minutes = Math.floor(ms / minuteMs) % 60;
  return [...(hours ? [[hours, 'h']] : []), ...((minutes || !hours) ? [[minutes, 'm']] : [])];
}
const duration = (ms) => durationParts(ms).map(([n, u]) => `${nums.format(n)}${u}`).join(' ');
const bigDuration = (ms) => durationParts(ms).map(([n, u]) => `${nums.format(n)}<span class="unit">${u}</span>`).join(' ');
const plural = (n, one, many = `${one}s`) => `${nums.format(n)} ${n === 1 ? one : many}`;

function activeSection() {
  const key = location.hash.slice(1);
  return Object.hasOwn(names, key) ? key : 'overview';
}
const state = { section: activeSection(), period: 1, end: null, day: null, hour: null, search: '', sort: 'time', report: null, controller: null, timer: null, lastFetch: 0, toast: null };
const themeNames = ['system', 'light', 'dark'];
const palettes = [
  ['lavender', 'Lavender', 'The original soft violet'],
  ['sage', 'Sage', 'Muted botanical green'],
  ['rosewater', 'Rosewater', 'Soft blush and rose'],
  ['oat', 'Oat', 'Warm parchment beige'],
  ['blue-mist', 'Blue Mist', 'Quiet slate blue'],
  ['soft-clay', 'Soft Clay', 'Earthy terracotta'],
];
const paletteNames = palettes.map(([id]) => id);
const paletteKey = 'irminsul.palette';
const themeKey = 'irminsul.theme';
function storedChoice(key, allowed, fallback) {
  try {
    const value = localStorage.getItem(key);
    if (allowed.includes(value)) return value;
    if (key === themeKey) {
      // Browser-only appearance follows the upgrade at the same localhost URL.
      const priorKey = 'stilltime.theme';
      const previous = localStorage.getItem(priorKey);
      if (allowed.includes(previous)) {
        try { localStorage.setItem(key, previous); localStorage.removeItem(priorKey); } catch (_) { /* usable without writes */ }
        return previous;
      }
    }
  } catch (_) { /* storage is optional */ }
  return fallback;
}
let themeChoice = storedChoice(themeKey, themeNames, 'system');
let paletteChoice = storedChoice(paletteKey, paletteNames, 'lavender');
document.documentElement.dataset.palette = paletteChoice;
function choosePalette(value) {
  if (!paletteNames.includes(value)) return;
  paletteChoice = value;
  document.documentElement.dataset.palette = value;
  try { localStorage.setItem(paletteKey, value); } catch (_) { /* usable without storage */ }
}
function applyTheme() {
  const dark = themeChoice === 'dark' || (themeChoice === 'system' && match.matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  document.documentElement.dataset.preference = themeChoice;
  const button = $('quick-theme');
  button.innerHTML = icons(dark ? 'sun' : 'moon');
  button.setAttribute('aria-label', `Switch to ${dark ? 'light' : 'dark'} appearance`);
  button.title = button.getAttribute('aria-label');
  document.querySelectorAll('[data-action="theme"]').forEach((el) => el.setAttribute('aria-pressed', String(el.dataset.value === themeChoice)));
}
function chooseTheme(value) {
  if (!themeNames.includes(value)) return;
  themeChoice = value;
  try { localStorage.setItem(themeKey, value); } catch (_) { /* usable without storage */ }
  applyTheme();
}
match.addEventListener('change', applyTheme);
$('quick-theme').addEventListener('click', () => chooseTheme(document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'));
applyTheme();

function selection() {
  return state.section === 'activity' ? { days: 1, end: state.day } : { days: state.period, end: state.end };
}
function apiUrl(endpoint, { days, end }) {
  const args = new URLSearchParams({ days });
  if (end) args.set('end', end);
  return `/api/v1/${endpoint}?${args}`;
}
function syncNav() {
  $('crumb').textContent = names[state.section];
  document.querySelectorAll('[data-view]').forEach((link) => {
    if (link.dataset.view === state.section) link.setAttribute('aria-current', 'page');
    else link.removeAttribute('aria-current');
  });
}
const statuses = { demo: 'Demo data', tracking: 'Tracking', waiting: 'Waiting for session', paused: 'Paused', idle: 'Idle', locked: 'Locked', 'display-off': 'Displays off', error: 'Unavailable' };
function setStatus(value, detail = '') {
  const status = $('status');
  const label = statuses[value] ?? statuses.waiting;
  if (status.dataset.status !== value || $('status-text').textContent !== label) {
    status.dataset.status = value;
    $('status-text').textContent = label;
  }
  status.title = detail || label;
}
function statusFrom(r) {
  setStatus(r.demo ? 'demo' : r.session.storage_ok ? r.session.status : 'error',
    r.demo ? 'Sample data. This dashboard is not tracking a device.' :
    !r.session.storage_ok ? 'Could not write the local database.' :
    r.session.current_app ? `Currently focused: ${r.session.current_app}` : 'On this device only');
}

function rangeLabel({ start, end }) {
  if (start === end) return fullDate(end);
  const a = dateFrom(start), b = dateFrom(end);
  if (a.getUTCFullYear() !== b.getUTCFullYear()) {
    return `${fmtDate(start, { year: 'numeric', month: 'short', day: 'numeric' })} – ${fmtDate(end, { year: 'numeric', month: 'short', day: 'numeric' })}`;
  }
  return a.getUTCMonth() === b.getUTCMonth() ? `${shortDate(start)}–${b.getUTCDate()}, ${b.getUTCFullYear()}` : `${shortDate(start)} – ${shortDate(end)}, ${b.getUTCFullYear()}`;
}
function controls(r, activity = false) {
  const periodNames = [[1, r.days === 1 && r.end !== r.today ? '1 day' : 'Today'], [7, '7 days'], [30, '30 days']];
  const date = `<div class="date-nav" role="group" aria-label="Report date">
      <button class="icon-button" data-action="previous" data-key="previous" aria-label="Previous ${activity || r.days === 1 ? 'day' : 'period'}" title="Previous"${r.end <= '1900-01-01' ? ' disabled' : ''}>${icons('left')}</button>
      <span class="period-label">${icons('calendar')}${esc(rangeLabel(r))}</span>
      <button class="icon-button" data-action="next" data-key="next" aria-label="Next ${activity || r.days === 1 ? 'day' : 'period'}" title="Next"${r.end >= r.today ? ' disabled' : ''}>${icons('right')}</button>
    </div>`;
  const buttons = periodNames.map(([n, label]) => `<button data-action="period" data-value="${n}" data-key="period-${n}" aria-pressed="${r.days === n}">${label}</button>`).join('');
  return `<div class="report-controls">${date}${activity ? (r.end < r.today ? `<button class="button" data-action="latest" data-key="latest">Back to today</button>` : '') : `<div class="segment" role="group" aria-label="Report period">${buttons}</div>`}</div>`;
}
function hero(kicker, title, summary, buttons = '') {
  return `<section class="hero"><div><p class="eyebrow">${kicker}</p><h1>${title}</h1><p class="hero-copy">${summary}</p></div>${buttons}</section>`;
}
function cardHead(title, subtitle, action = '') {
  return `<header class="card-head"><div><h2>${title}</h2>${subtitle ? `<p>${subtitle}</p>` : ''}</div>${action}</header>`;
}
function metric(title, name, value, foot, highlight = false) {
  return `<section class="card metric${highlight ? ' metric-accent' : ''}"><div class="metric-top"><h2>${title}</h2><span class="metric-icon">${icons(name)}</span></div><div class="metric-value">${value}</div><div class="metric-foot">${foot}</div></section>`;
}
function metrics(r) {
  const comparison = r.days === 1 ? 'previous day' : `previous ${r.days} days`;
  let activity = 'No earlier time to compare';
  if (r.previous_total_ms && r.total_ms) {
    const delta = Math.round(100 * (r.total_ms / r.previous_total_ms - 1));
    const marker = delta > 0 ? 'up' : delta < 0 ? 'trend' : 'switch';
    activity = `<span class="trend ${delta > 0 ? 'is-higher' : delta === 0 ? 'is-flat' : ''}">${icons(marker)}${Math.abs(delta)}%</span><span>vs ${comparison}</span>`;
  }
  const categories = r.categories.filter((c) => c.name !== 'Desktop & excluded').length;
  return `<div class="metric-grid">${metric('Screen time', 'clock', bigDuration(r.total_ms), activity, true)}${metric('Apps used', 'app', nums.format(r.apps.length), categories ? `Across ${plural(categories, 'category', 'categories')}` : 'No named apps yet')}${metric('App switches', 'switch', nums.format(r.switches), r.days > 1 ? `${nums.format(Math.round(r.switches / r.days))} per day on average` : 'Between tracked apps')}</div>`;
}

function chart(r, activity = false) {
  const hourly = r.days === 1;
  const items = hourly ? r.hourly : r.daily;
  const max = Math.max(0, ...items.map((d) => d.ms));
  const goal = r.goal_minutes * minuteMs;
  const min = hourly ? 15 : 180;
  const steps = hourly ? [15, 30, 45, 60, 90, 120, 180, 240, 360, 720, 1440] : [180, 240, 360, 480, 720, 960, 1440, 2160, 2880];
  const scale = (steps.find((step) => step >= Math.max(max / minuteMs, min)) ?? (Math.ceil(max / hourMs) * 60)) * minuteMs;
  const showGoal = !hourly && goal > 0 && goal < scale;
  const ticks = [0, 1, 2, 3].map((i) => ({ at: `${i * 100 / 3}%`, label: i === 3 ? (hourly ? '0m' : '0h') : duration(scale * (1 - i / 3)) }));
  const dense = items.length > 7;
  const buttons = items.map((item, i) => {
    const height = (100 * item.ms / scale).toFixed(4);
    const selected = hourly && activity && state.hour === i;
    const timeLabel = hourly ? `${hourText(i)}–${i === 23 ? '24:00' : hourText(i + 1)}` : fullDate(item.date);
    const key = hourly ? `hour-${i}` : `day-${item.date}`;
    const time = duration(item.ms);
    return `<button type="button" class="bar" data-action="${hourly ? 'hour' : 'day'}" data-value="${hourly ? i : esc(item.date)}" data-key="${key}" style="--height:${height}%" aria-label="${esc(timeLabel)}: ${time}${!hourly ? ', view day' : ''}"${hourly && activity ? ` aria-pressed="${selected}"` : ''}${item.ms ? '' : ' data-zero'}${!hourly && item.date === r.today ? ' data-today' : ''}><span class="bar-fill" aria-hidden="true"></span><span class="bar-tip" aria-hidden="true">${esc(hourly ? hourText(i) : shortDate(item.date))} · ${time}</span></button>`;
  }).join('');
  const x = items.map((item, i) => {
    const label = hourly ? (i % 4 === 0 ? hourText(i).slice(0, 2) : '') : items.length <= 7
      ? `${fmtDate(item.date, { weekday: 'short' })}<small>${dateFrom(item.date).getUTCDate().toString().padStart(2, '0')}</small>`
      : (i % 6 === 0 || i === items.length - 1 ? dateFrom(item.date).getUTCDate() : '');
    return `<span class="chart-label"${item.date === r.today ? ' data-today' : ''}>${label}</span>`;
  }).join('');
  return `<section class="card chart-card${activity ? ' activity-chart full-card' : ''}">
      ${cardHead(activity ? 'Hourly screen time' : 'Usage over time', hourly ? 'Active minutes by local hour' : 'Active screen time by day', `<span class="chart-pill">${icons('activity')}${hourly ? 'Hourly' : 'Daily'}</span>`)}
      <div class="chart" style="--slots:${items.length}"${dense ? ' data-dense' : ''}>
        <div class="chart-axis" aria-hidden="true">${ticks.map((t) => `<span style="--at:${t.at}">${t.label}</span>`).join('')}</div>
        <div class="plot"><div class="chart-grid" aria-hidden="true">${ticks.map((t) => `<i class="gridline" style="--at:${t.at}"></i>`).join('')}</div>${showGoal ? `<i class="goal-line" title="Daily goal: ${duration(goal)}" style="--at:${((1 - goal / scale) * 100).toFixed(4)}%" aria-hidden="true"></i>` : ''}
          <div class="bars">${buttons}</div>${max === 0 ? '<div class="chart-empty"><span>No screen time for this period</span></div>' : ''}
        </div><div class="chart-labels" aria-hidden="true">${x}</div>
      </div>
      <div class="chart-key"><span><i class="key-dot"></i>Screen time</span>${showGoal ? `<span><i class="key-goal"></i>${duration(goal)} daily goal</span>` : ''}</div>
      ${activity ? `<div class="activity-summary"><span><strong>${duration(r.total_ms)}</strong> active time · <strong>${plural(r.switches, 'switch', 'switches')}</strong></span><span>${state.hour === null ? 'Select an hour to filter apps' : `<button class="button" data-action="clear-hour" data-key="clear-hour">Clear hour filter</button>`}</span></div>` : ''}
    </section>`;
}

function categoryCard(r) {
  let offset = 0;
  const paths = r.categories.map((c) => {
    const size = c.ms / r.total_ms * 100;
    const gap = r.categories.length > 1 ? Math.min(1.8, size * .24) : 0;
    const start = offset + gap / 2;
    offset += size;
    return `<circle class="donut-shard" data-tone="${tone(c.name)}" cx="64" cy="64" r="54" pathLength="100" stroke-dasharray="${(size - gap).toFixed(5)} 100" stroke-dashoffset="-${start.toFixed(5)}"/>`;
  }).join('');
  const list = r.categories.map((c) => `<li data-tone="${tone(c.name)}" title="${duration(c.ms)}"><span class="legend-label"><i class="legend-dot" aria-hidden="true"></i>${esc(c.name)}</span><strong>${pct(c.ms / r.total_ms)}</strong></li>`).join('');
  return `<section class="card category-card">${cardHead('Time by category', 'Where your active time went')}
      <div class="category-inner">
        <div class="donut" aria-hidden="true"><svg viewBox="0 0 128 128" fill="none" stroke-width="12"><circle class="donut-track" cx="64" cy="64" r="54"/><g transform="rotate(-90 64 64)">${paths}</g></svg><span class="donut-center"><strong class="donut-value">${duration(r.total_ms)}</strong><small>ACTIVE TIME</small></span></div>
        ${list ? `<ul class="legend" aria-label="Share by category">${list}</ul>` : '<p class="muted">Categories appear as you use apps.</p>'}
      </div><div class="category-foot">${icons('info')} Focused time, not background activity.</div>
    </section>`;
}

function tile(app) {
  return `<span class="app-tile" data-tone="${tone(app.category)}">${icons(appIcons.has(app.icon) ? app.icon : 'app')}</span>`;
}
function appTable(apps, total, { compact = false, byHour = false } = {}) {
  if (!apps.length) return `<div class="empty">${icons('app')} ${state.section === 'applications' && state.search ? 'No applications match this search.' : 'No app time has been recorded here yet.'}</div>`;
  return `<table class="app-table${compact || byHour ? ' app-table--compact' : ''}"><thead><tr><th scope="col">APPLICATION</th><th scope="col">CATEGORY</th><th scope="col" class="cell-num">TIME</th>${compact || byHour ? '' : '<th scope="col" class="cell-num switch-col">SWITCHES</th>'}<th scope="col">SHARE</th></tr></thead><tbody>
      ${apps.map((app) => {
        const share = total ? app.ms / total : 0;
        return `<tr data-tone="${tone(app.category)}"><td><button class="app-cell" data-action="app" data-value="${esc(app.id)}">${tile(app)}<span class="app-name">${esc(app.name)}</span></button></td><td class="app-category">${esc(app.category)}</td><td class="cell-num app-time">${duration(app.ms)}</td>${compact || byHour ? '' : `<td class="cell-num app-time switch-col">${nums.format(app.switches)}</td>`}<td><span class="share-cell"><span aria-hidden="true" class="share-track"><i style="--share:${Math.min(100, share * 100).toFixed(3)}%"></i></span>${pct(share)}</span></td></tr>`;
      }).join('')}</tbody></table>`;
}
function overviewApps(r) {
  return `<section class="card table-card">${cardHead('Top applications', 'Apps with the most foreground time', `<a class="button link" href="#applications">View all ${icons('arrow')}</a>`)}${appTable(r.apps.slice(0, 4), r.total_ms, { compact: true })}</section>`;
}
function goalCard(r) {
  const goal = r.goal_minutes * minuteMs;
  const average = r.total_ms / r.days;
  const active = r.daily.filter((d) => d.ms > 0);
  const met = active.filter((d) => d.ms <= goal).length;
  let note = 'No active days in this period.';
  if (r.days === 1 && active.length) {
    note = average <= goal ? `<strong>${duration(goal - average)}</strong> left in your goal` : `<strong>${duration(average - goal)}</strong> above your goal`;
  } else if (active.length) {
    note = `<strong>${met} of ${plural(active.length, 'active day')}</strong> within your goal`;
  }
  return `<section class="card goal-card"><i class="goal-orbit" aria-hidden="true"></i>${cardHead('Daily goal', r.days === 1 ? 'Your screen time on this day' : 'Daily average in this period', `<span class="metric-icon">${icons('target')}</span>`)}
    ${goal ? `<div class="goal-value"><strong>${duration(average)}</strong><span>of ${duration(goal)}</span></div><span class="goal-track" role="progressbar" aria-label="Screen time goal used" aria-valuenow="${Math.min(100, Math.round(average / goal * 100))}"><i style="--share:${Math.min(100, average / goal * 100).toFixed(3)}%"></i></span><p class="goal-result">${note}</p>` : '<p class="goal-result">Set a reference point for how much time you want to spend at your screen each day.</p>'}
    <a class="button link" href="#settings">${goal ? 'Edit goal' : 'Set a goal'} ${icons('arrow')}</a></section>`;
}
function exportButton(r, label = 'Export CSV') {
  return `<a class="button" href="${esc(apiUrl('export', r))}" download>${icons('download')} ${label}</a>`;
}
function footer(r) {
  return `<footer class="page-foot"><span>${icons('shield')} ${r.demo ? 'Sample data · not a live session' : 'Private by design · stored on this device'}</span><a href="#settings">${r.demo ? 'About Irminsul' : 'Tracking settings'}</a></footer>`;
}
function notices(r) {
  if (r.demo) return '';
  if (!r.session.storage_ok) return `<div class="notice danger">${icons('info')} The database could not be updated. Recent usage is only in memory.</div>`;
  if (r.session.status === 'waiting') return `<div class="notice warning">${icons('info')} Irminsul is waiting for Hyprland and Wayland idle events. No time is being recorded.</div>`;
  if (r.paused) return `<div class="notice warning">${icons('pause')} Tracking is paused. <a class="button link" href="#settings">Resume in Settings</a></div>`;
  return '';
}
function overview(r) {
  return `${hero('ATTENTION JOURNAL', 'Irminsul', 'A clearer picture of where your screen time goes.', controls(r))}${notices(r)}${metrics(r)}<div class="insight-grid">${chart(r)}${categoryCard(r)}</div><div class="insight-grid">${overviewApps(r)}${goalCard(r)}</div>${footer(r)}`;
}

function matchingApps(r) {
  const terms = state.search.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return r.apps.filter((a) => terms.every((t) => `${a.name} ${a.category} ${a.id}`.toLowerCase().includes(t)))
    .sort((a, b) => state.sort === 'name' ? a.name.localeCompare(b.name) : state.sort === 'switches' ? b.switches - a.switches || b.ms - a.ms : b.ms - a.ms || a.name.localeCompare(b.name));
}
function applications(r) {
  const apps = matchingApps(r);
  return `${hero('FOREGROUND USAGE', 'Applications.', 'See what you spent time with, and how often you switched to it.', controls(r))}${notices(r)}
    <section class="card table-card full-card">${cardHead('Application breakdown', 'Select an app for more detail.', exportButton(r))}
      <div class="toolbar"><div class="toolbar-controls"><label class="search">${icons('search')}<input type="search" id="app-search" aria-label="Search applications" placeholder="Find an app…" value="${esc(state.search)}"></label><select id="app-sort" class="select" aria-label="Sort applications"><option value="time"${state.sort === 'time' ? ' selected' : ''}>Most time</option><option value="switches"${state.sort === 'switches' ? ' selected' : ''}>Most switches</option><option value="name"${state.sort === 'name' ? ' selected' : ''}>Name A–Z</option></select></div><span class="table-count" id="app-count" aria-live="polite">${plural(apps.length, 'app')}</span></div>
      <div id="app-results">${appTable(apps, r.total_ms)}</div>
    </section>${footer(r)}`;
}
function redrawApps() {
  if (!$('app-results') || !state.report) return;
  const apps = matchingApps(state.report);
  $('app-count').textContent = plural(apps.length, 'app');
  $('app-results').innerHTML = appTable(apps, state.report.total_ms);
}
function activity(r) {
  const byHour = state.hour !== null;
  const apps = byHour ? r.apps.map((a) => ({ ...a, ms: a.hourly[state.hour] })).filter((a) => a.ms).sort((a, b) => b.ms - a.ms) : r.apps;
  const total = byHour ? r.hourly[state.hour].ms : r.total_ms;
  const what = byHour ? `${hourText(state.hour)}–${state.hour === 23 ? '24:00' : hourText(state.hour + 1)}` : fullDate(r.end);
  return `${hero('A CLOSER LOOK', 'Activity by hour.', 'Explore the rhythm of a day, one hour at a time.', controls(r, true))}${notices(r)}${chart(r, true)}
    <section class="card table-card full-card">${cardHead(byHour ? 'Apps this hour' : 'Apps this day', esc(what), exportButton(r, byHour ? 'Export day CSV' : 'Export CSV'))}${appTable(apps, total, { byHour })}</section>${footer(r)}`;
}
function palettePicker() {
  const options = palettes.map(([id, name, description]) => `
    <label class="palette-option" data-option="${id}">
      <input class="palette-radio" type="radio" name="palette" value="${id}" aria-label="${name}" aria-describedby="palette-${id}-description"${paletteChoice === id ? ' checked' : ''}>
      <span class="palette-tile">
        <span class="palette-swatch" aria-hidden="true"><span class="swatch-rail"></span><span class="swatch-plot"><i></i><i></i><i></i><i></i></span></span>
        <span class="palette-caption"><strong>${name}</strong><small id="palette-${id}-description">${description}</small></span>
        <span class="palette-mark" aria-hidden="true">${icons('check')}</span>
      </span>
    </label>`).join('');
  return `<section class="card settings-card palette-card">${cardHead('Color palette', 'Choose a palette for your dashboard. Each works with light and dark appearance.', `<span class="metric-icon">${icons('palette')}</span>`)}<div class="palette-grid" role="radiogroup" aria-label="Color palette">${options}</div></section>`;
}
function settings(r) {
  const idle = r.idle_seconds ? `${duration(r.idle_seconds * 1_000)} threshold` : 'Disabled';
  const retention = r.retention_days ? plural(r.retention_days, 'day') : 'No automatic deletion';
  const sources = { wayland: 'Wayland events', snapshot: 'Hyprland snapshots', unavailable: 'Not available' };
  const idleModes = { 'input-only': 'Input only', 'inhibitor-aware': 'Respects inhibitors', disabled: 'Disabled', unavailable: 'Not available' };
  return `${hero('MAKE IT YOURS', 'Settings.', 'Choose how Irminsul looks and set a daily screen time goal.')}${notices(r)}${palettePicker()}
    <div class="settings-grid">
      <section class="card settings-card">${cardHead('Appearance', 'Light, dark, or your system setting.', `<span class="metric-icon">${icons('sun')}</span>`)}<div class="settings-row"><div class="segment" role="group" aria-label="Appearance">${themeNames.map((name) => `<button data-action="theme" data-value="${name}" data-key="theme-${name}" aria-pressed="${themeChoice === name}">${name[0].toUpperCase()}${name.slice(1)}</button>`).join('')}</div></div></section>
      <section class="card settings-card">${cardHead('Daily screen time goal', 'A reference for your reports, not an app blocker.', `<span class="metric-icon">${icons('target')}</span>`)}<div class="settings-row"><form id="goal-form" class="goal-form"><label for="goal">Minutes per day</label><input id="goal" class="number" name="goal" type="number" min="0" max="1440" required value="${r.goal_minutes}"><button class="button primary" type="submit" data-key="goal-save">Save</button></form><p>0 turns the goal off.</p></div></section>
      <section class="card settings-card">${cardHead('Tracking', r.demo ? 'This preview uses sample history. No device is being tracked.' : 'Pause collection without deleting your history.', `<span class="metric-icon">${icons('shield')}</span>`)}<div class="settings-row"><span class="badge">${r.demo ? 'Demo mode' : esc(statuses[r.session.status] || 'Waiting')}</span><button class="button switch-button" data-action="pause" data-key="pause">${icons(r.paused ? 'play' : 'pause')}${r.paused ? 'Resume tracking' : 'Pause tracking'}</button></div></section>
      <section class="card settings-card">${cardHead('Local configuration', 'Manage the daemon and dashboard files from the terminal.', `<span class="metric-icon">${icons('terminal')}</span>`)}<dl class="technical"><div><dt>Idle timeout</dt><dd>${idle}</dd></div><div><dt>History retention</dt><dd>${retention}</dd></div>${!r.demo ? `<div><dt>Idle signal</dt><dd>${idleModes[r.session.idle_detection] ?? 'Not available'}</dd></div><div><dt>Lock detection</dt><dd>${sources[r.session.lock_source] ?? 'Not available'}</dd></div><div><dt>Display detection</dt><dd>${sources[r.session.display_source] ?? 'Not available'}</dd></div>` : ''}</dl><p><code class="inline-code">irminsul theme init</code> copies editable HTML, CSS and JavaScript. <code class="inline-code">irminsul paths</code> shows their location.</p></section>
    </div>${footer(r)}`;
}

function showApp(id, opener) {
  const r = state.report;
  const app = r?.apps.find((a) => a.id === id);
  if (!app) return;
  const hourly = r.days === 1;
  const values = hourly ? app.hourly : app.daily;
  const max = Math.max(1, ...values);
  const bars = values.map((ms, i) => `<span style="--height:${(ms / max * 100).toFixed(2)}%" title="${hourly ? hourText(i) : esc(shortDate(r.daily[i].date))}: ${duration(ms)}"></span>`).join('');
  const sources = { 'built-in': 'Irminsul’s app catalog', 'desktop-entry': 'Linux desktop launcher', config: 'Your Irminsul configuration' };
  const source = Object.hasOwn(sources, app.category_source) ? sources[app.category_source] : '';
  const categoryNote = app.category_source === 'unclassified'
    ? 'No reliable category found. You can set one for this class in <code>config.toml</code>.'
    : source ? `Category source: ${source}` : '';
  $('dialog-content').innerHTML = `<div class="dialog-heading">${tile(app)}<div><h2 id="dialog-title">${esc(app.name)}</h2><p>${esc(app.category)} · <code>${esc(app.id)}</code></p></div></div>
    <div class="dialog-stats"><div><strong>${duration(app.ms)}</strong>Active screen time</div><div><strong>${nums.format(app.switches)}</strong>Switches into this app</div></div><p class="muted">${esc(rangeLabel(r))} · ${hourly ? 'By hour' : 'By day'}</p><div class="dialog-spark" style="--slots:${values.length}" aria-hidden="true">${bars}</div>${categoryNote ? `<p class="category-note">${icons('info')}<span>${categoryNote}</span></p>` : ''}`;
  const dialog = $('app-dialog');
  if (!dialog.open) {
    dialog.showModal();
    dialog.addEventListener('close', () => { if (opener.isConnected) opener.focus(); }, { once: true });
  }
}

function render() {
  const r = state.report;
  if (!r) return;
  const active = document.activeElement?.dataset?.key;
  const views = { overview, applications, activity, settings };
  $('main').innerHTML = views[state.section](r);
  $('main').setAttribute('aria-busy', 'false');
  statusFrom(r);
  if (active) {
    const same = $('main').querySelector(`[data-key="${CSS.escape(active)}"]`);
    const alternate = active === 'next' ? 'previous' : active === 'previous' ? 'next' : null;
    const usable = same && !same.disabled ? same : alternate ? $('main').querySelector(`[data-key="${alternate}"]:not(:disabled)`) : null;
    usable?.focus({ preventScroll: true });
  }
}
function schedule() {
  window.clearTimeout(state.timer);
  if (!document.hidden && !selection().end) state.timer = window.setTimeout(() => refresh(true), 60_000);
}
async function refresh(automatic = false) {
  if (automatic && (document.hidden || $('app-dialog').open || document.activeElement?.closest('form, .toolbar, .bars'))) { schedule(); return; }
  state.controller?.abort();
  const ctrl = new AbortController();
  state.controller = ctrl;
  if (!automatic) $('main').setAttribute('aria-busy', 'true');
  try {
    const result = await fetch(apiUrl('report', selection()), { signal: ctrl.signal, cache: 'no-store' });
    if (!result.ok) throw new Error((await result.json().catch(() => ({}))).error || `HTTP ${result.status}`);
    const report = await result.json();
    if (ctrl.signal.aborted) return;
    state.report = report;
    state.lastFetch = Date.now();
    render();
  } catch (e) {
    if (ctrl.signal.aborted) return;
    setStatus('error', 'Could not load the local report');
    $('main').setAttribute('aria-busy', 'false');
    if (!automatic || !state.report) $('main').innerHTML = `${hero('LOCAL REPORT', 'Could not load the report.', 'Check that the Irminsul daemon is running.')}
      <section class="card inside"><p>${esc(e.message)}</p><button class="button primary" data-action="retry" data-key="retry">Try again</button></section>`;
  } finally {
    if (!ctrl.signal.aborted) schedule();
  }
}
function notify(text) {
  const toast = $('toast');
  toast.textContent = text;
  toast.hidden = false;
  window.clearTimeout(state.toast);
  state.toast = window.setTimeout(() => { toast.hidden = true; }, 4200);
}
async function preference(payload, button, message) {
  if (button) button.disabled = true;
  try {
    const response = await fetch('/api/v1/preferences', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(payload) });
    if (!response.ok) throw new Error((await response.json().catch(() => ({}))).error || 'Could not save the preference.');
    notify(message);
    await refresh();
  } catch (e) { notify(e.message); }
  finally { if (button) button.disabled = false; }
}
function openDay(day, hour = null) {
  state.day = day === state.report.today ? null : day;
  state.hour = hour;
  if (state.section === 'activity') refresh();
  else location.hash = 'activity';
}
function movePeriod(direction) {
  const r = state.report;
  if (!r) return;
  const { days, end } = selection();
  const date = shiftDate(end ?? r.end, direction * days);
  const newEnd = date >= r.today ? null : date < '1900-01-01' ? '1900-01-01' : date;
  if (state.section === 'activity') { state.day = newEnd; state.hour = null; }
  else state.end = newEnd;
  refresh();
}

document.addEventListener('click', (e) => {
  const button = e.target.closest('[data-action]');
  if (!button || button.disabled) return;
  const { action, value } = button.dataset;
  const r = state.report;
  if (action === 'retry') { refresh(); return; }
  if (action === 'dialog-close') { $('app-dialog').close(); return; }
  if (!r) return;
  switch (action) {
    case 'period': state.period = Number(value); if (state.period === 1) state.end = null; refresh(); break;
    case 'previous': movePeriod(-1); break;
    case 'next': movePeriod(1); break;
    case 'latest': state.day = null; state.hour = null; refresh(); break;
    case 'day': openDay(value); break;
    case 'hour': {
      const hour = Number(value);
      if (state.section !== 'activity') openDay(r.end, hour);
      else { state.hour = state.hour === hour ? null : hour; render(); }
      break;
    }
    case 'clear-hour': state.hour = null; render(); $('main').querySelector('.bar')?.focus({ preventScroll: true }); break;
    case 'app': showApp(value, button); break;
    case 'theme': chooseTheme(value); break;
    case 'pause': preference({ paused: !r.paused }, button, r.demo ? 'Saved for this demo session' : r.paused ? 'Tracking resumed' : 'Tracking paused'); break;
  }
});
document.addEventListener('input', (e) => { if (e.target.id === 'app-search') { state.search = e.target.value; redrawApps(); } });
document.addEventListener('change', (e) => {
  if (e.target.id === 'app-sort') { state.sort = e.target.value; redrawApps(); }
  if (e.target.classList.contains('palette-radio') && e.target.checked) choosePalette(e.target.value);
});
document.addEventListener('submit', (e) => {
  if (e.target.id !== 'goal-form') return;
  e.preventDefault();
  const goal = $('goal').valueAsNumber;
  if (Number.isInteger(goal) && goal >= 0 && goal <= 1440) preference({ goal_minutes: goal }, e.submitter, 'Daily goal saved');
});
window.addEventListener('hashchange', () => {
  const next = activeSection();
  if (next === state.section) return;
  state.section = next;
  if ($('app-dialog').open) $('app-dialog').close();
  syncNav();
  window.scrollTo(0, 0);
  refresh();
});
document.addEventListener('visibilitychange', () => {
  if (!document.hidden && !selection().end && Date.now() - state.lastFetch >= 45_000) refresh(true);
  else schedule();
});
document.querySelector('.skip')?.addEventListener('click', (e) => {
  e.preventDefault();
  $('main').focus();
});
syncNav();
refresh();
