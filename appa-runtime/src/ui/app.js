'use strict';

let state;
let busy = false;
const params = new URLSearchParams(location.search);
const configureRequested = params.get('configure') === 'true' || params.get('setup') === 'true';
let view = configureRequested ? 'batteries' : 'overview';
const expanded = new Map();
const drafts = new Map();
const selected = new Set((new URLSearchParams(location.search).get('batteries') ?? '').split(',').filter(Boolean));
const content = document.querySelector('#content');
const notices = document.querySelector('#notice');
history.replaceState(null, '', location.pathname + location.search);

const reasons = {
  missing_configuration: 'Additional configuration required. See setup instructions.',
  missing_credential: 'Enter a token.',
  cli_not_authenticated: 'CLI is not logged in. Log in or enter a token.',
  missing_executable: 'Install the missing program, then check again.',
  invalid_credential: 'Token rejected by the provider.',
  insufficient_access: 'Token lacks the required access.',
  provider_unavailable: 'Provider unreachable. Check again later.',
  check_failed: 'Check failed or returned an invalid response.',
  check_timed_out: 'Check timed out. Check again later.',
};
const names = { github: 'GitHub', slack: 'Slack', huggingface: 'Hugging Face', databricks: 'Databricks', 'claude-code': 'Claude Code' };

