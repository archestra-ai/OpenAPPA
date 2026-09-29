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

// Each check reason as a badge and one sentence that says what to do next.
// `{name}` is the battery's display name.
const reasons = {
  missing_configuration: ['Needs setup', 'Setup is not complete. Follow the setup steps.'],
  missing_credential: ['Needs a token', 'Add a token to connect {name}.'],
  cli_not_authenticated: ['Not signed in', 'Sign in with the CLI, or add a token.'],
  missing_executable: ['Program missing', 'Install the missing program, then check again.'],
  invalid_credential: ['Token rejected', '{name} did not accept the token. The token can be expired, revoked, or copied incompletely. Add a new token.'],
  insufficient_access: ['Missing permissions', 'The token works, but it cannot read all that the battery needs. Compare its scopes with the setup steps.'],
  provider_unavailable: ['Cannot reach {name}', 'APPA cannot connect to {name}. Check your network or VPN, then check again.'],
  check_failed: ['Check error', 'The battery check stopped with an error, or gave an answer that APPA cannot read. Check again.'],
  check_timed_out: ['Check timed out', '{name} did not answer in 15 seconds. Check again later.'],
};
const names = {
  github: 'GitHub', slack: 'Slack', huggingface: 'Hugging Face', databricks: 'Databricks', 'claude-code': 'Claude Code',
  grain: 'Grain', linear: 'Linear', notion: 'Notion', sentry: 'Sentry', posthog: 'PostHog', pagerduty: 'PagerDuty',
  launchdarkly: 'LaunchDarkly', cloudflare: 'Cloudflare', monday: 'monday.com', archestra: 'Archestra', jev: 'Jev',
  xmemory: 'xmemory', 'google-workspace': 'Google Workspace', 'microsoft-learn': 'Microsoft Learn',
};
function displayName(name) { return names[name] ?? name; }
// Manifest text: `code` spans and bare https links, nothing else.
function rich(text, tag = 'span', cls) {
  const node = el(tag, undefined, cls);
  text.split(/(`[^`]+`)/).forEach(part => {
    if (part.startsWith('`') && part.endsWith('`') && part.length > 1) { node.append(el('code', part.slice(1, -1))); return; }
    part.split(/(https:\/\/[^\s)]+[^\s).,;:])/).forEach(piece => {
      if (!piece.startsWith('https://')) { if (piece) node.append(piece); return; }
      const link = el('a', piece.replace(/^https:\/\//, ''));
      link.href = piece; link.target = '_blank'; link.rel = 'noopener noreferrer';
      node.append(link);
    });
  });
  return node;
}

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
  state.errors.forEach(error => section.append(rich(error, 'div', 'notice error')));
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
  { key: 'custom', label: 'Your rules only' },
  { key: 'unknown', label: 'Source unknown' },
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
    if (!map.has(key)) map.set(key, { key, title: serverTitle(key), rules: [] });
    return map.get(key);
  };
  for (const rule of state.policy?.tool ?? []) {
    const key = namespaceOf(rule);
    if (key && key !== 'appa') entry(key).rules.push(rule);
  }
  for (const server of map.values()) {
    const sources = server.rules.map(sourceOf);
    server.sources = [...new Set(sources.filter(Boolean))];
    server.tools = new Set(server.rules.map(rule => rule.name.split('(')[0])).size;
    server.kind = server.sources.some(source => source !== CUSTOM) ? 'battery'
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
  heading('Overview', '', button('Refresh', refresh, 'secondary'));
  const intro = el('p', 'What your current OpenAPPA configuration protects. ', 'description');
  if (state.config) { intro.append('Read from '); intro.append(el('code', state.config)); intro.append('.'); }
  content.append(intro);
  errors();
  const list = servers();
  attention();
  content.append(summary(list));
  serverTable(list);
}
// One call to action per included battery that cannot work yet, with the reason to fix it.
function attention() {
  const broken = state.batteries.filter(b => b.configured && brokenBattery(b));
  if (!broken.length) return;
  const section = el('section', undefined, 'attention');
  section.setAttribute('aria-label', 'Batteries to set up');
  for (const b of broken) {
    const info = statusInfo(b);
    const card = el('div', undefined, 'attention-card');
    const text = el('div');
    const title = el('p', undefined, 'attention-title');
    title.append(el('strong', displayName(b.name)), ` · ${info.label.toLowerCase()}`);
    text.append(title);
    if (b.benefit) text.append(rich(b.benefit, 'p', 'attention-benefit'));
    card.append(text, button(`Set up ${displayName(b.name)}`, () => { expanded.set(b.name, true); navigate('batteries'); }));
    section.append(card);
  }
  content.append(section);
}
function summary(list) {
  const mcp = list.filter(s => isMcp(s.key)), hosts = list.filter(s => s.key.startsWith('host:'));
  const card = el('section', undefined, 'summary');
  card.setAttribute('aria-label', 'Summary');

  const covered = el('div', undefined, 'fact');
  covered.append(el('span', 'Protected', 'fact-label'));
  const big = el('p', undefined, 'fact-value');
  big.append(el('strong', mcp.length), ` ${mcp.length === 1 ? 'MCP server' : 'MCP servers'}`);
  covered.append(big);
  if (hosts.length) covered.append(el('span', `and ${hosts.map(h => h.title.replace(/ built-in tools$/, '')).join(', ')} built-in tools`, 'fact-note'));

  const sources = el('div', undefined, 'fact');
  sources.append(el('span', 'Rules for MCP servers come from', 'fact-label'));
  if (mcp.length) {
    const bar = el('div', undefined, 'bar');
    bar.setAttribute('role', 'img');
    const legend = el('ul', undefined, 'bar-legend');
    const parts = kinds.map(kind => ({ ...kind, n: mcp.filter(s => s.kind === kind.key).length })).filter(k => k.n);
    bar.setAttribute('aria-label', parts.map(p => `${p.label}: ${p.n}`).join(', '));
    for (const part of parts) {
      const segment = el('span', undefined, `segment ${part.key}`);
      segment.style.flexGrow = part.n;
      segment.title = `${part.label}: ${plural(part.n, 'server')}`;
      bar.append(segment);
      const item = el('li');
      item.append(el('span', undefined, `swatch ${part.key}`), `${part.label} `, el('strong', part.n));
      legend.append(item);
    }
    sources.append(bar, legend);
  } else sources.append(el('p', 'No rule names an MCP server.', 'fact-note'));

  const inUse = state.batteries.filter(b => b.configured);
  const batteriesFact = el('div', undefined, 'fact');
  batteriesFact.append(el('span', 'Batteries in use', 'fact-label'));
  const chips = el('div', undefined, 'battery-chips');
  for (const b of inUse) {
    const info = statusInfo(b);
    const chip = button(displayName(b.name), () => { if (brokenBattery(b)) expanded.set(b.name, true); navigate('batteries'); }, `battery-chip ${info.kind}`);
    chip.title = info.label;
    chips.append(chip);
  }
  if (!inUse.length) chips.append(el('span', 'None yet.', 'fact-note'));
  batteriesFact.append(chips);

  card.append(covered, sources, batteriesFact, rich(fallbackText(), 'p', 'summary-foot'));
  return card;
}
// What happens to a call no rule names, read from the policy's `*` rule.
function fallbackText() {
  const policy = state.policy ?? {};
  const wildcard = (policy.tool ?? []).find(rule => rule.name === '*');
  if (!wildcard || (wildcard.requires?.attention ?? []).includes('blocked')) return 'Tools with no rule: APPA blocks the call.';
  if (!wildcard.annotator) return 'Tools with no rule: one shared default rule applies.';
  const annotator = (policy.annotator ?? []).find(a => a.name === wildcard.annotator);
  const asks = (annotator?.marks ?? []).length > 0;
  return `Tools with no rule: before each call, the \`${wildcard.annotator}\` annotator writes a rule for that call only.${asks ? ' That rule can ask you to approve the call.' : ''}`;
}
function sourceChip(source) {
  if (source === CUSTOM) return el('span', 'your rules', 'chip');
  const battery = batteryByName(source);
  return brokenBattery(battery) ? el('span', `${displayName(source)} · ${statusInfo(battery).label.toLowerCase()}`, 'chip alert') : el('span', displayName(source), 'chip battery');
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
  if (serverFilter === 'setup' && !server.sources.some(source => brokenBattery(batteryByName(source)))) return false;
  if (serverFilter === 'battery' && !server.sources.some(source => source !== CUSTOM)) return false;
  if (serverFilter === 'custom' && !server.sources.includes(CUSTOM)) return false;
  return !query || server.title.toLowerCase().includes(query) || server.rules.some(rule => rule.name.toLowerCase().includes(query));
}
function serverTable(list) {
  content.append(el('h2', 'Tools in your configuration'));
  content.append(el('p', 'Each MCP server and set of built-in tools that your policy names, and where its rules come from. Open a row to see the rules.', 'description'));
  const toolbar = el('div', undefined, 'toolbar');
  const search = el('input', undefined, 'search');
  search.type = 'search'; search.placeholder = 'Search servers and rules'; search.value = query;
  search.setAttribute('aria-label', 'Search servers and rules');
  const filter = el('select');
  filter.setAttribute('aria-label', 'Filter servers');
  [['all', 'All'], ['setup', 'Battery needs setup'], ['battery', 'Battery rules'], ['custom', 'Your rules']]
    .forEach(([value, label]) => { const option = el('option', label); option.value = value; filter.append(option); });
  filter.value = serverFilter;
  const total = el('span', undefined, 'muted');
  toolbar.append(search, filter, total);
  content.append(toolbar);
  const result = table(['Server or tool set', 'Rules come from', 'Rules', ''], 'servers');
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
      const sources = el('td');
      if (server.sources.length) sources.append(...server.sources.map(sourceChip));
      else sources.append(el('span', 'Unknown', 'muted'));
      // A rule per tool is the usual case; say both counts only when they differ.
      const rules = el('td', server.rules.length === server.tools ? plural(server.tools, 'tool') : `${plural(server.tools, 'tool')} · ${plural(server.rules.length, 'rule')}`);
      const chevron = el('td', open ? '▾' : '▸', 'chevron');
      row.append(name, sources, rules, chevron);
      clickable(row, open, () => { open ? serversOpen.delete(server.key) : serversOpen.add(server.key); draw(); });
      body.append(row);
      if (open) body.append(serverDetail(server, draw));
    }
    if (!shown.length) {
      const row = el('tr'), cell = el('td', 'No matching servers.', 'empty'); cell.colSpan = 4; row.append(cell); body.append(row);
    }
  }
  search.addEventListener('input', () => { query = search.value.trim().toLowerCase(); draw(); });
  filter.addEventListener('change', () => { serverFilter = filter.value; draw(); });
  draw();
}
function serverDetail(server, redraw) {
  const row = el('tr', undefined, 'server-detail'), cell = el('td');
  cell.colSpan = 4;
  row.append(cell);
  const groups = new Map();
  for (const rule of server.rules) {
    if (query && !rule.name.toLowerCase().includes(query) && !server.title.toLowerCase().includes(query)) continue;
    const source = sourceOf(rule) ?? 'unknown';
    if (!groups.has(source)) groups.set(source, []);
    groups.get(source).push(rule);
  }
  for (const [source, rules] of groups) {
    const group = el('div', undefined, 'rule-group');
    const label = source === CUSTOM ? 'Your rules' : source === 'unknown' ? 'Source unknown' : `${displayName(source)} battery`;
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
// The mascot on openappa.com's pixel grid. 1 body · 3 muzzle and paws · 2 nose · 4 eyes.
const BEAST = [
  '.....11..........11.....', '.....11..........11.....', '....1111111111111111....',
  '...111111111111111111...', '...111111111111111111...', '...111111111111111111...',
  '...111444111111444111...', '...111444111111444111...', '...111444111111444111...',
  '...111111111111111111...', '...111111133331111111...', '...111111132231111111...',
  '...111111111111111111...', '....1111111111111111....', '.1111111111111111111111.',
  '111111111111111111111111', '111111111111111111111111', '111111111111111111111111',
  '111111111111111111111111', '111111111111111111111111', '11111..1111..1111..11111',
  '33333..3333..3333..33333',
];
function mascot() {
  const body = svg('g', {}), eyes = svg('g', { class: 'eyes' });
  const cls = { 1: 'px-body', 2: 'px-eye', 3: 'px-dim', 4: 'px-body' };
  BEAST.forEach((row, y) => {
    for (let x = 0; x < row.length;) {
      const c = row[x];
      let end = x; while (row[end] === c) end++;
      if (c !== '.') body.append(svg('rect', { x, y, width: end - x, height: 1, class: cls[c] }));
      if (c === '4') eyes.append(svg('rect', { x, y, width: end - x, height: 1, class: 'px-eye' }));
      x = end;
    }
  });
  const look = svg('g', { class: 'look' });
  look.append(eyes);
  const group = svg('g', { class: 'hop' });
  group.append(body, look);
  return group;
}
function bubble(x, y, w, text, cls, tailX) {
  const g = svg('g', { class: `bubble ${cls}` });
  g.append(svg('rect', { x, y, width: w, height: 9, class: 'bubble-box' }),
    svg('rect', { x: tailX, y: y + 9, width: 2, height: 2, class: 'bubble-tail' }),
    svg('rect', { x: tailX + 1, y: y + 11, width: 1, height: 1, class: 'bubble-tail' }));
  const label = svg('text', { x: x + w / 2, y: y + 6.2, 'text-anchor': 'middle', class: 'bubble-text' });
  label.textContent = text;
  g.append(label);
  return g;
}
// APPA hops from GitHub to Slack to Claude Code and asks each a question in its own language.
function journey() {
  const stops = [
    { name: 'GitHub', key: 'github', ask: 'mrrp? blep?', answer: 'brrzt! ok' },
    { name: 'Slack', key: 'slack', ask: 'psst… wub?', answer: 'shh… tsk!' },
    { name: 'Claude Code', key: 'claude', ask: 'hnn? zorp?', answer: 'hmm… k!' },
  ];
  const figure = el('figure', undefined, 'journey');
  const scene = svg('svg', { viewBox: '0 17 240 41', role: 'img', 'shape-rendering': 'crispEdges',
    'aria-label': 'The APPA mascot visits GitHub, Slack and Claude Code and asks each one a question.' });
  scene.append(svg('rect', { x: 0, y: 56, width: 240, height: 1, class: 'ground' }));
  stops.forEach((stop, i) => {
    const cx = 55 + 80 * i;
    const kiosk = svg('g', { class: `kiosk ${stop.key}` });
    kiosk.append(svg('rect', { x: cx - 14, y: 40, width: 34, height: 16, class: 'kiosk-box' }),
      svg('rect', { x: cx - 14, y: 40, width: 34, height: 3, class: 'kiosk-roof' }),
      svg('rect', { x: cx + 15, y: 45, width: 2, height: 2, class: `kiosk-lamp lamp-${i}` }));
    const label = svg('text', { x: cx + 3, y: 52, 'text-anchor': 'middle', class: 'kiosk-text' });
    label.textContent = stop.name;
    kiosk.append(label);
    scene.append(kiosk,
      bubble(cx - 44, 20, 34, stop.ask, `ask ask-${i}`, cx - 30),
      bubble(cx - 8, 25, 30, stop.answer, `answer answer-${i}`, cx + 4));
  });
  const walker = svg('g', { transform: 'translate(15 34)' });
  walker.append(mascot());
  scene.append(walker);
  const caption = el('figcaption');
  caption.id = 'journey-caption';
  caption.append(el('strong', 'Batteries teach OpenAPPA about your tools and your data. '),
    'Each battery brings rules for one set of tools. Many batteries also ask the provider questions, for example: ',
    el('em', 'Who can read this Slack channel?'),
    ' APPA uses the answers to let data go only to people who can already read it.');
  figure.append(scene, caption);
  return figure;
}
// The badge, its color, and the next step for one battery's latest check.
function statusInfo(b) {
  if (!b?.check) return { label: 'Checking…', kind: '', detail: '' };
  const name = displayName(b.name);
  if (b.check.status === 'ready') return { label: 'Ready', kind: 'good', detail: '' };
  const [label, text] = reasons[b.check.reason] ?? ['Needs setup', 'Follow the setup steps, then check again.'];
  const kind = b.check.status === 'unavailable' ? 'bad' : 'warn';
  const fill = s => s.replaceAll('{name}', name);
  if (b.check.reason === 'missing_executable') {
    const missing = b.dependencies.filter(d => !d.installed).map(d => `\`${d.executable}\``);
    if (missing.length) return { label, kind, detail: `Install ${missing.join(', ')}, then check again.` };
    const alternatives = b.alternatives.filter(a => !a.installed).map(a => `\`${a.executable}\``);
    if (alternatives.length) return { label, kind, detail: `Add a token, or install ${alternatives.join(', ')} and sign in.` };
  }
  if (b.check.reason === 'cli_not_authenticated' || b.check.reason === 'missing_credential') {
    const hints = b.alternatives.map(a => a.installed ? `\`${a.login_hint}\`` : `install \`${a.executable}\`, then \`${a.login_hint}\``);
    if (hints.length) return { label, kind, detail: `Add a token, or run ${hints.join('; ')} in a terminal.` };
  }
  return { label: fill(label), kind, detail: fill(text) };
}
function checkStatus(b) { const info = statusInfo(b); return status(info.label, info.kind); }
function configurable(b) { return b.credentials.length > 0 || b.setup?.length > 0 || (needsSetup(b) && b.alternatives.length > 0); }
function batteries() {
  heading('Batteries');
  errors();
  content.append(journey());
  const list = state.batteries.filter(relevant);
  if (!list.length) content.append(el('p', 'No batteries in use. Include one with appa battery install <name>.', 'empty'));
  else {
    const result = table(['Battery', 'Status', ''], 'battery-table');
    const body = el('tbody');
    list.forEach(b => {
      const row = el('tr'), name = el('td');
      name.append(el('span', displayName(b.name), 'battery-name'));
      name.title = b.description;
      const open = configurable(b) && (expanded.get(b.name) ?? (configureRequested && needsSetup(b)));
      // The open panel repeats the benefit under "Why connect".
      if (b.benefit && !open) name.append(rich(b.benefit, 'span', 'battery-benefit'));
      row.append(name);
      const checkCell = el('td'); checkCell.append(checkStatus(b));
      const detail = statusInfo(b).detail; if (detail) checkCell.append(rich(detail, 'span', 'check-message'));
      row.append(checkCell);
      const action = el('td', undefined, 'battery-action');
      const broken = brokenBattery(b);
      if (configurable(b)) {
        const label = open ? 'Close' : broken ? (b.credentials.length ? 'Add token' : 'Set up') : 'Configure';
        const toggle = button(label, () => { expanded.set(b.name, !open); render(); }, open || !broken ? 'link' : 'small');
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
}
function credentialField(b, c) {
  const field = el('div', undefined, 'credential'), head = el('div', undefined, 'field-heading');
  const id = `credential-${b.name}-${c.variable}`;
  const label = el('label', undefined);
  label.append(b.credentials.length === 1 ? 'Token ' : '', el('code', c.variable, 'variable'));
  label.htmlFor = id;
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
function stepList(b, title) {
  const steps = el('div', undefined, 'steps');
  if (title) steps.append(el('h3', title));
  const list = el('ol');
  b.setup.forEach(step => list.append(rich(step, 'li')));
  steps.append(list);
  return steps;
}
// Which way of signing in the latest ready check used.
function inUse(b, authentication) { return b.check?.status === 'ready' && b.check.authentication === authentication; }
function option(title, active) {
  const card = el('div', undefined, `option${active ? ' active' : ''}`);
  const head = el('div', undefined, 'option-head');
  head.append(el('h3', title));
  if (active) head.append(el('span', 'In use', 'in-use'));
  card.append(head);
  return card;
}
function configuration(b) {
  const form = el('form', undefined, 'battery-configuration');
  form.setAttribute('aria-label', `Configure ${displayName(b.name)}`);
  form.addEventListener('submit', event => { event.preventDefault(); save([b]); });
  if (b.benefit) {
    const why = el('div', undefined, 'why');
    why.append(el('h3', `Why connect ${displayName(b.name)}`), rich(b.benefit, 'p'));
    form.append(why);
  }
  const tokenPart = [];
  b.credentials.forEach(c => tokenPart.push(credentialField(b, c)));
  if (b.credentials.length) {
    const actions = el('div', undefined, 'actions');
    const submit = button('Save and check', () => {}); submit.type = 'submit'; actions.append(submit);
    tokenPart.push(actions);
  }
  if (!b.alternatives.length) {
    if (b.setup?.length) form.append(stepList(b, b.credentials.length ? 'How to get the token' : 'Setup steps'));
    form.append(...tokenPart);
    return form;
  }
  // A CLI sign-in and a token are two ways to one result: show them side by side.
  form.append(el('h3', `Choose how APPA signs in to ${displayName(b.name)}`, 'choose'));
  const choices = el('div', undefined, 'choices');
  for (const a of b.alternatives) {
    const active = inUse(b, 'cli');
    const card = option(`Sign in with ${a.executable}`, active);
    card.append(el('p', active ? 'You are signed in, and APPA uses this login. To sign in again, run:'
      : a.installed ? 'Run this in a terminal:' : `Install ${a.executable} first, then run this in a terminal:`),
      el('code', a.login_hint, 'login-hint'));
    const actions = el('div', undefined, 'actions');
    actions.append(button('Check again', () => check([b]), 'secondary'));
    card.append(actions);
    choices.append(card);
  }
  choices.append(el('span', 'or', 'or'));
  const card = option('Use a token', inUse(b, 'token'));
  if (b.setup?.length) card.append(stepList(b));
  card.append(...tokenPart);
  choices.append(card);
  form.append(choices);
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
