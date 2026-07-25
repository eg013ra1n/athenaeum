// Perseus web UI v2 — client script.
//
// A framework-free, single-file vanilla app. The page shell (index.html) is a
// two-tab skeleton: this script renders the Transfers + Settings sections into
// the empty <main> panels, wires their controls, and drives the 2 s poll. Every
// Settings section (account/OTP, device name, capture dirs, send targets,
// retention) is ported behaviour-for-behaviour from the v1 page; only the DOM
// structure + class names changed. The upload-speed cap is the one section with
// no v1 ancestor (W1 T1.6). The Transfers tab ships its "To Sync" strip
// plus the unified one-row-per-batch transfer list (filter chips, the shared
// bottom detail pane with Files / Targets / Log sub-tabs, and the two delete
// actions), rendered by `refreshTransfers` off GET /api/transfers.

// ── shared helpers (token, fetch, formatters) ───────────────────────────────
// Bearer token kept in sessionStorage, not localStorage (finding L4): it is
// scoped to the tab/session and not persisted to disk in the browser profile,
// shrinking the window in which a shared machine or a future XSS could lift the
// LAN token. The operator re-enters it per browser session.
let token = sessionStorage.getItem('perseusToken') || '';
const $ = (id) => document.getElementById(id);
const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

async function api(path, opts = {}) {
  const headers = Object.assign({}, opts.headers || {});
  if (token) headers['Authorization'] = 'Bearer ' + token;
  let res = await fetch(path, Object.assign({}, opts, { headers }));
  if (res.status === 401) {
    const t = window.prompt('Bearer token required for this Perseus node:');
    if (t) { token = t; sessionStorage.setItem('perseusToken', t); return api(path, opts); }
  }
  return res;
}
async function getJson(path) { const r = await api(path); if (!r.ok) throw new Error(await r.text()); return r.json(); }

const shortHex = (h) => (h || '').slice(0, 8);
const fmtSize = (b) => { if (b == null) return '–'; const u = ['B', 'KB', 'MB', 'GB', 'TB']; let i = 0, n = b; while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; } return n.toFixed(i ? 1 : 0) + ' ' + u[i]; };
const fmtDur = (s) => (s == null ? '–' : s.toFixed(1) + 's');
// A live `m:ss` countdown to a server-supplied RFC3339 deadline, clamped at 0 (a
// past deadline reads "0:00", never negative). Re-evaluated every 2 s `tick`, so
// it stays honest against the client clock without any timer of its own. Empty
// string when the stamp doesn't parse.
function fmtCountdown(iso) {
  const t = Date.parse(iso);
  if (isNaN(t)) return '';
  const secs = Math.max(0, Math.round((t - Date.now()) / 1000));
  const m = Math.floor(secs / 60), s = secs % 60;
  return m + ':' + String(s).padStart(2, '0');
}

// ── tab shell ───────────────────────────────────────────────────────────────
// Two tabs, active one remembered in localStorage. Switching toggles `hidden` on
// the panels + an `active` class on the buttons — no router, no history.
function setTab(name) {
  const transfers = name !== 'settings';
  activeTab = transfers ? 'transfers' : 'settings';
  $('tab-transfers').hidden = !transfers;
  $('tab-settings').hidden = transfers;
  $('tabBtnTransfers').classList.toggle('active', transfers);
  $('tabBtnSettings').classList.toggle('active', !transfers);
  try { localStorage.setItem('perseus.tab', transfers ? 'transfers' : 'settings'); } catch (e) { /* private mode */ }
  // Entering the Transfers tab: refresh at once (the /api/transfers poll is
  // gated to this tab, so it is idle while Settings is showing).
  if (transfers && window.PerseusApp) window.PerseusApp.refreshTransfers();
}

function wireTabs() {
  document.querySelectorAll('.tab').forEach((b) => b.addEventListener('click', () => setTab(b.dataset.tab)));
  let saved = 'transfers';
  try { saved = localStorage.getItem('perseus.tab') || 'transfers'; } catch (e) { /* private mode */ }
  setTab(saved);
}

// ── section markup (rendered into the empty <main> panels on boot) ───────────
function renderTransfersTab() {
  // The "To sync" strip: the pending accumulator as a rel_path tree (collapsed
  // by default, expanded by clicking the counter), the live Auto↔Manual toggle,
  // the auto quiet-window input, and the manual "Send N pending" button. Polled
  // on the 2 s tick; the toggle/quiet input are not clobbered while focused.
  // Below it, the `#transfers` section holds the unified one-row-per-batch list:
  // filter chips (with live counts), the list body, and the shared bottom detail
  // pane. Its body/chips/pane are re-rendered by refreshTransfers().
  $('tab-transfers').innerHTML = `
    <section id="tosync">
      <h2>To Sync</h2>
      <div class="row" style="margin-bottom:0.5rem;">
        <div class="seg" role="radiogroup" aria-label="Send mode">
          <label><input type="radio" name="sendMode" id="modeAuto" value="auto" /> Auto</label>
          <label><input type="radio" name="sendMode" id="modeManual" value="manual" /> Manual</label>
        </div>
        <label id="quietWrap" class="inline-label">
          quiet window (s)
          <input id="quietSecs" type="number" min="1" class="qty" aria-label="Auto quiet window in seconds" />
        </label>
        <button class="counter" id="pendingToggle" aria-expanded="false" aria-controls="pendingTree">
          <span id="pendingCaret">&#9656;</span> <span id="pendingCount">0</span> pending
        </button>
        <span class="spacer"></span>
        <button id="sendNow" disabled>Send 0 pending</button>
      </div>
      <div class="flash" id="tosyncFlash"></div>
      <div id="pendingTree" hidden><div class="empty">nothing pending — all captures sent</div></div>
    </section>
    <section id="transfers">
      <div class="transfers-head">
        <h2>Transfers</h2>
        <div id="transferChips" class="tchips" role="tablist" aria-label="Filter transfers"></div>
      </div>
      <div class="flash" id="transferFlash"></div>
      <div id="transferListBody"></div>
      <div id="transferDetail" class="tdetail" hidden></div>
    </section>`;
}