function el(tag, text, cls) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (cls) node.className = cls;
  return node;
}
function status(text, kind = '') { return el('span', text, `status ${kind}`); }
function button(text, action, style = '') {
  const node = el('button', text, `action ${style}`);
  node.type = 'button';
  node.addEventListener('click', action);
  return node;
}
function notice(text, error = false) {
  notices.replaceChildren(el('div', text, `notice${error ? ' error' : ''}`));
}
async function api(path, body) {
  const response = await fetch(`/api/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) {
    throw new Error(await response.text());
  }
  return response.json();
}
async function work(action) {
  if (busy) return;
  busy = true;
  document.querySelectorAll('button').forEach(b => b.disabled = true);
  try { await action(); }
  catch (error) { notice(error.message, true); }
  finally {
    busy = false;
    document.querySelectorAll('button').forEach(b => b.disabled = false);
  }
}
function heading(title, description, action) {
  const row = el('div', undefined, 'page-heading');
  row.append(el('h1', title));
  if (action) row.append(action);
  content.append(row);
  if (description) content.append(el('p', description, 'description'));
}
function table(headers, cls = '') {
  const wrap = el('div', undefined, 'table-wrap');
  const node = el('table', undefined, cls);
  const head = el('thead'), row = el('tr');
  headers.forEach(label => { const th = el('th', label); th.scope = 'col'; row.append(th); });
  head.append(row); node.append(head); wrap.append(node);
  return { wrap, node };
}
function errors() {
  if (!state.errors.length) return;
  const section = el('div', undefined, 'errors');
  state.errors.forEach(error => section.append(el('div', error, 'notice error')));
  content.append(section);
}
function render() {
  content.replaceChildren();
  document.querySelectorAll('[data-view]').forEach(b => b.setAttribute('aria-current', b.dataset.view === view ? 'page' : 'false'));
  if (view === 'overview') overview();
  else batteries();
}
function refresh() { return work(async () => { state = await api('state'); render(); }); }
const serversOpen = new Set();
const rulesOpen = new Set();
let query = '';
let serverFilter = 'all';
const CUSTOM = 'root configuration';
const kinds = [
  { key: 'battery', label: 'Battery rules' },
  { key: 'custom', label: 'Custom rules only' },
  { key: 'unknown', label: 'Rules, source unknown' },
  { key: 'none', label: 'No rules' },
];
function namespaceOf(rule) {
  const name = rule.name ?? '';
  if (name === '*') return null;
  if (typeof rule.server === 'string') return rule.server;
  const [kind, space] = name.split('/');
  if (kind === 'mcp' && space) return space;
  if (kind === 'host' && space) return `host:${space}`;
  return 'other';
}
function serverTitle(key) {
  if (key === 'host:claude-code') return 'Claude Code built-in tools';
  if (key.startsWith('host:')) return `${key.slice(5)} built-in tools`;
  if (key === '*') return 'Any MCP server';
  if (key === 'other') return 'Other tools';
  return key;
}
function isMcp(key) { return !key.startsWith('host:') && key !== '*' && key !== 'other'; }
function sourceOf(rule) { return state.origins[rule.name] ?? null; }
function servers() {
  const map = new Map();
  const entry = key => {
    if (!map.has(key)) map.set(key, { key, title: serverTitle(key), rules: [], configured: false });
    return map.get(key);
  };
  for (const rule of state.policy?.tool ?? []) {
    const key = namespaceOf(rule);
    if (key && key !== 'appa') entry(key).rules.push(rule);
  }
  for (const name of state.configured_servers) if (name !== 'appa') entry(name).configured = true;
  for (const server of map.values()) {
    const sources = server.rules.map(sourceOf);
    server.sources = [...new Set(sources.filter(Boolean))];
    server.tools = new Set(server.rules.map(rule => rule.name.split('(')[0])).size;
    server.kind = !server.rules.length ? 'none'
      : server.sources.some(source => source !== CUSTOM) ? 'battery'
      : sources.every(Boolean) ? 'custom' : 'unknown';
  }
  return [...map.values()].sort((a, b) => Number(isMcp(a.key)) - Number(isMcp(b.key)) || a.title.localeCompare(b.title));
}
function batteryByName(name) { return state.batteries.find(b => b.name === name); }
function brokenBattery(b) { return b?.check?.status === 'needs_configuration' || b?.check?.status === 'unavailable'; }
function plural(n, one, many = `${one}s`) { return `${n} ${n === 1 ? one : many}`; }
function svg(tag, attrs) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', tag);
  Object.entries(attrs).forEach(([key, value]) => node.setAttribute(key, value));
  return node;
}
function overview() {
  heading('Policy coverage', '', button('Refresh', refresh, 'secondary'));
  const list = servers(), mcp = list.filter(s => isMcp(s.key));
  const wildcard = (state.policy?.tool ?? []).some(rule => rule.name === '*');
  const cards = el('div', undefined, 'cards');
  cards.append(coverageCard(mcp, wildcard), batteriesCard());
  content.append(cards);
  if (!Object.keys(state.origins).length && (state.policy?.tool ?? []).length) {
    content.append(el('p', 'Rule sources are unavailable: the running policy differs from the configuration on disk.', 'muted'));
  }
  serverTable(list, wildcard);
  errors();
}
function coverageCard(mcp, wildcard) {
  const card = el('section', undefined, 'card');
  const head = el('div');
  head.append(el('h2', 'MCP servers'), el('p', `What decides calls to each of your ${plural(mcp.length, 'MCP server')}.`, 'muted'));
  card.append(head);
  if (!mcp.length) { card.append(el('p', 'No MCP servers are configured or named by a rule.', 'empty')); return card; }
  const count = key => mcp.filter(s => s.kind === key).length;
  const covered = mcp.length - count('none');
  const pct = n => Math.round(n * 100 / mcp.length);
  const body = el('div', undefined, 'coverage');
  const donut = el('div', undefined, 'donut');
  const chart = svg('svg', { viewBox: '0 0 120 120', 'aria-hidden': 'true' });
  const radius = 48, circumference = 2 * Math.PI * radius;
  chart.append(svg('circle', { cx: 60, cy: 60, r: radius, class: 'track' }));
  let offset = 0;
  for (const kind of kinds) {
    const length = count(kind.key) / mcp.length * circumference;
    if (!length || kind.key === 'none') { offset += length; continue; }
    chart.append(svg('circle', { cx: 60, cy: 60, r: radius, class: `arc ${kind.key}`,
      'stroke-dasharray': `${length} ${circumference}`, 'stroke-dashoffset': -offset }));
    offset += length;
  }
  const center = el('div', undefined, 'donut-center');
  center.append(el('strong', `${pct(covered)}%`), el('span', 'have a rule'));
  donut.append(chart, center);
  donut.setAttribute('role', 'img');
  donut.setAttribute('aria-label', `${covered} of ${mcp.length} MCP servers have a rule`);
  const legend = el('ul', undefined, 'legend');
  for (const kind of kinds) {
    const n = count(kind.key);
    if (!n && kind.key === 'unknown') continue;
    const item = el('li');
    item.append(el('span', undefined, `swatch ${kind.key}`), el('span', kind.label), el('span', n, 'value'), el('span', `${pct(n)}%`, 'share'));
    legend.append(item);
  }
  body.append(donut, legend);
  card.append(body);
  if (count('none')) {
    card.append(el('p', wildcard
      ? 'Calls to tools without a rule go to the wildcard annotator, one call at a time.'
      : 'Calls to tools without a rule are refused.', 'muted'));
  }
  return card;
}
function batteriesCard() {
  const card = el('section', undefined, 'card');
  const head = el('div');
  head.append(el('h2', 'Batteries'), el('p', 'Ready-made rules for common MCP servers.', 'muted'));
  const inUse = state.batteries.filter(b => b.included || b.configured);
  const broken = inUse.filter(brokenBattery);
  const tiles = el('div', undefined, 'tiles');
  const tile = (label, value, names, kind = '') => {
    const node = el('div', undefined, `tile ${kind}`);
    node.append(el('span', label, 'tile-label'), el('strong', value));
    if (names.length) node.append(el('span', names.join(', '), 'tile-names'));
    return node;
  };
  tiles.append(
    tile('Included', inUse.length, inUse.map(b => names[b.name] ?? b.name)),
    tile('Needs setup', broken.length, broken.map(b => names[b.name] ?? b.name), broken.length ? 'alert' : ''),
    tile('In catalog', state.batteries.length, []),
  );
  const actions = el('div', undefined, 'actions');
  actions.append(button('Open batteries', () => navigate('batteries'), 'secondary'));
  card.append(head, tiles, actions);
  return card;
}
function sourceChip(source) {
  if (source === CUSTOM) return el('span', 'custom', 'chip');
  const battery = batteryByName(source);
  return brokenBattery(battery) ? el('span', `${source} · needs setup`, 'chip alert') : el('span', source, 'chip battery');
}
function shortName(name) {
  const parts = name.split('/');
  return (parts[0] === 'mcp' || parts[0] === 'host') && parts.length > 2 ? parts.slice(2).join('/') : name;
}
function contractText(rule) {
  const key = k => /^[A-Za-z0-9_-]+$/.test(k) ? k : JSON.stringify(k);
  const value = v => typeof v === 'string' ? JSON.stringify(v)
    : Array.isArray(v) ? `[${v.map(value).join(', ')}]`
    : v && typeof v === 'object' ? (Object.keys(v).length ? `{ ${Object.entries(v).map(([k, x]) => `${key(k)} = ${value(x)}`).join(', ')} }` : '{}')
    : String(v);
  const { name, ...rest } = rule;
  return ['[[policy.tool]]', `name = ${value(name)}`, ...Object.entries(rest).map(([k, v]) => `${key(k)} = ${value(v)}`)].join('\n');
}
function clickable(row, open, toggle) {
  row.tabIndex = 0;
  row.setAttribute('aria-expanded', String(open));
  row.addEventListener('click', toggle);
  row.addEventListener('keydown', event => {
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); toggle(); }
  });
}
function serverMatches(server) {
  if (serverFilter === 'none' && server.rules.length) return false;
  if (serverFilter === 'setup' && !server.sources.some(source => brokenBattery(batteryByName(source)))) return false;
  if (serverFilter === 'battery' && !server.sources.some(source => source !== CUSTOM)) return false;
  if (serverFilter === 'custom' && !server.sources.includes(CUSTOM)) return false;
  return !query || server.title.toLowerCase().includes(query) || server.rules.some(rule => rule.name.toLowerCase().includes(query));
}
function serverTable(list, wildcard) {
  content.append(el('h2', 'Servers and built-in tools'));
  const toolbar = el('div', undefined, 'toolbar');
  const search = el('input', undefined, 'search');
  search.type = 'search'; search.placeholder = 'Search servers and rules'; search.value = query;
  search.setAttribute('aria-label', 'Search servers and rules');
  const filter = el('select');
  filter.setAttribute('aria-label', 'Filter servers');
  [['all', 'All servers'], ['none', 'No rules'], ['setup', 'Battery needs setup'], ['battery', 'Battery rules'], ['custom', 'Custom rules']]
    .forEach(([value, label]) => { const option = el('option', label); option.value = value; filter.append(option); });
  filter.value = serverFilter;
  const total = el('span', undefined, 'muted');
  toolbar.append(search, filter, total);
  content.append(toolbar);
  const result = table(['Server', 'Rules from', 'Rules', ''], 'servers');
  const body = el('tbody');
  result.node.append(body);
  content.append(result.wrap);
  function draw() {
    body.replaceChildren();
    const shown = list.filter(serverMatches);
    total.textContent = `${shown.length} of ${plural(list.length, 'server')}`;
    for (const server of shown) {
      const open = serversOpen.has(server.key) || (query && server.rules.some(rule => rule.name.toLowerCase().includes(query)));
      const row = el('tr', undefined, 'server-row');
      const name = el('td');
      name.append(el('span', server.title, 'server-name'));
      if (isMcp(server.key)) name.append(el('span', server.configured ? 'Configured in Claude Code' : 'Named by rules', 'server-origin'));
      const sources = el('td');
      if (server.sources.length) sources.append(...server.sources.map(sourceChip));
      else sources.append(el('span', server.rules.length ? 'Unknown' : 'None', 'muted'));
      const rules = el('td', server.rules.length ? `${plural(server.rules.length, 'rule')} · ${plural(server.tools, 'tool')}` : 'No rules', server.rules.length ? '' : 'muted');
      const chevron = el('td', open ? '▾' : '▸', 'chevron');
      row.append(name, sources, rules, chevron);
      clickable(row, open, () => { open ? serversOpen.delete(server.key) : serversOpen.add(server.key); draw(); });
      body.append(row);
      if (open) body.append(serverDetail(server, wildcard, draw));
    }
    if (!shown.length) {
      const row = el('tr'), cell = el('td', 'No matching servers.', 'empty'); cell.colSpan = 4; row.append(cell); body.append(row);
    }
  }
  search.addEventListener('input', () => { query = search.value.trim().toLowerCase(); draw(); });
  filter.addEventListener('change', () => { serverFilter = filter.value; draw(); });
  draw();
}
function serverDetail(server, wildcard, redraw) {
  const row = el('tr', undefined, 'server-detail'), cell = el('td');
  cell.colSpan = 4;
  row.append(cell);
  if (!server.rules.length) {
    cell.append(el('p', wildcard
      ? 'No rule names this server. The wildcard annotator judges each call to its tools.'
      : 'No rule names this server. Calls to its tools are refused.', 'muted'));
    return row;
  }
  const groups = new Map();
  for (const rule of server.rules) {
    if (query && !rule.name.toLowerCase().includes(query) && !server.title.toLowerCase().includes(query)) continue;
    const source = sourceOf(rule) ?? 'unknown';
    if (!groups.has(source)) groups.set(source, []);
    groups.get(source).push(rule);
  }
  for (const [source, rules] of groups) {
    const group = el('div', undefined, 'rule-group');
    const label = source === CUSTOM ? 'Custom rules' : source === 'unknown' ? 'Source unknown' : `${names[source] ?? source} battery`;
    group.append(el('h3', `${label} · ${rules.length}`));
    const list = el('ul', undefined, 'rules');
    for (const rule of rules) {
      const id = `${source}\u0000${rule.name}`, open = rulesOpen.has(id);
      const item = el('li');
      const line = el('div', shortName(rule.name), 'rule-line');
      line.title = rule.name;
      clickable(line, open, () => { open ? rulesOpen.delete(id) : rulesOpen.add(id); redraw(); });
      item.append(line);
      if (open) item.append(el('pre', contractText(rule)));
      list.append(item);
    }
    group.append(list);
    cell.append(group);
  }
  return row;
}
function needsSetup(b) { return b.check?.status === 'needs_configuration'; }
function relevant(b) { return b.selected || b.included || b.configured || selected.has(b.name); }
function checkStatus(b) {
  if (!b.check) return status('Checking…');
  if (b.check.status === 'ready') return status('Ready', 'good');
  if (b.check.status === 'unavailable') return status('Check failed', 'bad');
  return status('Needs setup', 'warn');
}
function readinessDetail(b) {
  if (!b.check || b.check.status === 'ready') return '';
  if (b.check.reason === 'missing_executable') {
    const missing = b.dependencies.filter(d => !d.installed).map(d => d.executable);
    if (missing.length) return `Install ${missing.join(', ')}.`;
    const alternatives = b.alternatives.filter(a => !a.installed).map(a => a.executable);
    if (alternatives.length) return `Enter a token or install ${alternatives.join(', ')} and sign in.`;
  }
  if (b.check.reason === 'cli_not_authenticated' || b.check.reason === 'missing_credential') {
    const hints = b.alternatives.map(a => a.installed ? a.login_hint : `install ${a.executable}, then ${a.login_hint}`);
    if (hints.length) return `Enter a token or run ${hints.join('; ')}.`;
  }
  return reasons[b.check.reason] ?? '';
}
function configurable(b) { return b.credentials.length > 0 || (needsSetup(b) && b.alternatives.length > 0); }
function batteries() {
  heading('Batteries', 'Ready means nothing a battery needs is missing. A battery without a connection check cannot tell whether the provider accepts its token.');
  const list = state.batteries.filter(relevant);
  if (!list.length) content.append(el('p', 'No batteries in use. Include one with appa battery install <name>.', 'empty'));
  else {
    const result = table(['Battery', 'Status', '']);
    const body = el('tbody');
    list.forEach(b => {
      const row = el('tr'), name = el('td', names[b.name] ?? b.name, 'battery-name');
      name.title = b.description;
      row.append(name);
      const checkCell = el('td'); checkCell.append(checkStatus(b));
      const detail = readinessDetail(b); if (detail) checkCell.append(el('span', detail, 'check-message'));
      row.append(checkCell);
      const action = el('td');
      const open = configurable(b) && (expanded.get(b.name) ?? (configureRequested && needsSetup(b)));
      if (configurable(b)) {
        const toggle = button(open ? 'Close' : 'Configure', () => { expanded.set(b.name, !open); render(); }, 'link');
        toggle.setAttribute('aria-expanded', String(open));
        toggle.setAttribute('aria-controls', `configure-${b.name}`);
        action.append(toggle);
      }
      row.append(action); body.append(row);
      if (open) {
        const detailRow = el('tr', undefined, 'configuration-row');
        detailRow.id = `configure-${b.name}`;
        const cell = el('td'); cell.colSpan = 3;
        cell.append(configuration(b));
        detailRow.append(cell); body.append(detailRow);
      }
    });
    result.node.append(body); content.append(result.wrap);
    const actions = el('div', undefined, 'actions');
    actions.append(button('Check again', () => check(list), 'secondary')); content.append(actions);
  }
  errors();
}
function credentialField(b, c) {
  const field = el('div', undefined, 'credential'), head = el('div', undefined, 'field-heading');
  const id = `credential-${b.name}-${c.variable}`;
  const label = el('label', b.credentials.length === 1 ? 'Token' : c.variable);
  label.htmlFor = id; label.title = c.variable;
  const source = el('span', undefined, 'field-source');
  if (c.saved) source.append(button('Delete', () => work(async () => {
    state = await api('credentials', { credentials: { [c.variable]: null }, batteries: [b.name] });
    drafts.delete(c.variable); render(); notice('Saved credential deleted.');
  }), 'link danger'));
  head.append(label, source);
  const input = el('input');
  input.type = 'password'; input.id = id; input.name = c.variable;
  input.autocomplete = 'new-password'; input.spellcheck = false; input.maxLength = 16384;
  input.placeholder = c.saved ? 'Replace saved token' : c.source === 'environment' ? 'Optional saved token' : 'Enter token';
  input.dataset.credential = c.variable; input.value = drafts.get(c.variable) ?? '';
  input.addEventListener('input', () => { if (input.value) drafts.set(c.variable, input.value); else drafts.delete(c.variable); });
  field.append(head, input);
  if (c.source === 'environment') field.append(el('small', 'Environment overrides saved credentials.'));
  return field;
}
function configuration(b) {
  const form = el('form', undefined, 'battery-configuration');
  form.setAttribute('aria-label', `Configure ${names[b.name] ?? b.name}`);
  form.addEventListener('submit', event => { event.preventDefault(); save([b]); });
  b.credentials.forEach(c => form.append(credentialField(b, c)));
  if (needsSetup(b)) b.alternatives.forEach(a => {
    form.append(el('p', `${b.credentials.length ? 'Or sign in' : 'Sign in'} using ${a.executable}${a.installed ? '' : ' (install it first)'}. Run this in your terminal, then check again:`, 'requirement'), el('code', a.login_hint, 'login-hint'));
  });
  if (needsSetup(b) && b.setup) {
    const instructions = el('details', undefined, 'instructions');
    instructions.append(el('summary', 'Setup instructions'), el('p', b.setup)); form.append(instructions);
  }
  if (b.credentials.length) {
    const actions = el('div', undefined, 'actions');
    const submit = button('Save and check', () => {}); submit.type = 'submit'; actions.append(submit);
    form.append(actions);
  }
  return form;
}
function save(list) {
  return work(async () => {
    const variables = new Set(list.flatMap(b => b.credentials.map(c => c.variable)));
    const credentials = Object.fromEntries([...drafts].filter(([key]) => variables.has(key)));
    notice('Saving and checking…');
    state = await api('credentials', { credentials, batteries: list.map(b => b.name) });
    Object.keys(credentials).forEach(key => drafts.delete(key));
    list.forEach(b => { if (state.batteries.find(item => item.name === b.name)?.check?.status === 'ready') expanded.delete(b.name); });
    render();
    const failed = state.batteries.filter(b => list.some(item => item.name === b.name) && (needsSetup(b) || b.check?.status === 'unavailable')).length;
    notice(failed ? `Saved. ${failed} ${failed === 1 ? 'battery needs' : 'batteries need'} attention.` : 'Saved.');
  });
}
function check(list) {
  return work(async () => {
    notice('Checking connections…');
    state = await api('check', { batteries: list.map(b => b.name) }); render(); notices.replaceChildren();
  });
}
function navigate(next) { if (busy || !state) return; view = next; notices.replaceChildren(); render(); }
document.querySelectorAll('[data-view]').forEach(b => b.addEventListener('click', () => navigate(b.dataset.view)));
document.querySelector('.brand').addEventListener('click', event => { event.preventDefault(); navigate('overview'); });
(async () => {
  try {
    state = await api('state'); render();
    const list = state.batteries.filter(relevant);
    if (list.length) await check(list);
  } catch (error) { content.replaceChildren(el('h1', 'Connection needed'), el('p', error.message, 'description')); }
})();
setInterval(async () => {
  if (!state || busy || view !== 'overview' || content.contains(document.activeElement)) return;
  try { state = await api('state'); if (view === 'overview' && !busy && !content.contains(document.activeElement)) render(); } catch { /* Explicit actions surface errors. */ }
}, 10000);
