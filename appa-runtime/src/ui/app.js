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
  verified: 'Connection verified',
  missing_credential: 'Enter a token.',
  cli_not_authenticated: 'CLI is not logged in. Log in or enter a token.',
  missing_executable: 'Install the missing executable, then check again.',
  invalid_credential: 'Credential rejected by provider.',
  insufficient_access: 'Credential lacks the required access.',
  provider_unavailable: 'Provider unavailable. Try again.',
  check_failed: 'Check failed or returned an invalid response.',
  check_timed_out: 'Check timed out. Try again.',
  no_check: 'No provider check declared.',
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
  else if (view === 'policies') policies();
  else batteries();
}
function refresh() { return work(async () => { state = await api('state'); render(); }); }
function overview() {
  heading('Status', '', button('Refresh', refresh, 'secondary'));
  const rules = state.policy?.tool ?? [];
  const servers = new Set(rules.map(rule => rule.name?.split('/'))
    .filter(parts => parts?.[0] === 'mcp' && parts[1] && parts[1] !== 'appa' && !/[?*\[]/.test(parts[1]))
    .map(parts => parts[1]));
  const metrics = table(['Batteries installed', 'Policy rules', 'MCPs covered'], 'stats');
  const body = el('tbody'), row = el('tr');
  [state.batteries.length, rules.length, servers.size].forEach(value => row.append(el('td', value)));
  body.append(row); metrics.node.append(body); content.append(metrics.wrap);
  errors();
}
function needsSetup(b) { return b.check?.status === 'needs_configuration'; }
function relevant(b) { return b.selected || b.included || b.configured || selected.has(b.name); }
function checkStatus(b) {
  if (!b.check) return status('Not checked');
  if (b.check.status === 'ready') return status('Ready', 'good');
  if (b.check.status === 'unavailable') return status('Check failed', 'bad');
  if (b.check.status === 'needs_configuration') return status('Needs setup', 'warn');
  return status('Unverified');
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
    if (hints.length) return `Enter a credential or ${hints.join('; ')}.`;
  }
  return reasons[b.check.reason] ?? '';
}
function batteries() {
  heading('Batteries', 'Readiness reported by each battery.');
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
      const open = expanded.get(b.name) ?? (configureRequested && needsSetup(b));
      const action = el('td');
      const toggle = button(open ? 'Close' : 'Configure', () => { expanded.set(b.name, !open); render(); }, 'link');
      toggle.setAttribute('aria-expanded', String(open));
      toggle.setAttribute('aria-controls', `configure-${b.name}`);
      action.append(toggle); row.append(action); body.append(row);
      const detailRow = el('tr', undefined, 'configuration-row');
      detailRow.id = `configure-${b.name}`; detailRow.hidden = !open;
      const cell = el('td'); cell.colSpan = 3;
      if (open) cell.append(configuration(b));
      detailRow.append(cell); body.append(detailRow);
    });
    result.node.append(body); content.append(result.wrap);
    const actions = el('div', undefined, 'actions');
    actions.append(button('Check connections', () => check(list), 'secondary')); content.append(actions);
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
  b.dependencies.filter(d => !d.installed).forEach(d => form.append(el('div', `Install ${d.executable}.`, 'requirement warn')));
  if (needsSetup(b)) b.alternatives.forEach(a => {
    form.append(el('p', `${b.credentials.length ? 'Or sign in' : 'Sign in'} using ${a.executable}${a.installed ? '' : ' (install it first)'}. Run this in your terminal, then click Check connection:`, 'requirement'), el('code', a.login_hint, 'login-hint'));
  });
  if (needsSetup(b) && b.setup) {
    const instructions = el('details', undefined, 'instructions');
    instructions.append(el('summary', 'Setup instructions'), el('p', b.setup)); form.append(instructions);
  }
  if (!b.credentials.length && !b.dependencies.some(d => !d.installed) && !b.alternatives.length && !b.setup) {
    form.append(el('p', 'No configuration required.', 'muted'));
  }
  const actions = el('div', undefined, 'actions');
  if (b.credentials.length) {
    const submit = button('Save and check', () => {}); submit.type = 'submit'; actions.append(submit);
  }
  actions.append(button('Check connection', () => check([b]), 'secondary'));
  form.append(actions);
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
function policies() {
  heading('Policies', state.runtime ? 'Active contracts, grouped by MCP namespace.' : 'Configured contracts. Enforcement is awaiting setup.', button('Refresh', refresh, 'secondary'));
  const search = el('input', undefined, 'search');
  search.type = 'search'; search.placeholder = 'Filter by server or tool'; search.setAttribute('aria-label', 'Filter policies');
  content.append(search);
  const groups = new Map();
  for (const rule of state.policy?.tool ?? []) {
    const name = rule.name ?? 'Unnamed rule', server = name.startsWith('mcp/') ? name.split('/')[1] : 'Built-in / other';
    if (!groups.has(server)) groups.set(server, []);
    groups.get(server).push(rule);
  }
  for (const server of state.configured_servers) if (server !== 'appa' && !groups.has(server)) groups.set(server, []);
  const results = el('div'); content.append(results);
  function draw() {
    results.replaceChildren(); const query = search.value.trim().toLowerCase();
    for (const [server, all] of [...groups].sort(([a], [b]) => a.localeCompare(b))) {
      const rules = all.filter(rule => `${server} ${rule.name}`.toLowerCase().includes(query));
      if (query && !rules.length && !server.toLowerCase().includes(query)) continue;
      const group = el('details', undefined, 'policy-group'); group.open = true;
      const summary = el('summary', server); summary.append(el('small', ` ${rules.length} ${rules.length === 1 ? 'contract' : 'contracts'}`)); group.append(summary);
      const aliases = state.runtime?.server_aliases?.[server];
      if (aliases?.length) group.append(el('p', `Connections: ${aliases.join(', ')}`, 'muted'));
      if (!all.length) group.append(el('p', 'No contracts under this namespace. Check aliases or annotation coverage.', 'muted'));
      else {
        const result = table(['Tool contract', 'Source'], 'policy-table'), body = el('tbody');
        for (const rule of rules) {
          const row = el('tr'), cell = el('td'), details = el('details');
          details.append(el('summary', rule.name), el('pre', JSON.stringify(rule, null, 2))); cell.append(details);
          row.append(cell, el('td', state.origins[rule.name] ?? (state.runtime ? 'Active configuration' : 'Configuration'))); body.append(row);
        }
        result.node.append(body); group.append(result.wrap);
      }
      results.append(group);
    }
    if (!results.children.length) results.append(el('p', 'No matching policy contracts.', 'empty'));
  }
  search.addEventListener('input', draw); draw();
  const report = state.runtime?.validation;
  if (report) {
    const details = el('details', undefined, 'check-list'); details.append(el('summary', 'Coverage checks'));
    const result = table(['Tool', 'Status', 'Details']), body = el('tbody');
    for (const item of report.tools ?? []) {
      const row = el('tr'), cell = el('td'); cell.append(status(item.status, item.status === 'valid' ? 'good' : 'warn'));
      row.append(el('td', item.tool, 'mono'), cell, el('td', item.reason ?? '')); body.append(row);
    }
    result.node.append(body); details.append(result.wrap);
    for (const error of [...(report.errors ?? []), ...(report.diagnostics ?? [])]) details.append(el('p', error, 'muted'));
    content.append(details);
  }
  content.append(el('p', `${state.runtime?.inventory?.tools?.length ?? 0} observed tools. Missing observations leave coverage unverified.`, 'muted'));
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
  if (!state || busy || view !== 'overview') return;
  try { state = await api('state'); if (view === 'overview' && !busy) render(); } catch { /* Explicit actions surface errors. */ }
}, 10000);