function renderSettingsTab() {
  // Account (email → OTP via the hub) + device name; capture dirs; send targets;
  // retention. Elements toggled via JS `.style.display` carry an initial inline
  // `display:none` (the flow the ported handlers drive); everything else is
  // styled by class in style.css.
  $('tab-settings').innerHTML = `
    <section id="account">
      <h2>Account</h2>
      <div id="acctSignedOut" style="display:none;">
        <div class="row">
          <input id="acctEmail" type="email" placeholder="you@example.com" size="28" autocomplete="email" />
          <button id="acctSendCode">Send code</button>
        </div>
        <div class="row" id="acctCodeRow" style="display:none; margin-top:0.5rem;">
          <input id="acctCode" placeholder="6-digit code" size="14" inputmode="numeric" autocomplete="one-time-code" />
          <button id="acctVerify">Sign in</button>
          <button id="acctResend" class="ghost">Send again</button>
        </div>
      </div>
      <div id="acctSignedIn" style="display:none;">
        <div class="row">
          <div>
            <div><b id="acctSignedInEmail"></b></div>
            <div class="muted" id="acctPrimary"></div>
          </div>
          <span class="spacer"></span>
          <button id="acctSignOut" class="ghost">Sign out</button>
          <button id="acctSignOutConfirm" class="danger" style="display:none;">Confirm sign-out</button>
        </div>
      </div>
      <div class="flash" id="acctFlash"></div>
      <div class="row divider">
        <input id="devName" placeholder="(hostname default)" size="24" autocomplete="off" />
        <button id="devNameSave" class="ghost">Save name</button>
        <span class="muted">this node's name in your account</span>
      </div>
      <div class="flash" id="devNameFlash"></div>
    </section>

    <section id="capture-dirs">
      <h2>Capture Directories</h2>
      <div class="tablewrap"><table>
        <thead><tr><th>directory</th><th></th></tr></thead>
        <tbody id="cdList"></tbody>
      </table></div>
      <div class="row" style="margin-top:0.5rem;">
        <input id="cdAddInput" placeholder="/path/to/capture" size="36" />
        <button id="cdAdd" class="ghost">Add</button>
        <button id="cdSave">Save</button>
      </div>
      <div class="flash" id="cdFlash"></div>
    </section>

    <section id="targets-sec">
      <h2>Send Targets</h2>
      <div class="muted target-intro">Devices in your account this node sends captures to. Pick from your devices below; Perseus capture agents are send-only and never listed.</div>
      <div id="tgChips" class="chips"></div>
      <div class="row" style="margin-top:0.6rem;">
        <select id="tgSelect" class="tg-select"></select>
        <button id="tgAddSel" class="ghost">Add</button>
        <button id="tgRefresh" class="ghost" title="Refresh device list">&#8635;</button>
      </div>
      <div class="muted picker-hint" id="tgPickerHint"></div>
      <details class="advanced">
        <summary class="muted">Advanced: add by device name or id</summary>
        <div class="row" style="margin-top:0.4rem;">
          <input id="tgAddInput" placeholder="device name or id" size="30" />
          <button id="tgAdd" class="ghost">Add</button>
        </div>
      </details>
      <div class="row" style="margin-top:0.6rem;">
        <button id="tgSave">Save</button>
      </div>
      <div class="flash" id="tgFlash"></div>
    </section>

    <section id="upload-limit">
      <h2>Upload Speed</h2>
      <div class="muted target-intro">Caps the total sync upload rate so a big transfer leaves the site's uplink usable (SSH, remote desktop). 0 = unlimited. Applies immediately, mid-transfer included.</div>
      <div class="row">
        <label class="inline-label">Upload speed limit (MB/s)
          <input id="ulLimit" type="number" min="0" step="1" class="qty" aria-label="Upload speed limit in whole megabytes per second, 0 for unlimited" />
        </label>
        <button id="ulSave">Save</button>
      </div>
      <div class="flash" id="ulFlash"></div>
    </section>

    <section id="retention">
      <h2>Retention</h2>
      <div class="indicators">
        <span class="ind">soak opt-in: <b id="iSoak">–</b></span>
        <span class="ind">live deletion: <b id="iLive">–</b></span>
        <span class="muted">Live deletion is enabled in perseus.toml only</span>
      </div>
      <div class="form-grid">
        <label>policy
          <select id="fPolicy">
            <option value="keep_everything">keep_everything</option>
            <option value="on_confirm">on_confirm</option>
            <option value="keep_days">keep_days</option>
            <option value="disk_pct">disk_pct</option>
          </select>
        </label>
        <label>keep_days<input id="fKeepDays" type="number" min="1" /></label>
        <label>disk_max_pct<input id="fDiskPct" type="number" min="1" max="100" /></label>
        <label>interval_secs<input id="fInterval" type="number" min="1" /></label>
        <label>dry-run<select id="fDryRun"><option value="true">true</option><option value="false">false</option></select></label>
        <button id="saveRet">Save</button>
      </div>
      <div class="flash" id="retflash"></div>
      <h2 class="section-sub">Recent retention passes</h2>
      <ul class="log" id="retlog"><li class="empty">no passes recorded yet</li></ul>
    </section>`;
}

// ── status (connection dot + lifecycle banner) ──────────────────────────────
// The lifecycle banner is global to both tabs, so it is inserted just below the
// header (not inside a tab panel). Both the dot and the banner are driven by the
// /api/status poll.
function mountBanner() {
  document.querySelector('header').insertAdjacentHTML(
    'afterend',
    '<div class="banner" id="agent-banner" style="display:none;"></div>'
  );
}

// The lifecycle banner. Hidden while the engine is running normally; otherwise it
// surfaces the supervisor's agentState: yellow for setup, blue for starting, red
// for a failed launch, and a yellow "applying…" note while a saved capture-dir
// edit awaits its restart.
function renderAgentBanner(s) {
  const b = $('agent-banner');
  if (s.restartPending) {
    b.className = 'banner warn';
    b.textContent = 'Applying capture folder changes… restarting the sync engine';
    b.style.display = '';
    return;
  }
  switch (s.agentState) {
    case 'running':
      b.style.display = 'none';
      break;
    case 'needs_setup':
      b.className = 'banner warn';
      b.textContent = 'Setup required: ' + (s.agentDetail || 'finish setup to start syncing');
      b.style.display = '';
      break;
    case 'starting':
      b.className = 'banner info';
      b.textContent = 'Starting…';
      b.style.display = '';
      break;
    case 'failed':
      b.className = 'banner err';
      b.textContent = 'Sync engine failed: ' + (s.agentDetail || 'see the logs');
      b.style.display = '';
      break;
    default:
      b.style.display = 'none';
  }
}

async function refreshStatus() {
  try {
    const s = await getJson('/api/status');
    $('connDot').className = 'conn-dot ok';
    $('conn').textContent = 'connected';
    renderAgentBanner(s);
  } catch (e) {
    $('connDot').className = 'conn-dot err';
    $('conn').textContent = 'offline: ' + e.message;
  }
}

// ── retention (policy editor + recent-pass log) ─────────────────────────────
async function loadPolicy() {
  try {
    const p = await getJson('/api/retention/policy');
    $('fPolicy').value = p.policy;
    $('fKeepDays').value = p.keepDays;
    $('fDiskPct').value = p.diskMaxPct;
    $('fInterval').value = p.intervalSecs;
    $('fDryRun').value = String(p.dryRun);
    $('iSoak').textContent = p.soakOptIn ? 'yes' : 'no';
    $('iLive').textContent = p.liveDeletionPossible ? 'ACTIVE' : 'off (dry-run)';
    $('iLive').style.color = p.liveDeletionPossible ? 'var(--error)' : 'var(--content-muted)';
  } catch (e) { const f = $('retflash'); f.textContent = 'load failed: ' + e.message; f.className = 'flash err'; }
}

