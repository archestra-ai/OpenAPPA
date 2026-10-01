'use strict';

// `appa ui --battery <name>` serves this page for one battery. The chat gives the steps;
// the page only takes the token. When the battery is ready, the command exits.
const batteryName = new URLSearchParams(location.search).get('battery');
const content = document.querySelector('#content');
const notices = document.querySelector('#notice');
let state;
let busy = false;

// Each check reason as the one sentence that says what to do next. `{name}` is the battery's name.
const reasons = {
  missing_configuration: 'Setup is not complete. Follow the steps in the chat.',
  missing_executable: 'A program the battery needs is missing. Install it, then check again.',
  invalid_credential: '{name} did not accept the token. The token can be expired, revoked, or copied incompletely. Add a new token.',
  insufficient_access: 'The token works, but it cannot read all that the battery needs. Compare its scopes with the steps in the chat.',
  provider_unavailable: 'APPA cannot connect to {name}. Check your network or VPN, then check again.',
  check_failed: 'The battery check stopped with an error, or gave an answer that APPA cannot read. Check again.',
  check_timed_out: '{name} did not answer in 15 seconds. Check again later.',
};
const names = {
  github: 'GitHub', slack: 'Slack', huggingface: 'Hugging Face', databricks: 'Databricks', 'claude-code': 'Claude Code',
  grain: 'Grain', linear: 'Linear', notion: 'Notion', sentry: 'Sentry', posthog: 'PostHog', pagerduty: 'PagerDuty',
  launchdarkly: 'LaunchDarkly', cloudflare: 'Cloudflare', monday: 'monday.com', archestra: 'Archestra', jev: 'Jev',
  xmemory: 'xmemory', 'google-workspace': 'Google Workspace', 'microsoft-learn': 'Microsoft Learn',
};
function displayName(name) { return names[name] ?? name; }

function el(tag, text, cls) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (cls) node.className = cls;
  return node;
}
function button(text, action, style = '') {
  const node = el('button', text, `action ${style}`);
  node.type = 'button';
  node.addEventListener('click', action);
  return node;
}
function notice(text, error = false) { notices.replaceChildren(el('div', text, `notice${error ? ' error' : ''}`)); }
async function api(path, body) {
  const response = await fetch(`/api/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}
async function work(label, action) {
  if (busy) return;
  busy = true;
  document.querySelectorAll('button').forEach(b => b.disabled = true);
  notice(label);
  try { state = await action(); notices.replaceChildren(); render(); }
  catch (error) { notice(error.message, true); }
  finally {
    busy = false;
    document.querySelectorAll('button').forEach(b => b.disabled = false);
  }
}

function credentialField(b, c) {
  const field = el('div', undefined, 'credential');
  const id = `credential-${c.variable}`;
  const label = el('label');
  label.htmlFor = id;
  // The heading names the battery; the variable only tells two tokens apart.
  if (b.credentials.length === 1) label.append('Token');
  else label.append('Token ', el('code', c.variable));
  const input = el('input');
  input.type = 'password'; input.id = id; input.name = c.variable;
  input.autocomplete = 'new-password'; input.spellcheck = false; input.maxLength = 16384;
  input.placeholder = c.saved ? 'Replace the saved token' : 'Enter token';
  field.append(label, input);
  if (c.source === 'environment') field.append(el('small', 'An environment variable overrides a saved token.'));
  return field;
}

function render() {
  content.replaceChildren();
  const b = state.batteries.find(item => item.name === batteryName);
  if (!b) {
    content.append(el('h1', 'Battery not found'), el('p', `This installation has no battery named ${batteryName}.`, 'description'));
    return;
  }
  const name = displayName(b.name);
  if (b.check?.status === 'ready') {
    content.append(el('h1', `${name} is connected`), el('p', 'Go back to Claude Code. You can close this tab.', 'description'));
    return;
  }
  content.append(el('h1', `${name} token`));
  state.errors.forEach(error => content.append(el('p', error, 'notice error')));
  const reason = reasons[b.check?.reason];
  if (reason) content.append(el('p', reason.replaceAll('{name}', name), 'notice error'));
  const form = el('form');
  form.setAttribute('aria-label', `${name} token`);
  b.credentials.forEach(c => form.append(credentialField(b, c)));
  const actions = el('div', undefined, 'actions');
  const submit = el('button', 'Save', 'action'); submit.type = 'submit';
  actions.append(submit); form.append(actions);
  form.addEventListener('submit', event => {
    event.preventDefault();
    const credentials = Object.fromEntries([...form.querySelectorAll('input')].filter(i => i.value).map(i => [i.name, i.value]));
    work('Saving and checking…', () => api('credentials', { credentials, batteries: [b.name] }));
  });
  content.append(form);
  // A CLI sign-in is the other way to the same result.
  for (const a of b.alternatives) {
    const other = el('div', undefined, 'alternative');
    other.append(el('p', a.installed ? `Or sign in with ${a.executable} in a terminal, then check again:` : `Or install ${a.executable}, sign in with it, then check again:`),
      el('code', a.login_hint));
    const again = el('div', undefined, 'actions');
    again.append(button('Check again', check, 'secondary'));
    other.append(again);
    content.append(other);
  }
}
function check() { return work('Checking…', () => api('check', { batteries: [batteryName] })); }

(async () => {
  try { state = await api('state'); render(); await check(); }
  catch (error) { content.replaceChildren(el('h1', 'Connection needed'), el('p', error.message, 'description')); }
})();
