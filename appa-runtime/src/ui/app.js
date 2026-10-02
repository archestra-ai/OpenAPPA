'use strict';

// `appa ui --battery <name>` serves this page for one battery. The chat gives the steps;
// the page takes the token, and offers the same battery's other entry points. When the
// battery is ready, the command exits.
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
function notice(text, error = false) {
  if (!text) { notices.replaceChildren(); return; }
  notices.replaceChildren(el('div', text, `notice${error ? ' error' : ''}`));
}
async function api(path, body) {
  const response = await fetch(`/api/${path}`, {
    method: body === undefined ? 'GET' : 'POST',
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}
function setBusy(value) {
  busy = value;
  document.querySelectorAll('button').forEach(b => b.disabled = value);
  document.querySelectorAll('input, select').forEach(f => f.disabled = value);
}
async function work(label, action) {
  if (busy) return;
  setBusy(true);
  notice(label);
  try { state = await action(); notices.replaceChildren(); render(); }
  catch (error) { notice(error.message, true); }
  finally { setBusy(false); }
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

// ---- OrcaRouter -------------------------------------------------------------
//
// Two explicit choices, both ending in the same ordinary OrcaRouter key: paste an
// existing key, or connect an account. The connect flow is out-of-band (the code is
// shown on the consent screen and pasted here); a machine whose browser reaches this
// page can ask for the loopback redirect instead.

// The in-flight login. `generation` is the attempt this page holds; every answer is
// checked against it, so a superseded attempt cannot change the page.
let orca = { generation: 0, url: '', busy: false };

/** Cancel any in-flight login. Reached by Cancel, by choosing the key path, and by pagehide. */
function orcaCancel(send = true) {
  // Clear the page's own state synchronously: a pagehide cannot await, and the guarded
  // `finally` of a superseded request would refuse to touch it.
  orca.busy = false;
  orca.url = '';
  if (send) api('orcarouter/cancel', {}).catch(() => {});
}

// The model control is a combobox over the catalog this deployment can actually call.
// It is a listbox the page renders, not a free-text field and not a native `select`:
// the options are the filtered catalog, and the one chosen is the profile's model.
function orcaModelSelect(auth) {
  const field = el('div', undefined, 'field');
  const id = `orca-model-${auth}`;
  const label = el('label'); label.htmlFor = id; label.append('Model');
  const trigger = el('button', 'Loading models…', 'model-trigger');
  trigger.type = 'button'; trigger.id = id; trigger.setAttribute('aria-haspopup', 'listbox');
  trigger.setAttribute('aria-expanded', 'false');
  const panel = el('div', undefined, 'model-panel');
  panel.hidden = true; panel.setAttribute('role', 'listbox'); panel.setAttribute('aria-label', 'Model');
  field.append(label, trigger, panel);
  const status = el('small', undefined, 'model-status');
  field.append(status);

  // The chosen model, and every option the last load produced. Only ids the catalog
  // returned ever reach `options`; a model the catalog does not list cannot be chosen.
  const state = { value: '', options: [], open: false, loaded: false };

  const paint = () => {
    trigger.textContent = state.value || (state.options.length ? 'Choose a model' : trigger.textContent);
    panel.replaceChildren();
    for (const model of state.options) {
      const option = el('button', model.label, 'model-option');
      option.type = 'button'; option.dataset.model = model.id;
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', String(model.id === state.value));
      if (model.id === state.value) option.classList.add('selected');
      const detail = [model.context_length ? `${model.context_length} context` : '', (model.input_modalities || []).join(', ')]
        .filter(Boolean).join(' · ');
      if (detail) option.append(el('small', detail));
      option.addEventListener('click', () => {
        state.value = model.id;
        close();
        paint();
      });
      panel.append(option);
    }
  };
  const open = () => { state.open = true; panel.hidden = false; trigger.setAttribute('aria-expanded', 'true'); };
  const close = () => { state.open = false; panel.hidden = true; trigger.setAttribute('aria-expanded', 'false'); };
  trigger.addEventListener('click', () => { state.open ? close() : open(); });
  trigger.addEventListener('keydown', event => { if (event.key === 'Escape') close(); });
  document.addEventListener('click', event => { if (!field.contains(event.target)) close(); });

  const load = async (capability = 'chat', modalities = 'text') => {
    status.textContent = 'Loading models…';
    try {
      const answer = await api(`orcarouter/models?capability=${capability}&modalities=${modalities}`);
      state.options = answer.models;
      state.loaded = true;
      // A model the catalog no longer offers cannot stay chosen.
      if (state.value && !state.options.some(model => model.id === state.value)) {
        state.value = '';
        status.textContent = 'The selected model no longer supports this input. Choose another.';
      } else {
        status.textContent = answer.degraded
          ? `Catalog unavailable: showing ${answer.source === 'verified_seed' ? 'the verified seed' : 'the last known good catalog'}. Refresh to try again.`
          : `${answer.models.length} models from the live catalog.`;
      }
      trigger.disabled = !state.options.length;
      if (!state.options.length && !status.textContent) status.textContent = 'No model matches this capability.';
    } catch (error) {
      state.options = []; state.loaded = false;
      trigger.disabled = true;
      status.textContent = 'The model catalog could not be read. Refresh to try again.';
    }
    if (state.open) open(); else close();
    paint();
  };

  trigger.addEventListener('focus', () => { if (!state.loaded) { state.loaded = true; load(); } });
  field.addEventListener('attachment-change', event => {
    // An attachment or a task change recomputes the options from the catalog.
    load(event.detail.capability, event.detail.modalities);
  });
  window.addEventListener('orcarouter-attachment-change', event => {
    field.dispatchEvent(new CustomEvent('attachment-change', { detail: event.detail }));
  });
  field.modelOptions = () => state.options.map(model => model.id);
  field.selectedModel = () => state.value;
  return field;
}

function orcaSection() {
  const status = state.orcarouter;
  const section = el('section', undefined, 'orca');
  section.setAttribute('aria-label', 'OrcaRouter');
  const head = el('div', undefined, 'orca-head');
  const mark = el('img', undefined, 'orca-mark');
  // The official OrcaRouter mark. The browser fetches it from the auth origin, which the
  // page's Content-Security-Policy admits.
  mark.src = 'https://www.orcarouter.ai/orca-logo-classic.png';
  mark.alt = ''; mark.width = 24; mark.height = 24; mark.loading = 'lazy'; mark.decoding = 'async';
  head.append(mark, el('h2', 'OrcaRouter'));
  section.append(head);
  section.append(el('p', status.stored
    ? `A key is stored (${status.source === 'environment' ? 'from the environment' : 'saved in APPA'}). Enter a new one to replace it, or clear it.`
    : 'Enter an API key, or connect an account to create one.', 'description'));

  // Choice 1 — an existing API key.
  const keyBlock = el('div', undefined, 'auth-block');
  keyBlock.append(el('h3', 'API key'));
  const variable = status.variable;
  const keyInput = el('input');
  keyInput.type = 'password'; keyInput.id = 'orca-key'; keyInput.name = variable;
  keyInput.autocomplete = 'new-password'; keyInput.spellcheck = false; keyInput.maxLength = 16384;
  keyInput.placeholder = status.stored ? 'Replace the saved key' : 'sk-orca-…';
  const keyLabel = el('label', undefined, 'field-label'); keyLabel.htmlFor = 'orca-key'; keyLabel.append(el('code', variable));
  const keyActions = el('div', undefined, 'actions');
  keyActions.append(
    button('Save key', () => work('Saving the key…', () => api('orcarouter/key', { key: keyInput.value }))),
    button('Clear key', () => work('Clearing the key…', () => api('orcarouter/key', { key: null })), 'secondary'),
  );
  keyBlock.append(keyLabel, keyInput, keyActions);
  section.append(keyBlock);

  // Choice 2 — connect an account (OAuth 2.0 + PKCE).
  const connectBlock = el('div', undefined, 'auth-block');
  connectBlock.append(el('h3', 'Connect with OrcaRouter'));
  connectBlock.append(el('p', 'Opens the OrcaRouter consent screen. Approve it, then paste the code it shows.', 'description'));
  if (orca.url) {
    const link = el('code', undefined, 'orcarouter-url');
    const anchor = el('a', orca.url); anchor.href = orca.url; anchor.target = '_blank'; anchor.rel = 'noreferrer noopener';
    link.append(anchor);
    connectBlock.append(link);
    const codeInput = el('input');
    codeInput.type = 'text'; codeInput.id = 'orca-code'; codeInput.autocomplete = 'one-time-code';
    codeInput.placeholder = 'Paste the code from OrcaRouter';
    const codeActions = el('div', undefined, 'actions');
    codeActions.append(
      button('Finish connecting', () => work('Exchanging the code…', () => api('orcarouter/complete', { code: codeInput.value, generation: orca.generation }))),
      button('Cancel', () => { orcaCancel(); render(); }, 'secondary'),
    );
    connectBlock.append(codeInput, codeActions);
  } else {
    const actions = el('div', undefined, 'actions');
    actions.append(
      button('Connect with OrcaRouter', async () => {
        if (busy) return;
        setBusy(true);
        try {
          const answer = await api('orcarouter/begin', {});
          orca.generation = answer.generation; orca.url = answer.url;
          window.open(answer.url, '_blank', 'noopener');
          render();
        } catch (error) { notice(error.message, true); }
        finally { setBusy(false); }
      }),
      button('Connect this machine (redirect)', async () => {
        if (busy) return;
        setBusy(true);
        try {
          const answer = await api('orcarouter/begin', { loopback: true });
          orca.generation = answer.generation; orca.url = answer.url;
          window.open(answer.url, '_blank', 'noopener');
          render();
        } catch (error) { notice(error.message, true); }
        finally { setBusy(false); }
      }, 'secondary'),
    );
    connectBlock.append(actions);
  }
  section.append(connectBlock);

  // The model catalog, filtered to what this deployment can actually call.
  section.append(orcaModelSelect('orcarouter'));

  // Where the key is managed on the OrcaRouter side.
  const manage = el('p', undefined, 'description');
  const keys = el('a', 'Manage keys'); keys.href = status.key_url; keys.target = '_blank'; keys.rel = 'noreferrer noopener';
  const apps = el('a', 'Authorized apps'); apps.href = status.authorized_apps_url; apps.target = '_blank'; apps.rel = 'noreferrer noopener';
  manage.append('OrcaRouter: ', keys, ' · ', apps);
  section.append(manage);
  return section;
}

// `pagehide` may put this page into the back-forward cache. Invalidate the attempt and
// clear the busy flag and the authorization hint synchronously here — a guarded
// `finally` from the superseded request would refuse to, leaving the restored page busy —
// then tell the server to cancel, with `keepalive` so the request survives the unload.
window.addEventListener('pagehide', () => {
  if (!orca.url && !orca.busy) return;
  orcaCancel(false);
  notice('');
  try {
    fetch('/api/orcarouter/cancel', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}', keepalive: true });
  } catch { /* the page is going away regardless */ }
});

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
  // The OrcaRouter battery is also the deployment's model provider: its page shows both
  // ways in and the models the deployment can call.
  if (state.orcarouter && (b.name === 'orcarouter' || state.orcarouter.configured)) {
    content.append(orcaSection());
  }
}
function check() { return work('Checking…', () => api('check', { batteries: [batteryName] })); }

(async () => {
  try { state = await api('state'); render(); await check(); }
  catch (error) { content.replaceChildren(el('h1', 'Connection needed'), el('p', error.message, 'description')); }
})();