async function savePolicy() {
  const edit = {
    policy: $('fPolicy').value,
    keepDays: Number($('fKeepDays').value),
    diskMaxPct: Number($('fDiskPct').value),
    intervalSecs: Number($('fInterval').value),
    dryRun: $('fDryRun').value === 'true',
  };
  const f = $('retflash');
  try {
    const r = await api('/api/retention/policy', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(edit) });
    if (r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    f.textContent = 'saved'; f.className = 'flash ok';
    loadPolicy();
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

async function refreshLog() {
  try {
    const rows = await getJson('/api/retention/log');
    $('retlog').innerHTML = rows.length
      ? rows.map((r) => {
          const bits = [`${r.dryRun ? 'dry-run' : 'LIVE'}`, `${r.policy}`, `deleted ${r.deleted.length}`, `eligible ${r.wouldDelete.length}`];
          if (r.errors.length) bits.push('⚠ ' + r.errors.map(esc).join('; '));
          return `<li class="logline"><span class="mono muted">${esc(r.at)}</span> — ${bits.map(esc).join(' · ')}</li>`;
        }).join('')
      : '<li class="empty">no passes recorded yet</li>';
  } catch (e) { $('retlog').innerHTML = `<li class="empty">error: ${esc(e.message)}</li>`; }
}

// ── capture directories (apply live via supervisor restart) ─────────────────
// `cdWorking` is the local editable list. It is loaded from the server on page
// load and re-synced after each Save, and is mutated in place by the Add/Remove
// buttons — the 2 s tick never touches the list, so an in-progress add/remove is
// never clobbered mid-edit. The "applying…" state is the shared agent-banner
// below the header (driven by `/api/status` restartPending), so this editor has
// no banner of its own.
let cdWorking = [];

function renderCaptureDirs() {
  const tb = $('cdList');
  tb.innerHTML = cdWorking.length
    ? cdWorking.map((d, i) => `<tr><td class="mono">${esc(d)}</td><td><button class="ghost" data-cdrm="${i}">Remove</button></td></tr>`).join('')
    : '<tr><td colspan="2" class="empty">no capture directories — add at least one before saving</td></tr>';
  document.querySelectorAll('[data-cdrm]').forEach((b) => b.addEventListener('click', () => {
    cdWorking.splice(Number(b.dataset.cdrm), 1);
    renderCaptureDirs();
  }));
}

async function loadCaptureDirs() {
  try {
    const c = await getJson('/api/capture-dirs');
    cdWorking = c.configured.slice();
    renderCaptureDirs();
  } catch (e) { const f = $('cdFlash'); f.textContent = 'load failed: ' + e.message; f.className = 'flash err'; }
}

function addCaptureDir() {
  const v = $('cdAddInput').value.trim();
  if (!v) return;
  cdWorking.push(v);
  $('cdAddInput').value = '';
  renderCaptureDirs();
}

async function saveCaptureDirs() {
  const f = $('cdFlash');
  try {
    const r = await api('/api/capture-dirs', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ dirs: cdWorking }) });
    if (r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    const c = await r.json();
    cdWorking = c.configured.slice();
    renderCaptureDirs();
    f.textContent = 'saved'; f.className = 'flash ok';
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

function wireCaptureDirs() {
  $('cdAdd').addEventListener('click', addCaptureDir);
  $('cdAddInput').addEventListener('keydown', (e) => { if (e.key === 'Enter') addCaptureDir(); });
  $('cdSave').addEventListener('click', saveCaptureDirs);
}

// ── send targets (device picker; restart-to-apply, mirrors capture dirs) ─────
// `tgWorking` is the local editable list, loaded on page load and re-synced after
// each Save; the picker/free-text Add and the chip × mutate it in place.
// `tgOptions` is the receiver-capable device list from /api/targets/options,
// fetched only on load / sign-in change / manual ↻ — NEVER on the 2 s tick (each
// fetch proxies a hub call). Stored targets are device IDs (rename-robust); legacy
// name entries still resolve. Targets are bound at engine spawn, so a save is
// restart-to-apply.
let tgWorking = [];
let tgOptions = [];             // [{ id, name, capability }] receiver candidates
let tgOptError = null;          // hub-failure string, or null
let tgAccountSignedIn = null;   // null = unknown yet; drives picker enable/hint

// Resolve a stored target entry (id, or a legacy name) to a display label via the
// options list; fall back to the raw string (offline, or not in the list).
function tgLabel(entry) {
  const hit = tgOptions.find((d) => d.id === entry) || tgOptions.find((d) => d.name === entry);
  return hit ? hit.name : entry;
}

function renderTargets() {
  const box = $('tgChips');
  if (!tgWorking.length) {
    box.innerHTML = '<div class="empty">no send targets — pick a device below (or add one in Advanced)</div>';
    return;
  }
  box.innerHTML = tgWorking.map((t, i) =>
    `<span class="chip target"><span>${esc(tgLabel(t))}</span>`
    + `<button class="chipx" data-tgrm="${i}" title="Remove" aria-label="Remove">&times;</button></span>`
  ).join('');
  box.querySelectorAll('[data-tgrm]').forEach((b) => b.addEventListener('click', () => {
    tgWorking.splice(Number(b.dataset.tgrm), 1);
    renderTargets();
    renderTargetSelect();
  }));
}

// Populate the dropdown with receiver-capable devices not already added. Disabled
// with a hint when signed out, when the hub list failed, or when there is nothing
// left to add.
function renderTargetSelect() {
  const sel = $('tgSelect');
  const add = $('tgAddSel');
  const hint = $('tgPickerHint');
  const disable = (msg, hintText) => {
    sel.innerHTML = `<option value="">${esc(msg)}</option>`;
    sel.disabled = true; add.disabled = true; hint.textContent = hintText || '';
  };
  if (tgAccountSignedIn === null) { disable('loading…'); return; }
  if (tgAccountSignedIn === false) { disable('sign in to list your devices'); return; }
  if (tgOptError) { disable('could not load devices', tgOptError + ' — try ↻'); return; }
  const added = new Set(tgWorking);
  const avail = tgOptions.filter((d) => !added.has(d.id) && !added.has(d.name));
  if (!avail.length) {
    disable(tgOptions.length ? 'all devices added' : 'no receiver devices in your account');
    return;
  }
  sel.innerHTML = avail.map((d) => `<option value="${esc(d.id)}">${esc(d.name)}</option>`).join('');
  sel.disabled = false; add.disabled = false; hint.textContent = '';
}

async function loadTargetOptions() {
  try {
    const o = await getJson('/api/targets/options');
    tgOptions = Array.isArray(o.devices) ? o.devices : [];
    tgOptError = o.error || null;
  } catch (e) {
    tgOptions = []; tgOptError = e.message;
  }
  renderTargets();        // labels may now resolve id → name
  renderTargetSelect();
}

async function loadTargets() {
  try {
    const c = await getJson('/api/targets');
    tgWorking = c.configured.slice();
    renderTargets();
    renderTargetSelect();
  } catch (e) { const f = $('tgFlash'); f.textContent = 'load failed: ' + e.message; f.className = 'flash err'; }
}

function addTargetFromSelect() {
  const v = $('tgSelect').value;
  if (!v) return;
  if (!tgWorking.includes(v)) tgWorking.push(v);
  renderTargets();
  renderTargetSelect();
}

function addTarget() {
  const v = $('tgAddInput').value.trim();
  if (!v) return;
  if (!tgWorking.includes(v)) tgWorking.push(v);
  $('tgAddInput').value = '';
  renderTargets();
  renderTargetSelect();
}

async function saveTargets() {
  const f = $('tgFlash');
  try {
    const r = await api('/api/targets', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ targets: tgWorking }) });
    if (r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    const c = await r.json();
    tgWorking = c.configured.slice();
    renderTargets();
    renderTargetSelect();
    f.textContent = c.restartPending ? 'saved — restarting the sync engine to apply' : 'saved';
    f.className = 'flash ok';
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

function wireTargets() {
  $('tgAddSel').addEventListener('click', addTargetFromSelect);
  $('tgRefresh').addEventListener('click', loadTargetOptions);
  $('tgAdd').addEventListener('click', addTarget);
  $('tgAddInput').addEventListener('keydown', (e) => { if (e.key === 'Enter') addTarget(); });
  $('tgSave').addEventListener('click', saveTargets);
}

// ── device name (this node's name in the account) ───────────────────────────
async function loadDeviceName() {
  try {
    const d = await getJson('/api/device-name');
    // Never clobber the field while the operator is typing in it.
    if (document.activeElement !== $('devName')) $('devName').value = d.deviceName || '';
  } catch (e) { /* offline surfaced by refreshStatus() */ }
}

async function saveDeviceName() {
  const f = $('devNameFlash');
  try {
    const r = await api('/api/device-name', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ name: $('devName').value }) });
    if (r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    const d = await r.json();
    $('devName').value = d.deviceName || '';
    if (d.hubError) { f.textContent = d.hubError; f.className = 'flash err'; }
    else { f.textContent = d.deviceName ? 'saved' : 'saved — using the hostname default'; f.className = 'flash ok'; }
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

function wireDeviceName() {
  $('devNameSave').addEventListener('click', saveDeviceName);
  $('devName').addEventListener('keydown', (e) => { if (e.key === 'Enter') saveDeviceName(); });
}

// ── upload speed limit (decimal MB/s; 0 = unlimited) ────────────────────────
// Loaded once at boot (nothing else moves it) and never clobbered while the
// operator is typing. The PUT applies live on the running node; when the engine
// is detached the server reports appliedLive:false and the flash says so rather
// than implying a cap that did not take effect yet.
async function loadUploadLimit() {
  try {
    const d = await getJson('/api/upload-limit');
    if (document.activeElement !== $('ulLimit')) $('ulLimit').value = d.maxUploadMbps;
  } catch (e) { /* offline surfaced by refreshStatus() */ }
}

async function saveUploadLimit() {
  const f = $('ulFlash');
  // The field is a whole-MB/s u32 server-side. Rounding a fractional entry would
  // be actively dangerous — 0.5 would floor to 0, which means UNLIMITED, the exact
  // opposite of what was typed. So reject it here and send nothing.
  const raw = $('ulLimit').value.trim();
  const mbps = Number(raw);
  if (raw === '' || !Number.isInteger(mbps) || mbps < 0) {
    f.textContent = 'whole MB/s (minimum 1); 0 = unlimited';
    f.className = 'flash err';
    return;
  }
  try {
    const r = await api('/api/upload-limit', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ maxUploadMbps: mbps }) });
    if (r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    const d = await r.json();
    $('ulLimit').value = d.maxUploadMbps;
    const what = d.maxUploadMbps ? `capped at ${d.maxUploadMbps} MB/s` : 'unlimited';
    f.textContent = d.appliedLive ? `saved — ${what}` : `saved — ${what} (applies when the sync engine starts)`;
    f.className = 'flash ok';
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

function wireUploadLimit() {
  $('ulSave').addEventListener('click', saveUploadLimit);
  $('ulLimit').addEventListener('keydown', (e) => { if (e.key === 'Enter') saveUploadLimit(); });
}

// ── account (email → OTP sign-in) ───────────────────────────────────────────
// Two visual states from /api/account. Errors surface inline in the shared
// #acctFlash (the page's .flash.err helper), never a blocking dialog. The 2 s
// tick refreshes the card, but skips the render while the operator is interacting
// with it (focus inside the card) so typing / the two-click sign-out are never
// clobbered mid-action.
function acctFlash(msg, ok) { const f = $('acctFlash'); f.textContent = msg; f.className = 'flash ' + (ok ? 'ok' : 'err'); }

function renderAccount(a) {
  const inGate = !!a.signedIn;
  $('acctSignedIn').style.display = inGate ? '' : 'none';
  $('acctSignedOut').style.display = inGate ? 'none' : '';
  if (inGate) {
    $('acctSignedInEmail').textContent = a.email || '(signed in)';
    // Sync 2C mesh model: no per-account "primary" — show this node's own hub
    // device id instead (send targets live in their own editor).
    $('acctPrimary').textContent = a.deviceId ? 'device id ' + shortHex(a.deviceId) : '';
    // Reset the two-click sign-out to its resting state.
    $('acctSignOut').style.display = '';
    $('acctSignOutConfirm').style.display = 'none';
  }
  // Send-targets picker: refetch the device list only when the signed-in state
  // flips (sign-in → list devices; sign-out → clear). The 2 s tick calls
  // renderAccount but this guard keeps it from ever hub-calling.
  if (inGate !== tgAccountSignedIn) {
    tgAccountSignedIn = inGate;
    if (inGate) {
      loadTargetOptions();
    } else {
      tgOptions = []; tgOptError = null;
      renderTargets();
      renderTargetSelect();
    }
  }
}

async function refreshAccount() {
  // Never re-render while the operator is mid-entry in the card — it would wipe
  // the email/code inputs (or revert the two-click sign-out).
  if ($('account').contains(document.activeElement)) return;
  try { renderAccount(await getJson('/api/account')); }
  catch (e) { /* offline is surfaced by refreshStatus() */ }
}

async function acctSendCode() {
  const email = $('acctEmail').value.trim();
  if (!email) { acctFlash('enter your account email', false); return; }
  try {
    const r = await api('/api/account/request-code', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email }) });
    if (!r.ok) throw new Error(await r.text());
    $('acctCodeRow').style.display = '';
    $('acctCode').focus();
    acctFlash('code sent to ' + email + ' — check your inbox', true);
  } catch (e) { acctFlash('could not send code: ' + e.message, false); }
}

