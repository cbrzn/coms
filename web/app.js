/* The server bundles this file and xterm.js; the dashboard makes no external requests. */
const $ = (id) => document.getElementById(id);
const fragment = new URLSearchParams(location.hash.slice(1));
const storageKey = `tmuxor:${location.host}`;
const token = fragment.get('token') || sessionStorage.getItem(`${storageKey}:token`) || '';
if (token) sessionStorage.setItem(`${storageKey}:token`, token);
if (location.hash) history.replaceState(null, '', location.pathname);
let state = { agents: [], messages: [], history: [] };
let selected = sessionStorage.getItem(`${storageKey}:agent`);
let selectedMessage = null;
let split = sessionStorage.getItem(`${storageKey}:split`) === 'true';
let currentReview = null;
let actionPending = false;
let operatorPending = false;
let stopped = false;
let toastTimer;
const terminals = new Map();
const drafts = new Map();

function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}
function toast(message, error = false) {
  $('toast').textContent = message;
  $('toast').classList.toggle('error', error);
  $('toast').hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { $('toast').hidden = true; }, error ? 7500 : 3500);
}
async function api(path, body) {
  const response = await fetch(path, {
    method: body === undefined ? 'GET' : 'POST',
    headers: { Authorization: `Bearer ${token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(15000),
  });
  if (!response.ok) {
    let message = `Request failed (${response.status}).`;
    try { message = (await response.json()).error || message; } catch { /* non-JSON error */ }
    const error = new Error(message);
    error.status = response.status;
    throw error;
  }
  return response.status === 204 ? null : response.json();
}
function statusLabel(agent) {
  return { running: 'Running', active: 'Active', review: 'Review pending', exited: 'Exited', offline: 'Offline' }[agent.status] || 'Connecting';
}
function chooseAgent(role) {
  selected = role;
  sessionStorage.setItem(`${storageKey}:agent`, role);
  renderAgents();
  renderTerminals();
}
function renderAgents() {
  $('agent-count').textContent = String(state.agents.length).padStart(2, '0');
  $('agents').replaceChildren(...state.agents.map((agent, index) => {
    const button = node('button', `agent${agent.role === selected ? ' selected' : ''}`);
    button.type = 'button';
    button.setAttribute('aria-pressed', String(agent.role === selected));
    button.setAttribute('aria-label', `${agent.role}, ${agent.adapter}, ${statusLabel(agent)}`);
    const copy = node('span', 'agent-copy');
    copy.append(node('span', 'agent-name', agent.role), node('span', 'agent-detail', `${agent.adapter} · ${statusLabel(agent)}`));
    button.append(node('span', 'avatar', String(index + 1).padStart(2, '0')), copy, node('span', `status-dot ${agent.status}`));
    button.addEventListener('click', () => chooseAgent(agent.role));
    return button;
  }));
}
function makeTerminal(agent) {
  const panel = node('section', 'terminal-panel');
  panel.setAttribute('aria-label', `${agent.role} terminal`);
  const heading = node('div', 'terminal-header');
  const connectionLabel = node('span', 'terminal-connection', 'Connecting…');
  const full = node('button', 'terminal-fullscreen', '⤢');
  full.type = 'button';
  full.setAttribute('aria-label', `Expand ${agent.role} terminal`);
  full.addEventListener('click', () => {
    const request = document.fullscreenElement ? document.exitFullscreen() : panel.requestFullscreen();
    request.catch((error) => toast(error.message, true));
  });
  heading.append(node('span', 'dot'), node('strong', '', agent.role), node('span', 'terminal-adapter', agent.adapter), connectionLabel, full);
  const container = node('div', 'terminal-container');
  panel.append(heading, container);
  $('terminals').append(panel);
  const terminal = new Terminal({
    cursorBlink: true, fontSize: 12, lineHeight: 1.2, fontFamily: '"SFMono-Regular", "Cascadia Code", "DejaVu Sans Mono", monospace',
    scrollback: 10000, allowProposedApi: false,
    theme: { background: '#1d231f', foreground: '#d8dfd3', cursor: '#d8eba1', selectionBackground: '#526044', black: '#1d231f', brightBlack: '#7c8976', green: '#a8c57f', brightGreen: '#d8eba1', blue: '#8babc0', yellow: '#d1b980' },
  });
  const fit = new FitAddon.FitAddon();
  terminal.loadAddon(fit);
  terminal.open(container);
  const item = { panel, terminal, fit, socket: null, reconnect: null, observer: null, connecting: false, closed: false };
  const send = (message) => { if (item.socket?.readyState === WebSocket.OPEN) item.socket.send(JSON.stringify(message)); };
  terminal.onData((data) => send({ type: 'input', data }));
  terminal.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));
  function connect() {
    if (stopped || item.closed) return;
    connectionLabel.textContent = 'Connecting…';
    const url = new URL(`/terminal/${encodeURIComponent(agent.role)}`, location.href);
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
    url.searchParams.set('token', token);
    const socket = new WebSocket(url);
    socket.binaryType = 'arraybuffer';
    item.socket = socket;
    socket.addEventListener('open', () => {
      terminal.reset();
      connectionLabel.textContent = 'Live';
      fit.fit();
      send({ type: 'resize', cols: terminal.cols, rows: terminal.rows });
    });
    socket.addEventListener('message', (event) => {
      if (typeof event.data === 'string') {
        try { const message = JSON.parse(event.data); if (message.error) toast(message.error, true); } catch { /* invalid control message */ }
      } else terminal.write(new Uint8Array(event.data));
    });
    socket.addEventListener('close', () => {
      connectionLabel.textContent = 'Reconnecting…';
      if (!stopped && !item.closed) item.reconnect = setTimeout(connect, 2500);
    });
    socket.addEventListener('error', () => { connectionLabel.textContent = 'Disconnected'; });
  }
  item.observer = new ResizeObserver(() => { if (!panel.hidden && container.clientWidth > 0) fit.fit(); });
  item.observer.observe(container);
  connect();
  return item;
}
function renderTerminals() {
  $('terminal-empty').hidden = state.agents.length > 0;
  $('terminals').classList.toggle('split', split);
  $('focus-view').setAttribute('aria-pressed', String(!split));
  $('split-view').setAttribute('aria-pressed', String(split));
  $('workspace-title').textContent = split ? 'The whole team.' : selected || 'Team workspace';
  const agent = state.agents.find((a) => a.role === selected);
  $('workspace-subtitle').textContent = split ? `${state.agents.length} agents. One shared direction.` : agent ? `${agent.adapter} · ${statusLabel(agent)} · Your own view into the session.` : 'A shared view of your agents at work.';
  $('composer-role').textContent = selected || 'an agent';
  $('operator-send').disabled = operatorPending || !agent || ['offline', 'exited'].includes(agent.status);
  for (const agent of state.agents) {
    const visible = split || agent.role === selected;
    if (visible && !terminals.has(agent.role)) terminals.set(agent.role, makeTerminal(agent));
    const item = terminals.get(agent.role);
    if (!item) continue;
    item.panel.hidden = !visible;
    if (visible) requestAnimationFrame(() => item.fit.fit());
  }
}
function rememberDraft() {
  if (!currentReview) return;
  drafts.set(currentReview, { text: $('review-text').value, to: $('review-to').value });
}
function renderInbox() {
  $('inbox-count').textContent = state.messages.length;
  if (!state.messages.some((m) => m.id === selectedMessage)) selectedMessage = state.messages[0]?.id || null;
  $('inbox-empty').hidden = state.messages.length > 0;
  $('review-form').hidden = !selectedMessage;
  $('inbox-list').replaceChildren(...state.messages.map((message) => {
    const button = node('button', `inbox-item${message.id === selectedMessage ? ' selected' : ''}`);
    button.type = 'button';
    button.setAttribute('aria-pressed', String(message.id === selectedMessage));
    const top = node('span', 'inbox-item-top');
    top.append(node('strong', '', message.from), node('span', 'inbox-arrow', '→'), node('span', '', message.to || 'Choose recipient'));
    button.append(top, node('p', '', message.uncertain ? 'Interrupted delivery · check recipient' : message.text.slice(0, 120)));
    button.addEventListener('click', () => { rememberDraft(); selectedMessage = message.id; renderInbox(); });
    return button;
  }));
  const message = state.messages.find((m) => m.id === selectedMessage);
  if (!message) { currentReview = null; return; }
  if (currentReview !== selectedMessage) {
    currentReview = selectedMessage;
    const options = [['', 'Choose a recipient…'], ...state.agents.map((a) => [a.role, a.role]), ['human', 'Human · keep with me'], ['done', 'Done · finish the chain']];
    $('review-to').replaceChildren(...options.map(([value, label]) => { const option = node('option', '', label); option.value = value; return option; }));
    const draft = drafts.get(selectedMessage);
    $('review-text').value = draft?.text ?? message.text;
    let to = draft?.to ?? message.to;
    if (['operator', 'none'].includes(to)) to = 'human';
    $('review-to').value = options.some(([value]) => value === to) ? to : '';
    $('busy-confirm').checked = false;
  }
  $('review-from').textContent = `From ${message.from}`;
  const time = Number(message.id.replace('.json', '')) / 1e6;
  $('review-time').textContent = Number.isFinite(time) ? new Date(time).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
  renderReviewControls();
}
function renderReviewControls() {
  const message = state.messages.find((m) => m.id === selectedMessage);
  if (!message) return;
  const to = $('review-to').value;
  const target = state.agents.find((a) => a.role === to);
  const parked = ['human', 'done'].includes(to);
  const unavailable = target && ['offline', 'exited'].includes(target.status);
  $('busy-confirm-label').hidden = !target?.busy || message.uncertain;
  $('review-warning').hidden = !message.uncertain && !unavailable;
  $('review-warning').textContent = message.uncertain ? 'This delivery was interrupted. Check the recipient’s terminal before dismissing it. Send any follow-up using the message box.' : unavailable ? 'This agent is no longer running. Choose another recipient or keep this message for yourself.' : '';
  $('send-message').textContent = parked ? 'Acknowledge ✓' : 'Approve & send ↗';
  $('send-message').disabled = actionPending || !to || message.uncertain || unavailable || (target?.busy && !$('busy-confirm').checked) || (!parked && !$('review-text').value.trim());
  $('drop-message').textContent = message.uncertain ? 'Dismiss' : 'Drop';
  $('drop-message').disabled = actionPending;
}
function renderHistory() {
  $('activity-list').replaceChildren(...state.history.map((entry) => {
    const item = node('article', 'activity-item');
    const meta = node('div', 'activity-meta');
    meta.append(node('strong', '', `${entry.from} → ${entry.to || 'unrouted'}`), node('span', 'activity-action', entry.action));
    item.append(meta, node('p', '', entry.text), node('time', '', new Date(Number(entry.time) * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })));
    return item;
  }));
  if (!state.history.length) $('activity-list').append(node('p', '', 'Reviewed messages will appear here.'));
}
async function refresh() {
  try {
    state = await api('/api/state');
    $('connection-label').textContent = 'Connected';
    $('connection-dot').classList.remove('disconnected');
    $('error-banner').hidden = true;
    const repoName = state.repo.split('/').filter(Boolean).at(-1);
    $('project-name').textContent = repoName;
    $('project-name').title = state.repo;
    document.title = `${repoName} · tmuxor`;
    if (!state.agents.some((a) => a.role === selected)) selected = state.agents[0]?.role;
    renderAgents(); renderTerminals(); renderInbox(); renderHistory();
  } catch (error) {
    $('connection-label').textContent = 'Disconnected';
    $('connection-dot').classList.add('disconnected');
    $('error-banner').textContent = error.status === 401 ? error.message : 'Dashboard connection lost. Your agent sessions keep running. Reconnecting…';
    $('error-banner').hidden = false;
  }
}
async function review(action) {
  if (!selectedMessage || actionPending) return;
  actionPending = true;
  renderReviewControls();
  const id = selectedMessage;
  try {
    const result = await api(`/api/messages/${encodeURIComponent(id)}`, { action, to: $('review-to').value, text: $('review-text').value, confirm_busy: $('busy-confirm').checked });
    drafts.delete(id);
    selectedMessage = null;
    toast(result.action === 'sent' ? `Sent to ${result.to}.` : result.action === 'dropped' ? 'Message dropped.' : 'Message acknowledged.');
  } catch (error) { toast(error.message, true); }
  finally { actionPending = false; await refresh(); }
}
$('review-form').addEventListener('submit', (event) => { event.preventDefault(); review('send'); });
$('drop-message').addEventListener('click', () => review('drop'));
$('review-text').addEventListener('input', () => { rememberDraft(); renderReviewControls(); });
$('review-to').addEventListener('change', () => { $('busy-confirm').checked = false; rememberDraft(); renderReviewControls(); });
$('busy-confirm').addEventListener('change', renderReviewControls);
$('operator-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (operatorPending) return;
  const agent = state.agents.find((a) => a.role === selected);
  const text = $('operator-text').value;
  if (!agent || !text.trim()) return;
  if (agent.busy && !confirm(`${agent.role} may still be working. Send this message now?`)) return;
  operatorPending = true;
  $('operator-send').disabled = true;
  try {
    await api(`/api/agents/${encodeURIComponent(agent.role)}/message`, { text, confirm_busy: agent.busy });
    if ($('operator-text').value === text) $('operator-text').value = '';
    toast(`Message sent to ${agent.role}.`);
  } catch (error) { toast(error.message, true); }
  finally { operatorPending = false; await refresh(); }
});
$('operator-text').addEventListener('keydown', (event) => { if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); $('operator-form').requestSubmit(); } });
for (const [id, value] of [['focus-view', false], ['split-view', true]]) {
  $(id).addEventListener('click', () => { split = value; sessionStorage.setItem(`${storageKey}:split`, String(split)); renderTerminals(); });
}
for (const name of ['inbox', 'activity']) {
  $(`${name}-tab`).addEventListener('click', () => {
    for (const other of ['inbox', 'activity']) { $(`${other}-tab`).setAttribute('aria-selected', String(other === name)); $(`${other}-panel`).hidden = other !== name; }
  });
}
$('inbox-toggle').addEventListener('click', () => { const hidden = document.querySelector('.workspace').classList.toggle('inbox-hidden'); $('inbox-toggle').setAttribute('aria-expanded', String(!hidden)); });
window.addEventListener('pagehide', () => {
  stopped = true;
  for (const item of terminals.values()) { clearTimeout(item.reconnect); item.closed = true; item.socket?.close(); item.observer.disconnect(); }
});
(async function poll() { await refresh(); if (!stopped) setTimeout(poll, 1500); })();