async function acctVerify() {
  const email = $('acctEmail').value.trim();
  const code = $('acctCode').value.trim();
  if (!code) { acctFlash('enter the code from your email', false); return; }
  try {
    const r = await api('/api/account/verify', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email, code }) });
    if (!r.ok) throw new Error(await r.text());
    const a = await r.json();
    $('acctCode').value = '';
    $('acctCodeRow').style.display = 'none';
    renderAccount(a);
    acctFlash('signed in', true);
    // Readiness changed — re-fetch status + account so the banner + engine state
    // catch up at once (the server also woke the supervisor).
    refreshStatus(); refreshAccount();
  } catch (e) { acctFlash('sign-in failed: ' + e.message, false); }
}

async function acctSignOut() {
  try {
    const r = await api('/api/account/logout', { method: 'POST' });
    if (!r.ok) throw new Error(await r.text());
    // Render the signed-out state directly (mirrors acctVerify()) instead of
    // relying on refreshAccount(): its focus-guard skips the re-render while focus
    // is inside #account, and "Confirm sign-out" still holds focus at this point —
    // without this the card would keep showing the signed-in email until focus
    // moved elsewhere.
    $('acctSignOutConfirm').blur();
    acctFlash('signed out', true);
    // The logout call already succeeded — a hiccup on this follow-up GET is not a
    // sign-out failure, so it is swallowed here (same as refreshAccount()'s own
    // error handling) rather than overwriting the flash above with a false
    // "failed" message.
    try { renderAccount(await getJson('/api/account')); } catch (e) { /* offline surfaced by refreshStatus() */ }
    // Readiness changed — re-fetch status so the banner/engine state catch up at
    // once (the server also woke the supervisor).
    refreshStatus();
  } catch (e) { acctFlash('sign-out failed: ' + e.message, false); }
}

function wireAccount() {
  $('acctSendCode').addEventListener('click', acctSendCode);
  $('acctResend').addEventListener('click', acctSendCode);
  $('acctVerify').addEventListener('click', acctVerify);
  $('acctEmail').addEventListener('keydown', (e) => { if (e.key === 'Enter') acctSendCode(); });
  $('acctCode').addEventListener('keydown', (e) => { if (e.key === 'Enter') acctVerify(); });
  // Two-click sign-out — no blocking confirm() dialog. The confirm button is
  // focused so the tick's focus-guard holds it until the operator acts.
  $('acctSignOut').addEventListener('click', () => {
    $('acctSignOut').style.display = 'none';
    $('acctSignOutConfirm').style.display = '';
    $('acctSignOutConfirm').focus();
  });
  $('acctSignOutConfirm').addEventListener('click', acctSignOut);
}

function wireRetention() {
  $('saveRet').addEventListener('click', savePolicy);
}

// ── to sync: pending tree + Auto/Manual toggle + manual send ────────────────
// `pendingCount` is the last-fetched total; the Send button label + enabled state
// derive from it plus the currently-selected mode. `lastPendingJson` guards the
// tree render so an unchanged pending set doesn't collapse the operator's expanded
// <details> every 2 s tick. The tree itself is collapsed by default and toggled by
// clicking the pending counter.
let pendingCount = 0;
let lastPendingJson = null;

const currentMode = () => ($('modeManual').checked ? 'manual' : 'auto');

function updateQuietVisibility() {
  // The quiet window is inert in manual mode — only show it for Auto.
  $('quietWrap').style.display = currentMode() === 'auto' ? '' : 'none';
}

function updateSendButton() {
  const manual = currentMode() === 'manual';
  const btn = $('sendNow');
  btn.textContent = `Send ${pendingCount} pending`;
  // Auto flushes on its own quiet timer → the manual button is hidden. In Manual
  // it is enabled only when there is something to flush.
  btn.style.display = manual ? '' : 'none';
  btn.disabled = !(manual && pendingCount > 0);
}

function applyModeControls(mode, quietSecs) {
  $('modeAuto').checked = mode !== 'manual';
  $('modeManual').checked = mode === 'manual';
  // Never clobber the quiet input while the operator is typing in it.
  if (document.activeElement !== $('quietSecs')) $('quietSecs').value = quietSecs;
  updateQuietVisibility();
  updateSendButton();
}

function renderNode(node) {
  const label = `${esc(node.name)} <span class="muted">(${node.count})</span>`;
  const childHtml = node.children.map(renderNode).join('');
  const fileHtml = node.files.length
    ? `<ul class="tree-files">${node.files.map((f) => `<li class="mono">${esc(f)}</li>`).join('')}</ul>`
    : '';
  return `<details open class="tree-node"><summary>${label}</summary><div class="tree-body">${childHtml}${fileHtml}</div></details>`;
}

function renderPendingTree(root) {
  // Skip the re-render (and preserve expand state) when nothing changed.
  const json = JSON.stringify(root);
  if (json === lastPendingJson) return;
  lastPendingJson = json;
  const container = $('pendingTree');
  if (!root || root.count === 0) {
    container.innerHTML = '<div class="empty">nothing pending — all captures sent</div>';
    return;
  }
  // The root is the synthetic empty node — render its children/files at the top
  // level rather than showing a blank summary for it.
  const childHtml = root.children.map(renderNode).join('');
  const fileHtml = root.files.length
    ? `<ul class="tree-files">${root.files.map((f) => `<li class="mono">${esc(f)}</li>`).join('')}</ul>`
    : '';
  container.innerHTML = childHtml + fileHtml;
}

async function refreshPending() {
  try {
    const p = await getJson('/api/pending');
    pendingCount = p.count;
    $('pendingCount').textContent = p.count;
    // Don't clobber the toggle/quiet input while the operator is interacting with
    // the card; the tree + button label always reflect the live count.
    if (!$('tosync').contains(document.activeElement)) applyModeControls(p.mode, p.autoQuietSecs);
    else updateSendButton();
    renderPendingTree(p.tree);
  } catch (e) {
    $('pendingTree').innerHTML = `<div class="empty">error: ${esc(e.message)}</div>`;
  }
}

async function saveSendMode() {
  const edit = { mode: currentMode(), autoQuietSecs: Number($('quietSecs').value) || 0 };
  const f = $('tosyncFlash');
  try {
    const r = await api('/api/send-mode', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(edit) });
    if (r.status === 400 || r.status === 422) { f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err'; return; }
    if (!r.ok) throw new Error(await r.text());
    const m = await r.json();
    applyModeControls(m.mode, m.autoQuietSecs);
    f.textContent = 'saved'; f.className = 'flash ok';
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
}

async function sendNow() {
  const f = $('tosyncFlash');
  try {
    const r = await api('/api/send-now', { method: 'POST' });
    if (!r.ok) throw new Error(await r.text());
    const rep = await r.json();
    f.textContent = rep.flushed > 0 ? `flushed ${rep.flushed} pending` : 'nothing pending to send';
    f.className = 'flash' + (rep.flushed > 0 ? ' ok' : '');
    // Reflect the flush at once: the tree empties and the new transfer appears in
    // the Transfers list (filled by a later task; a no-op refresh until then).
    refreshPending(); window.PerseusApp.refreshTransfers();
  } catch (e) { f.textContent = 'send failed: ' + e.message; f.className = 'flash err'; }
}

function wireTosync() {
  document.querySelectorAll('input[name="sendMode"]').forEach((r) => r.addEventListener('change', () => {
    updateQuietVisibility(); updateSendButton(); saveSendMode();
  }));
  $('quietSecs').addEventListener('change', saveSendMode);
  $('quietSecs').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); saveSendMode(); } });
  $('sendNow').addEventListener('click', sendNow);
  // The pending tree is collapsed by default; the counter toggles it.
  $('pendingToggle').addEventListener('click', () => {
    const tree = $('pendingTree');
    const open = tree.hidden;               // becomes open
    tree.hidden = !open;
    $('pendingToggle').setAttribute('aria-expanded', String(open));
    $('pendingCaret').innerHTML = open ? '&#9662;' : '&#9656;';
  });
}

// ── transfers: unified one-row-per-batch list + detail pane + delete actions ──
// The Transfers tab's list, filled from GET /api/transfers (Task 4 DTO). It is
// pure read-model rendering off `transfersData`; every mutation (per-target
// retry/kick/cancel/resend, source-file delete, history delete) POSTs then calls
// refreshTransfers() so the server truth drives the next render. State lives in a
// handful of module vars so a 2 s poll can re-render without losing the operator's
// filter selection or open detail pane, and a JSON signature skips the re-render
// (and its reflow) when nothing changed.
const TERMINAL_STATES = new Set(['confirmed', 'failed', 'cancelled']);
const TRANSFER_FILTERS = [
  ['all', 'All'], ['sending', 'Sending'], ['waiting', 'Waiting'],
  ['completed', 'Completed'], ['cancelled', 'Cancelled'], ['failed', 'Failed'],
];
let activeTab = 'transfers';        // gates the /api/transfers poll to its tab
let transfersData = [];             // last GET /api/transfers payload
let transferFilter = 'all';         // active filter chip
let openTransferRef = null;         // packageRef of the open detail pane (or null)
let openTransferSubTab = 'files';   // 'files' | 'targets' | 'log'
const transferEvents = {};          // packageRef -> events (cached until pane closes)
let transferDialogOpen = false;     // an in-page confirm is showing → suspend re-render
let lastTransferSig = null;         // last render signature (skip unchanged reflows)

// A receiver-declined target: cancelled AND the last error is the receiver's own
// decline (the divert-eligibility key the resend-as-new button needs).
const isDeclinedTarget = (t) => t.state === 'cancelled' && !!t.lastError && t.lastError.startsWith('cancelled by receiver');
// `waiting` membership: any target non-terminal with an armed retry deadline.
const batchIsWaiting = (b) => (b.targets || []).some((t) => !TERMINAL_STATES.has(t.state) && t.nextRetryAt);
// Every visible outbound row terminal (or none in the window) → history-deletable.
const batchAllTerminal = (b) => (b.targets || []).every((t) => TERMINAL_STATES.has(t.state));
const transferName = (b) => (b.displayName && b.displayName.trim()) ? b.displayName : shortHex(b.batchUuid);

function transferFlash(msg, ok) {
  const f = $('transferFlash');
  if (!f) return;
  f.textContent = msg;
  f.className = 'flash ' + (ok ? 'ok' : 'err');
}

function batchInFilter(b, f) {
  switch (f) {
    case 'sending': return b.outcome === 'sending';
    case 'waiting': return batchIsWaiting(b);
    case 'completed': return b.outcome === 'confirmed';
    case 'cancelled': return b.outcome === 'cancelled';
    case 'failed': return b.outcome === 'failed';
    default: return true; // 'all'
  }
}

// ── render: filter chips ──
function renderTransferChips() {
  $('transferChips').innerHTML = TRANSFER_FILTERS.map(([key, label]) => {
    const n = transfersData.filter((b) => batchInFilter(b, key)).length;
    const active = key === transferFilter;
    return `<button class="tfilter${active ? ' active' : ''}" data-filter="${key}" role="tab" aria-selected="${active}">`
      + `${label}<span class="tfilter-n">${n}</span></button>`;
  }).join('');
}

// ── render: one batch row ──
// State colours ride the shared `.chip.<state>` classes (accent for in-flight,
// success for confirmed/delivered, warning for waiting, error for declined,
// muted for cancelled). A waiting target's countdown lives in a `data-countdown`
// span updated in place every tick (no full re-render).
function targetChipHtml(t) {
  const declined = isDeclinedTarget(t);
  const nonTerminal = !TERMINAL_STATES.has(t.state);
  const waiting = nonTerminal && t.nextRetryAt;
  let cls, inner;
  if (declined) {
    cls = 'declined'; inner = `${esc(t.name)} · declined`;
  } else if (waiting) {
    cls = 'waiting';
    inner = `${esc(t.name)} · retry <span class="cd" data-countdown="${esc(t.nextRetryAt)}">${esc(fmtCountdown(t.nextRetryAt) || '…')}</span>`;
  } else {
    let label = t.state;
    if (nonTerminal && t.byteSize > 0 && ['announced', 'transferring', 'delivered'].includes(t.state)) {
      label += ' ' + Math.floor(100 * t.bytesDone / t.byteSize) + '%';
    }
    cls = t.state; inner = `${esc(t.name)} · ${esc(label)}`;
  }
  return `<span class="chip ${esc(cls)}">${inner}</span>`;
}

function transferRowHtml(b) {
  const open = b.packageRef === openTransferRef;
  const markers = [];
  if (b.generation > 1) markers.push(`<span class="tmarker">attempt ${b.generation}</span>`);
  if (b.filesDeletedAt) markers.push(`<span class="tmarker files-deleted">files deleted</span>`);
  const targetChips = (b.targets || []).map(targetChipHtml).join(' ');
  const d = b.deletable || {};
  const dfTitle = d.allowed
    ? 'Delete the source capture files for this batch from disk'
    : ('Cannot delete files: ' + (d.blockers || []).join('; '));
  const dfBtn = `<button class="ghost" data-act="delete-files" data-ref="${esc(b.packageRef)}"`
    + `${d.allowed ? '' : ' disabled'} title="${esc(dfTitle)}">Delete files</button>`;
  const dhBtn = batchAllTerminal(b)
    ? `<button class="ghost" data-act="delete-history" data-ref="${esc(b.packageRef)}" title="Remove this batch from the transfer history">Delete history</button>`
    : '';
  const files = `${b.fileCount} file${b.fileCount === 1 ? '' : 's'}`;
  return `<div class="trow${open ? ' open' : ''}" data-batch-toggle data-ref="${esc(b.packageRef)}">
    <div class="trow-main">
      <span class="trow-name">${esc(transferName(b))}</span>
      ${markers.join(' ')}
      <span class="spacer"></span>
      <span class="trow-actions">${dfBtn}${dhBtn}</span>
    </div>
    <div class="trow-sub">
      <span class="muted mono">${esc(b.createdAt)}</span>
      <span class="muted">· ${files} · ${esc(fmtSize(b.totalBytes))}</span>
      <span class="trow-targets">${targetChips}</span>
    </div>
  </div>`;
}

function renderTransferListBody() {
  const body = $('transferListBody');
  if (!transfersData.length) { body.innerHTML = '<div class="empty">No transfers yet</div>'; return; }
  const visible = transfersData.filter((b) => batchInFilter(b, transferFilter));
  if (!visible.length) { body.innerHTML = '<div class="empty">No transfers match this filter</div>'; return; }
  body.innerHTML = visible.map(transferRowHtml).join('');
}

// ── render: detail pane (Files / Targets / Log) ──
function transferFilesHtml(b) {
  const targets = b.targets || [];
  const files = b.files || [];
  if (!files.length) return '<div class="empty">no file manifest recorded for this batch</div>';
  const rows = files.map((f) => {
    const cells = new Map((f.targets || []).map((c) => [c.peerHex, c]));
    // Compact "N/N confirmed" when every target's per-file cell reached `done`
    // (the per-file OutboundFileState — NOT the target-level `confirmed`
    // OutboundState, which per-file cells never carry). A dedup delivery also
    // lands at `done`, so it counts as uniform here; the "(dedup)" label only
    // shows in the breakdown branch below, for a mixed-state file.
    let uniform = targets.length > 0;
    for (const t of targets) {
      const c = cells.get(t.peerHex);
      if (!c || c.state !== 'done') { uniform = false; break; }
    }
    let delivery;
    if (!targets.length) {
      delivery = '<span class="muted">—</span>';
    } else if (uniform) {
      delivery = `<span class="chip confirmed">${targets.length}/${targets.length} confirmed</span>`;
    } else {
      delivery = targets.map((t) => {
        const c = cells.get(t.peerHex);
        if (!c) return `<span class="chip cancelled">missing on ${esc(t.name)}</span>`;
        if (c.outcome === 'duplicate') return `<span class="chip confirmed">${esc(t.name)}: confirmed (dedup)</span>`;
        return `<span class="chip ${esc(c.state)}">${esc(t.name)}: ${esc(c.state)}</span>`;
      }).join(' ');
    }
    return `<tr><td class="mono">${esc(f.relPath)}</td><td>${esc(fmtSize(f.byteSize))}</td><td class="tdelivery">${delivery}</td></tr>`;
  }).join('');
  return `<div class="tablewrap"><table>
    <thead><tr><th>file</th><th>size</th><th>delivery</th></tr></thead>
    <tbody>${rows}</tbody></table></div>`;
}

function transferTargetsHtml(b) {
  const targets = b.targets || [];
  if (!targets.length) return '<div class="empty">no targets — this batch has no outbound rows in the current window</div>';
  return targets.map((t) => {
    const declined = isDeclinedTarget(t);
    const nonTerminal = !TERMINAL_STATES.has(t.state);
    const waiting = nonTerminal && t.nextRetryAt;
    const cls = declined ? 'declined' : (waiting ? 'waiting' : t.state);
    const label = declined ? 'declined' : t.state;
    const pct = t.byteSize > 0 ? Math.floor(100 * t.bytesDone / t.byteSize) : 0;
    const waitNote = waiting
      ? ` · retry in <span class="cd" data-countdown="${esc(t.nextRetryAt)}">${esc(fmtCountdown(t.nextRetryAt) || '…')}</span>`
      : '';
    const err = t.lastError ? `<div class="terr${t.state === 'failed' ? ' err' : ''}">${esc(t.lastError)}</div>` : '';
    const acts = [
      `<button class="ghost" data-act="kick" data-id="${t.rowId}"${nonTerminal ? '' : ' disabled'} title="Announce / serve this transfer now">Send now</button>`,
      `<button class="ghost" data-act="cancel" data-id="${t.rowId}"${nonTerminal ? '' : ' disabled'} title="Cancel this transfer">Cancel</button>`,
      `<button class="ghost" data-act="retry" data-id="${t.rowId}"${(!nonTerminal && !declined) ? '' : ' disabled'} title="Resend this package in place">Retry</button>`,
    ];
    if (declined) acts.push(`<button data-act="resend-as-new" data-id="${t.rowId}" title="Divert into a brand-new transfer">Resend as new</button>`);
    return `<div class="ttrow">
      <div class="ttrow-head">
        <span class="chip ${esc(cls)}">${esc(t.name)}: ${esc(label)}</span>
        ${t.generation > 1 ? `<span class="tmarker">attempt ${t.generation}</span>` : ''}
        <span class="spacer"></span>
        <span class="ttrow-acts">${acts.join('')}</span>
      </div>
      <div class="ttrow-prog">
        <div class="tbar"><div class="tbar-fill" style="width:${pct}%"></div></div>
        <span class="muted">${esc(fmtSize(t.bytesDone))} / ${esc(fmtSize(t.byteSize))} (${pct}%)${waitNote}</span>
      </div>
      ${err}
    </div>`;
  }).join('');
}

function transferLogHtml(b) {
  const evs = transferEvents[b.packageRef];
  if (evs === undefined) return '<div class="empty">loading events…</div>';
  if (!evs.length) return '<div class="empty">no events recorded for this batch</div>';
  return '<ul class="tlog">' + evs.map((e) => {
    const detail = e.detail ? ` — ${esc(e.detail)}` : '';
    return `<li><span class="mono muted">${esc(e.ts)}</span> <span class="chip">${esc(e.target)}</span> <b>${esc(e.kind)}</b>${detail}</li>`;
  }).join('') + '</ul>';
}

function renderTransferDetail() {
  const pane = $('transferDetail');
  const b = transfersData.find((x) => x.packageRef === openTransferRef);
  if (!b) { pane.hidden = true; pane.innerHTML = ''; return; }
  pane.hidden = false;
  const tab = openTransferSubTab;
  const subBtn = (key, label) => `<button class="tsub${key === tab ? ' active' : ''}" data-subtab="${key}">${label}</button>`;
  const bodyHtml = tab === 'targets' ? transferTargetsHtml(b)
    : tab === 'log' ? transferLogHtml(b)
      : transferFilesHtml(b);
  pane.innerHTML = `
    <div class="tdetail-head">
      <div><b>${esc(transferName(b))}</b> <span class="muted mono">${esc(b.batchUuid)}</span></div>
      <div class="tdetail-tabs">${subBtn('files', 'Files')}${subBtn('targets', 'Targets')}${subBtn('log', 'Log')}</div>
      <button class="ghost tdetail-close" data-detail-close title="Close">&times;</button>
    </div>
    <div class="tdetail-body">${bodyHtml}</div>`;
}

// A JSON fingerprint of everything the render depends on; identical fingerprint →
// skip the re-render so a 2 s poll of unchanged data never reflows the list (nor
// resets the log scroll). Excludes the live countdown text, which is patched in
// place by updateTransferCountdowns() so waiting deadlines stay honest.
function transferSignature() {
  return JSON.stringify({
    d: transfersData,
    f: transferFilter,
    o: openTransferRef,
    s: openTransferSubTab,
    e: (openTransferSubTab === 'log' && openTransferRef) ? (transferEvents[openTransferRef] || null) : 0,
  });
}

function renderTransfers() {
  if (transferDialogOpen) return; // never reshuffle the list under an open confirm
  const sig = transferSignature();
  if (sig === lastTransferSig) return;
  lastTransferSig = sig;
  renderTransferChips();
  renderTransferListBody();
  renderTransferDetail();
}

// Patch every armed-retry countdown in place each tick — cheap, and it keeps the
// deadlines live without the full-list reflow renderTransfers() guards against.
function updateTransferCountdowns() {
  document.querySelectorAll('#transfers [data-countdown]').forEach((el) => {
    el.textContent = fmtCountdown(el.dataset.countdown) || '…';
  });
}

// ── detail pane open/close + sub-tabs ──
function toggleTransferDetail(ref) {
  if (openTransferRef === ref) { closeTransferDetail(); return; }
  // Switching directly row A -> row B (no close in between): evict A's cached
  // journal too, not only on an explicit close, so a later reopen of A re-fetches
  // rather than showing a stale Log tab.
  if (openTransferRef) delete transferEvents[openTransferRef];
  openTransferRef = ref;
  openTransferSubTab = 'files';
  renderTransfers();
}

function closeTransferDetail() {
  if (openTransferRef) delete transferEvents[openTransferRef]; // re-fetch on next open
  openTransferRef = null;
  renderTransfers();
}

function setTransferSubTab(tab) {
  openTransferSubTab = tab;
  // Events are read ONLY when the Log tab is opened, and cached until the pane
  // closes (one fetch per open, not per poll).
  if (tab === 'log' && openTransferRef && transferEvents[openTransferRef] === undefined) {
    loadTransferEvents(openTransferRef);
  }
  renderTransfers();
}

async function loadTransferEvents(ref) {
  try {
    const evs = await getJson('/api/transfers/events?ref=' + encodeURIComponent(ref));
    transferEvents[ref] = Array.isArray(evs) ? evs : [];
  } catch (e) {
    transferEvents[ref] = [];
    transferFlash('could not load events: ' + e.message, false);
  }
  if (openTransferRef === ref && openTransferSubTab === 'log') renderTransfers();
}

// ── per-target actions (kick / cancel / retry / resend-as-new) ──
async function onTransferAction(btn) {
  const act = btn.dataset.act;
  if (act === 'delete-files') { confirmDeleteFiles(btn.dataset.ref); return; }
  if (act === 'delete-history') { confirmDeleteHistory(btn.dataset.ref); return; }
  const id = Number(btn.dataset.id);
  const dispatch = {
    kick: ['/api/kick', { ids: [id] }, 'send requested'],
    cancel: ['/api/cancel', { ids: [id] }, 'cancel requested'],
    retry: ['/api/retry', { ids: [id] }, 'resent'],
    'resend-as-new': ['/api/resend-as-new', { id }, 'diverted to a new transfer'],
  }[act];
  if (!dispatch) return;
  const [path, body, verb] = dispatch;
  btn.disabled = true;
  try {
    const r = await api(path, { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });
    if (!r.ok) throw new Error(await r.text());
    const rep = await r.json();
    // retry/kick/cancel report per-id rejections; resend-as-new 400s on failure
    // (caught above), so a 200 there is always success.
    const rej = (rep.rejected || [])[0];
    if (rej) transferFlash(`${act}: ${rej.reason}`, false);
    else transferFlash(verb, true);
  } catch (e) {
    transferFlash(`${act} failed: ${e.message}`, false);
  }
  refreshTransfers();
}

// ── in-page confirm dialog (no window.confirm / alert) ──
// The OK handler closes the dialog BEFORE running onConfirm so the action's
// follow-up refreshTransfers()/renderTransfers() are not suppressed by the
// dialog-open guard.
function openConfirm({ title, bodyHtml, confirmLabel, danger, onConfirm }) {
  transferDialogOpen = true;
  const overlay = document.createElement('div');
  overlay.className = 'tconfirm-overlay';
  overlay.innerHTML = `
    <div class="tconfirm" role="dialog" aria-modal="true" aria-label="${esc(title)}">
      <h3>${esc(title)}</h3>
      <div class="tconfirm-body">${bodyHtml}</div>
      <div class="tconfirm-actions">
        <button class="ghost" data-cd-cancel>Cancel</button>
        <button class="${danger ? 'danger' : ''}" data-cd-ok>${esc(confirmLabel)}</button>
      </div>
    </div>`;
  const close = () => { transferDialogOpen = false; overlay.remove(); };
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay || e.target.closest('[data-cd-cancel]')) { close(); return; }
    if (e.target.closest('[data-cd-ok]')) { close(); onConfirm(); }
  });
  document.body.appendChild(overlay);
  overlay.querySelector('[data-cd-ok]').focus();
}

// ── delete source files (obligation-gated) ──
function confirmDeleteFiles(ref) {
  const b = transfersData.find((x) => x.packageRef === ref);
  if (!b) return;
  const d = b.deletable || {};
  const delivered = d.deliveredTargets || 0;
  const closed = d.closed || [];
  let body = `<p>Delete <b>${b.fileCount}</b> source capture file${b.fileCount === 1 ? '' : 's'} for this batch from disk?</p>`;
  body += `<p class="muted">Delivered to ${delivered} target${delivered === 1 ? '' : 's'}.</p>`;
  if (closed.length) body += `<p class="muted">Closed by receiver: ${closed.map(esc).join(', ')}</p>`;
  if (delivered === 0) body += `<p class="tconfirm-warn">No target confirmed this batch — these files exist nowhere else.</p>`;
  openConfirm({ title: 'Delete source files', bodyHtml: body, confirmLabel: 'Delete files', danger: true, onConfirm: () => doDeleteFiles(ref) });
}

async function doDeleteFiles(ref) {
  try {
    const r = await api('/api/delete-files', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ packageRef: ref }) });
    if (r.status === 409) {
      const j = await r.json();
      transferFlash('delete refused: ' + (j.blockers || []).join('; '), false);
    } else if (r.status === 404) {
      transferFlash('delete failed: unknown batch', false);
    } else if (!r.ok) {
      throw new Error(await r.text());
    } else {
      const rep = await r.json();
      const parts = [`removed ${rep.removed.length}`];
      if (rep.skipped.length) parts.push(`skipped ${rep.skipped.length}`);
      if (rep.failed.length) parts.push(`failed ${rep.failed.length}`);
      transferFlash(parts.join(', '), rep.failed.length === 0);
    }
  } catch (e) {
    transferFlash('delete failed: ' + e.message, false);
  }
  refreshTransfers();
}

// ── delete history (whole batch group; kept only for all-terminal batches) ──
function confirmDeleteHistory(ref) {
  const b = transfersData.find((x) => x.packageRef === ref);
  if (!b) return;
  openConfirm({
    title: 'Delete from history',
    bodyHtml: `<p>Remove <b>${esc(transferName(b))}</b> from the transfer history?</p>`
      + `<p class="muted">This drops the sender bookkeeping only — the source files on disk are not touched.</p>`,
    confirmLabel: 'Delete history',
    danger: true,
    onConfirm: () => doDeleteHistory(ref),
  });
}

async function doDeleteHistory(ref) {
  try {
    const r = await api('/api/delete', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ packageRefs: [ref] }) });
    if (!r.ok) throw new Error(await r.text());
    const rep = await r.json();
    const rej = (rep.rejected || []).find((x) => x.ref === ref);
    if (rej) {
      transferFlash('not deleted: ' + rej.reason, false);
    } else {
      // Remove the row locally for instant feedback; the refetch confirms it.
      transfersData = transfersData.filter((b) => b.packageRef !== ref);
      if (openTransferRef === ref) openTransferRef = null;
      transferFlash('removed from history', true);
      renderTransfers();
    }
  } catch (e) {
    transferFlash('history delete failed: ' + e.message, false);
  }
  refreshTransfers();
}

// ── delegated click handler + poll ──
async function onTransfersClick(e) {
  const filterBtn = e.target.closest('[data-filter]');
  if (filterBtn) { transferFilter = filterBtn.dataset.filter; renderTransfers(); return; }
  const subBtn = e.target.closest('[data-subtab]');
  if (subBtn) { setTransferSubTab(subBtn.dataset.subtab); return; }
  if (e.target.closest('[data-detail-close]')) { closeTransferDetail(); return; }
  // Action buttons before the row toggle so a click on them never opens the pane.
  const actBtn = e.target.closest('[data-act]');
  if (actBtn) { e.stopPropagation(); onTransferAction(actBtn); return; }
  const row = e.target.closest('[data-batch-toggle]');
  if (row) toggleTransferDetail(row.dataset.ref);
}

function wireTransfers() {
  $('transfers').addEventListener('click', onTransfersClick);
}

async function refreshTransfers() {
  // The list poll is gated to its own tab — Settings never fetches transfers.
  if (activeTab !== 'transfers') return;
  try {
    const data = await getJson('/api/transfers');
    transfersData = Array.isArray(data) ? data : [];
    // Drop the detail pane if its batch vanished (e.g. history-deleted elsewhere).
    if (openTransferRef && !transfersData.some((b) => b.packageRef === openTransferRef)) {
      openTransferRef = null;
    }
  } catch (e) {
    // Offline is surfaced by the connection dot (refreshStatus); keep the last
    // render rather than blanking the list on a transient poll hiccup.
    return;
  }
  renderTransfers();
  updateTransferCountdowns();
}

// ── boot ────────────────────────────────────────────────────────────────────
// The shared helpers + the live refreshTransfers renderer are exposed on
// PerseusApp so the poll tick and the To-Sync manual flush can drive the list.
window.PerseusApp = {
  api, getJson, $, esc, shortHex, fmtSize, fmtDur, fmtCountdown,
  setTab,
  refreshTransfers,
};

function tick() {
  refreshStatus();
  refreshLog();
  refreshAccount();
  loadDeviceName();
  refreshPending();
  window.PerseusApp.refreshTransfers();
}

function boot() {
  renderTransfersTab();
  renderSettingsTab();
  mountBanner();
  wireTabs();
  wireTosync();
  wireTransfers();
  wireCaptureDirs();
  wireTargets();
  wireDeviceName();
  wireUploadLimit();
  wireAccount();
  wireRetention();
  // Initial load; the 2 s tick then polls status + retention log + account +
  // device name + pending. The policy form, targets list, device-name field and
  // upload cap are on demand / load-once (not clobbered while editing).
  loadPolicy(); loadCaptureDirs(); loadTargets(); loadDeviceName(); loadUploadLimit(); refreshAccount();
  tick(); setInterval(tick, 2000);
}

boot();
