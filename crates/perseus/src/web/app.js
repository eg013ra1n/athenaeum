// Perseus web UI v2 — client script.
//
// A framework-free, single-file vanilla app. The page shell (index.html) is a
// three-tab skeleton: this script renders the Transfers + Library + Settings
// sections into the empty <main> panels, wires their controls, and drives the
// 2 s poll (the Library listing is on-demand only — see its section). Every
// Settings section (account/OTP, device name, capture dirs, send targets,
// retention) is ported behaviour-for-behaviour from the v1 page; only the DOM
// structure + class names changed. The upload-speed cap is the one section with
// no v1 ancestor (W1 T1.6). The Transfers tab ships its "To Sync" strip
// plus the unified one-row-per-batch transfer list (filter chips, the shared
// bottom detail pane with Files / Targets / Log sub-tabs, the two delete
// actions, and — on a settled batch — "Send to device…", the §6 two-step
// divert), rendered by `refreshTransfers` off GET /api/transfers.

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
// Epoch milliseconds → `YYYY-MM-DD HH:MM` in the operator's local zone (the
// project-wide timestamp spelling). `0` is what the listing reports for a
// filesystem that gave no mtime, so it reads as unknown rather than 1970.
function fmtMtime(ms) {
  const n = Number(ms);
  if (!n) return '–';
  const d = new Date(n);
  if (isNaN(d.getTime())) return '–';
  const p = (x) => String(x).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
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
// Three tabs, active one remembered in localStorage. Switching toggles `hidden`
// on the panels + an `active` class on the buttons — no router, no history. An
// unknown / never-stored name falls back to Transfers.
const TAB_NAMES = ['transfers', 'library', 'settings'];
const tabBtnId = (name) => 'tabBtn' + name[0].toUpperCase() + name.slice(1);

function setTab(name) {
  const active = TAB_NAMES.includes(name) ? name : 'transfers';
  activeTab = active;
  TAB_NAMES.forEach((t) => {
    $('tab-' + t).hidden = t !== active;
    $(tabBtnId(t)).classList.toggle('active', t === active);
  });
  try { localStorage.setItem('perseus.tab', active); } catch (e) { /* private mode */ }
  // Entering a tab: refresh it at once. Both list views are gated to their own
  // tab — /api/transfers is polled only while Transfers shows, and the library
  // listing is fetched on demand only (never polled; see the library section).
  if (active === 'transfers' && window.PerseusApp) window.PerseusApp.refreshTransfers();
  if (active === 'library') libRefresh();
  // The preview overlay and the action dialogs belong to the Library tab.
  // Leaving the tab closes both — an overlay left "open" while its tab is hidden
  // would keep the document-level arrow/Escape listener, stealing those keys
  // from the tab now on screen (and a dialog would still be holding a selection
  // snapshot nobody can see).
  else { libPvClose(); libDlgClose(); }
  // Same rule for the Transfers tab's own dialog: it holds an Escape listener
  // and, left open, its guard would freeze the list it is no longer over.
  if (active !== 'transfers') sendToClose();
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
  // by default, expanded by clicking the counter), the live three-way send-mode
  // control (0.5.1 T14: Immediately / On schedule / Manually), the auto
  // quiet-window input, the schedule editor, and the "Send N pending now"
  // button. Polled on the 2 s tick; the controls are not clobbered while the
  // operator is inside the card or while a save is in flight.
  // Below it, the `#transfers` section holds the unified one-row-per-batch list:
  // filter chips (with live counts), the list body, and the shared bottom detail
  // pane. Its body/chips/pane are re-rendered by refreshTransfers().
  $('tab-transfers').innerHTML = `
    <section id="tosync">
      <h2>To Sync</h2>
      <div class="row" style="margin-bottom:0.5rem;">
        <div class="seg" role="radiogroup" aria-label="Send mode">
          <label><input type="radio" name="sendMode" id="modeAuto" value="auto" /> Immediately (quiet window)</label>
          <label><input type="radio" name="sendMode" id="modeScheduled" value="scheduled" /> On schedule</label>
          <label><input type="radio" name="sendMode" id="modeManual" value="manual" /> Manually</label>
        </div>
        <label id="quietWrap" class="inline-label">
          quiet window (s)
          <input id="quietSecs" type="number" min="1" class="qty" aria-label="Auto quiet window in seconds" />
        </label>
        <button class="counter" id="pendingToggle" aria-expanded="false" aria-controls="pendingTree">
          <span id="pendingCaret">&#9656;</span> <span id="pendingCount">0</span> pending
        </button>
        <span class="spacer"></span>
        <button id="sendNow" disabled title="Send everything pending right now. Works in every mode.">Send 0 pending now</button>
      </div>
      <!-- Scheduled mode only: the local wall-clock send times + the catch-up
           opt-in. Revealed by updateModeVisibility(); every mutation saves. -->
      <div id="schedWrap" class="sched-wrap" style="display:none;">
        <div class="muted">Sends everything pending at these times, on this device's local clock.</div>
        <div id="schedList" class="sched-list"></div>
        <div class="row">
          <label class="inline-label">add a time
            <input id="schedAdd" type="time" class="qty" aria-label="New scheduled send time" />
          </label>
          <button id="schedAddBtn" class="ghost">Add</button>
          <span class="spacer"></span>
          <label class="inline-label">
            <input id="schedCatchup" type="checkbox" /> Catch up a missed send at startup
          </label>
        </div>
        <!-- The armed-but-unsaved marker (see schedArming). On the card itself,
             not only in the shared flash line, which the next message overwrites. -->
        <div id="schedUnsaved" class="sched-unsaved" hidden>On schedule is not saved yet — add a send time to apply it.</div>
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
      <!-- Spec §7's network-share hint. It belongs beside the poll interval, but
           that knob has no editor on this page (it is file-only), so it is stated
           here — next to the field that decides WHAT gets polled — naming the key
           rather than pointing at a control that does not exist. -->
      <div class="muted target-intro">Watching a network share (SMB/NFS)? Raise <code>poll_interval_secs</code> in <code>perseus.toml</code> to reduce NAS load — the folder is re-listed on every poll.</div>
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
      <!-- Spec §8: what this node will do to capture files, in the operator's
           words, generated from the EFFECTIVE config below. The mode banner and
           the policy sentence are the two things an operator must be able to
           read without opening perseus.toml. -->
      <div class="banner" id="retMode" style="display:none;"></div>
      <div class="ret-card">
        <div id="retPolicy">reading the retention policy…</div>
        <div class="muted ret-note" id="retCadence"></div>
        <!-- A recorded fact from the pass log, never a predicted next tick. -->
        <div class="muted ret-note" id="retLastPass"></div>
        <div class="muted ret-note">Deleting files yourself is a separate path and always available: Library &rarr; <b>Delete</b>, and a batch's <b>Delete source files</b>. Those are recorded as <code>manual-web</code>; retention's own deletions are recorded as <code>retention_deleted</code>, so the audit trail never confuses the two.</div>
        <div class="ret-note"><a href="#retlogHead">Recent retention passes &darr;</a></div>
      </div>
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
      <h2 class="section-sub" id="retlogHead">Recent retention passes</h2>
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

// Free space per unique volume behind the capture dirs + data dir. A volume the
// agent could not probe (offline share) is simply absent from `volumes` — the
// header then shows one chip fewer rather than a stale or invented number.
// Below FREE_LOW_BYTES the chip turns red: a fixed floor, not a percentage, so
// it means the same thing on a 2 TB capture disk and a 256 GB SSD.
const FREE_LOW_BYTES = 10 * 1024 * 1024 * 1024;

function renderVolumes(vols) {
  const el = $('volChips');
  if (!el) return;   // never let a missing chip slot fake an offline poll
  const list = Array.isArray(vols) ? vols : [];
  el.innerHTML = list.map((v) => {
    const low = Number(v.freeBytes) < FREE_LOW_BYTES;
    const title = `${v.root} — ${fmtSize(v.freeBytes)} free of ${fmtSize(v.totalBytes)}`;
    return `<span class="chip vol${low ? ' chip-danger' : ''}" title="${esc(title)}">Free: ${esc(fmtSize(v.freeBytes))}</span>`;
  }).join('');
}

// The armed scheduled send, straight off `/api/status`. The server sends a stamp
// only in scheduled mode with a live deadline, so a missing/unparseable value
// hides the line rather than guessing — the same rule as the volume chips: no
// reading is better than one nobody can vouch for.
function renderNextSend(iso) {
  const el = $('nextSend');
  if (!el) return;
  const t = iso ? Date.parse(iso) : NaN;
  if (isNaN(t)) { el.textContent = ''; el.hidden = true; return; }
  // The server sends a local-offset stamp; fmtMtime prints it in the operator's
  // own zone, so a 06:00 schedule reads back as 06:00.
  el.textContent = 'Next scheduled send: ' + fmtMtime(t);
  el.hidden = false;
}

async function refreshStatus() {
  try {
    const s = await getJson('/api/status');
    $('connDot').className = 'conn-dot ok';
    $('conn').textContent = 'connected';
    renderAgentBanner(s);
    renderVolumes(s.volumes);
    renderNextSend(s.nextScheduledSend);
    // The Library tab's roots view rides this same poll (root list + the
    // per-root free-space chip); it never fetches a listing on the tick.
    libOnStatus(s);
  } catch (e) {
    $('connDot').className = 'conn-dot err';
    $('conn').textContent = 'offline: ' + e.message;
    // Drop the chips rather than leave a reading nobody can vouch for.
    renderVolumes([]);
    renderNextSend(null);
    libOnStatus(null);
  }
}

// ── retention (transparency card + policy editor + recent-pass log) ─────────

// The policy sentence (spec §8), generated from the EFFECTIVE config so it can
// never describe a policy the node is not running.
//
// The `keep_days` wording deliberately does NOT say "after every target
// confirmed" (the spec's draft phrasing). The evaluator
// (`athenaeum_core::sync::retention`) draws candidates from `store.confirmed()`
// — EVERY confirmed row, tested for age individually — so a fan-out package is
// a candidate as soon as its EARLIEST confirmation ages out, without waiting for
// the slowest target. Saying "every target" would promise a delay the code does
// not honour, which is the one thing this card exists to prevent.
//
// Returns plain text: the caller assigns it with `textContent`, so a
// server-supplied policy name in the fallback arm can never inject markup.
function retentionPolicyText(p) {
  const days = Number(p.keepDays);
  const pct = Number(p.diskMaxPct);
  switch (p.policy) {
    case 'keep_everything':
      return 'Perseus never deletes capture files. Manual deletion in Library, and a batch’s "Delete source files", are the only paths.';
    case 'on_confirm':
      return 'A file is deleted on the first pass after a target confirms receiving it — there is no grace period.';
    case 'keep_days':
      return `A file becomes deletable ${days} day${days === 1 ? '' : 's'} after it was confirmed received — the clock starts at the confirmation, not at capture. When a batch went to several devices, the earliest confirmation starts it.`;
    case 'disk_pct':
      return `While a capture volume sits at or over ${pct} %, confirmed files are deleted oldest-confirmed-first until usage drops back under the cap. Below the cap nothing is deleted.`;
    default:
      return `Unrecognised policy "${p.policy}" — this page cannot say what it will do.`;
  }
}

// "Checked every hour", from `interval_secs`. No next-pass countdown: the
// retention loop is a plain `sleep(interval)` select with no exposed deadline,
// and inventing a timer here would produce a number the agent never promised.
// The card states the cadence (config) and the LAST pass (a recorded fact, see
// `refreshLog`) instead.
function retentionCadenceText(p) {
  // Under keep_everything the loop still ticks, but no pass can ever have a
  // consequence — describing a cadence would dress up a no-op as vigilance.
  if (p.policy === 'keep_everything') return '';
  const secs = Number(p.intervalSecs);
  if (!Number.isFinite(secs) || secs <= 0) return 'Pass cadence unknown.';
  let every;
  if (secs % 3600 === 0) { const h = secs / 3600; every = h === 1 ? 'every hour' : `every ${h} hours`; }
  else if (secs % 60 === 0) { const m = secs / 60; every = m === 1 ? 'every minute' : `every ${m} minutes`; }
  else every = `every ${secs} seconds`;
  return `Checked ${every}. Every candidate is verified to still match what was transferred before it is touched.`;
}

// Yellow dry-run / red live-armed, with the two-key opt-in named in full.
// `keep_everything` gets neither: nothing is ever a candidate, so a red "armed"
// banner would overstate and a yellow "nothing is deleted" would just repeat the
// policy sentence. The one case worth saying out loud is armed-but-inert, so the
// operator knows what flipping the dropdown below means.
function renderRetentionMode(p) {
  const b = $('retMode');
  if (!b) return;
  b.style.display = '';
  if (p.policy === 'keep_everything') {
    if (p.liveDeletionPossible) {
      b.className = 'banner warn';
      b.textContent = 'Live deletion is armed in perseus.toml, but keep_everything means nothing is ever a candidate. Changing the policy below starts deleting confirmed files for real.';
      return;
    }
    b.style.display = 'none';
    b.textContent = '';
    return;
  }
  if (p.liveDeletionPossible) {
    b.className = 'banner err';
    b.textContent = 'LIVE DELETION ARMED — confirmed capture files are really removed. Both keys are turned in perseus.toml: dry_run = false and i_have_verified_the_soak = true.';
    return;
  }
  b.className = 'banner warn';
  // dry_run = false alone never validates, so a node can reach here with the
  // flag off and still delete nothing. Say which key is missing rather than
  // letting the editor below read as if it were armed.
  b.textContent = p.dryRun
    ? 'DRY-RUN — nothing is deleted. Candidates are only logged, every pass.'
    : 'DRY-RUN — nothing is deleted. dry_run is off, but i_have_verified_the_soak is not set in perseus.toml, and live deletion needs both keys.';
}

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
    // textContent, not innerHTML: the sentence embeds server-supplied numbers
    // and, in the fallback arm, a server-supplied policy name.
    $('retPolicy').textContent = retentionPolicyText(p);
    $('retCadence').textContent = retentionCadenceText(p);
    renderRetentionMode(p);
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

// The card's "last pass" line rides the same fetch as the log below it: the ring
// buffer's newest `at` IS the last pass, so it costs no extra request and states
// a fact the agent recorded rather than one this page derived.
function renderRetentionLastPass(rows) {
  const el = $('retLastPass');
  if (!el) return;
  const newest = rows && rows.length ? rows[0] : null;
  if (!newest) {
    el.textContent = 'No pass recorded since this agent started.';
    return;
  }
  const when = fmtMtime(Date.parse(newest.at));
  const acted = (newest.deleted || []).length;
  const eligible = (newest.wouldDelete || []).length;
  el.textContent = newest.dryRun
    ? `Last pass ${when} — ${eligible} candidate${eligible === 1 ? '' : 's'} logged, nothing deleted.`
    : `Last pass ${when} — ${acted} file${acted === 1 ? '' : 's'} deleted of ${eligible} eligible.`;
}

async function refreshLog() {
  try {
    const rows = await getJson('/api/retention/log');
    renderRetentionLastPass(rows);
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

// ── to sync: pending tree + send-mode control + schedule editor + manual send ─
// `pendingCount` is the last-fetched total; the Send button label + enabled state
// derive from it. `lastPendingJson` guards the tree render so an unchanged pending
// set doesn't collapse the operator's expanded <details> every 2 s tick. The tree
// itself is collapsed by default and toggled by clicking the pending counter.
//
// `scheduleTimes` mirrors the server's normalised `HH:MM` list and is the payload
// every save PUTs. It is only ever REPLACED by a server response (the poll, or a
// PUT echo) or by an explicit add/remove — so the page can never invent a schedule.
// `sendCfgSeen` is what makes the other half of that claim — that the page can
// never PUT a send config it has not READ — actually hold. The boot value of
// `scheduleTimes` is `[]`, and an empty list sent EXPLICITLY is a clear the server
// honours; so if the first `/api/pending` never landed (agent restarting, page
// opened offline) a plain save would erase the on-disk schedule with a 200. The
// flag is set only where a server response paints the card (`applyModeControls`),
// and until it is true `saveSendMode` writes NOTHING — the mode and the quiet
// window are no more the node's than the times are (see the gate there).
// `sendModeSaving` counts PUTs in flight: while one is, the poll must not repaint
// the card, or a tick landing between the request and its response would visibly
// revert the operator's edit (and, worse, leave the mutation staged against a
// stale list).
// `schedArming` covers the one state that is deliberately NOT saved: the operator
// has picked "On schedule" on a node with no times yet. That PUT would be a 422
// (the config refuses `scheduled` with an empty schedule) and the next poll would
// snap the radio back to Immediately — collapsing the very editor they opened to
// type a time into. While armed, the page holds the un-saved selection, the poll
// leaves the card alone, `#schedUnsaved` says so ON THE CARD (the shared flash is
// overwritten by the next message), and the first added time carries the mode
// switch with it. It ends when a save carries the intent, or when the operator
// picks another mode.
let pendingCount = 0;
let lastPendingJson = null;
let scheduleTimes = [];
let sendModeSaving = 0;
let schedArming = false;
let sendCfgSeen = false;

// Said by every path that refuses to act on an un-read card, so the operator sees
// one explanation whichever control they reached for.
const READING_HINT = 'still reading the schedule on this node — try again in a moment';

// The single writer of `schedArming`, so the un-saved marker on the card can
// never disagree with the flag the poll guard reads.
function setSchedArming(on) {
  schedArming = on;
  const el = $('schedUnsaved');
  if (el) el.hidden = !on;
}

// Which radio is selected. Read from the inputs (not from a remembered variable)
// so it can never disagree with what the operator sees — and, since T14, it knows
// all THREE modes: before, `scheduled` read back as `auto`, so simply opening the
// page on a scheduled node and saving anything silently rewrote the mode.
const currentMode = () =>
  ($('modeScheduled').checked ? 'scheduled' : $('modeManual').checked ? 'manual' : 'auto');

// `HH:MM`, tolerating a single-digit hour and a browser that appends seconds
// (`<input type="time">` yields `HH:MM:SS` when a step is in play). Returns the
// canonical zero-padded form, or null if it is not a time at all — the same shape
// the Rust side accepts, so the page never sends what the validator would refuse.
const HHMM_RE = /^(\d{1,2}):(\d{2})(?::\d{2})?$/;
function normTime(s) {
  const m = HHMM_RE.exec(String(s ?? '').trim());
  if (!m) return null;
  const h = Number(m[1]), mi = Number(m[2]);
  if (!(h >= 0 && h <= 23 && mi >= 0 && mi <= 59)) return null;
  return String(h).padStart(2, '0') + ':' + String(mi).padStart(2, '0');
}
// Canonical list: padded, deduped, sorted. Sorting zero-padded `HH:MM` strings
// lexically IS chronological order, so no date arithmetic is needed. Anything
// unparseable is dropped rather than carried — the server normalises identically.
function normalizeTimes(list) {
  const out = [];
  (Array.isArray(list) ? list : []).forEach((t) => {
    const n = normTime(t);
    if (n && out.indexOf(n) < 0) out.push(n);
  });
  out.sort();
  return out;
}

function updateModeVisibility() {
  const mode = currentMode();
  // The quiet window belongs to Immediately; the schedule editor to On schedule.
  $('quietWrap').style.display = mode === 'auto' ? '' : 'none';
  $('schedWrap').style.display = mode === 'scheduled' ? '' : 'none';
}

function updateSendButton() {
  const btn = $('sendNow');
  btn.textContent = `Send ${pendingCount} pending now`;
  // Visible in EVERY mode: `POST /api/send-now` flushes the accumulator whatever
  // the mode is, so hiding it outside Manual (as the v2 page did) misrepresented
  // the backend — an operator in Immediately or On schedule can always push what
  // is waiting without changing their mode.
  btn.style.display = '';
  btn.disabled = pendingCount === 0;
}

// Paint the schedule points as removable chips. `data-sched-del` carries the
// canonical time; the delegated handler matches it against `scheduleTimes`, so a
// value that is not in the list removes nothing.
function renderScheduleTimes() {
  const list = $('schedList');
  if (!list) return;
  if (!scheduleTimes.length) {
    list.innerHTML = '<div class="empty">no send times yet — add one below</div>';
    return;
  }
  list.innerHTML = scheduleTimes
    .map((t) => `<span class="chip sched-time">${esc(t)}<button class="chipx" data-sched-del="${esc(t)}" title="Remove ${esc(t)}" aria-label="Remove ${esc(t)}">&times;</button></span>`)
    .join('');
}

function applyModeControls(mode, quietSecs, times, catchup) {
  $('modeScheduled').checked = mode === 'scheduled';
  $('modeManual').checked = mode === 'manual';
  $('modeAuto').checked = mode !== 'scheduled' && mode !== 'manual';
  // Never clobber an input while the operator is typing/toggling in it.
  if (document.activeElement !== $('quietSecs')) $('quietSecs').value = quietSecs;
  if (document.activeElement !== $('schedCatchup')) $('schedCatchup').checked = catchup !== false;
  scheduleTimes = normalizeTimes(times);
  // This is the ONLY caller-agnostic proof the page has read the node's send
  // config: every path into here (the poll, a PUT echo, the post-rejection
  // re-sync) carries server values. From here on a save may speak about the
  // schedule; before it, it must stay silent about it.
  sendCfgSeen = true;
  updateModeVisibility();
  renderScheduleTimes();
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
    // Don't clobber the controls while the operator is interacting with the card,
    // nor while a save is in flight (a tick landing mid-PUT carries the OLD state
    // and would visibly undo the edit); the tree + button label always reflect the
    // live count.
    if (!sendModeSaving && !schedArming && !$('tosync').contains(document.activeElement)) {
      applyModeControls(p.mode, p.autoQuietSecs, p.scheduleTimes, p.scheduleCatchup);
    } else updateSendButton();
    renderPendingTree(p.tree);
  } catch (e) {
    $('pendingTree').innerHTML = `<div class="empty">error: ${esc(e.message)}</div>`;
  }
}

// The single save path for the whole strip: mode, quiet window, schedule times
// and the catch-up flag go in ONE PUT, because the server writes them in one
// validated file edit (`scheduled` with no times is a validation error, so the
// mode cannot be saved apart from its times). A 400/422 renders the server's own
// message inline — for a scheduled save with no times that is the validator's
// actionable "needs at least one send time", and the config on disk is untouched.
async function saveSendMode() {
  const f = $('tosyncFlash');
  // Until the card has been painted from server values, the WHOLE strip is
  // read-only — not just its schedule half. Omitting the two schedule keys was
  // not enough: `mode` and `autoQuietSecs` are not optional on the backend, and
  // on an un-read page neither of them is the node's. The radiogroup can be
  // BLANK here (the mode guard in `wireTosync` undoes a refused selection), and
  // `currentMode()` reads blank as `auto`; an empty `#quietSecs` reads as 0. So
  // one click on a scheduled node would PUT `auto` + a zero quiet window over
  // its config, with a 200. Nothing may be written before something was read.
  if (!sendCfgSeen) {
    f.textContent = READING_HINT;
    f.className = 'flash';
    return;
  }
  const edit = {
    mode: currentMode(),
    autoQuietSecs: Number($('quietSecs').value) || 0,
    // Safe to speak about only because of the gate above: `scheduleTimes: []` is
    // an explicit clear the server obeys, and the boot value IS `[]` — sent
    // before the first read it would have erased the operator's send times.
    scheduleTimes: scheduleTimes.slice(),
    scheduleCatchup: $('schedCatchup').checked,
  };
  // Whatever this save does, the un-saved arming state is over: the request now
  // carries the operator's intent, and its outcome (or the next poll) is truth.
  setSchedArming(false);
  sendModeSaving++;
  try {
    const r = await api('/api/send-mode', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(edit) });
    if (r.status === 400 || r.status === 422) {
      f.textContent = 'rejected: ' + (await r.text()); f.className = 'flash err';
      // The page is now holding a state the server refused (e.g. the schedule the
      // operator just emptied, which is still on disk). Re-read the truth instead
      // of waiting for a tick that the focus guard may never let through — two
      // out-of-step lists is how the NEXT save silently drops what this rejection
      // preserved.
      await resyncSendMode();
      return;
    }
    if (!r.ok) throw new Error(await r.text());
    const m = await r.json();
    applyModeControls(m.mode, m.autoQuietSecs, m.scheduleTimes, m.scheduleCatchup);
    f.textContent = 'saved'; f.className = 'flash ok';
    // The armed deadline moves with the schedule; re-read it now rather than
    // waiting out the tick, so "Next scheduled send" agrees with what was saved.
    refreshStatus();
  } catch (e) { f.textContent = 'save failed: ' + e.message; f.className = 'flash err'; }
  finally { sendModeSaving--; }
}

// Re-read the send config and repaint the card without the poll's focus guard:
// that guard protects an edit in progress, and this runs after an edit has
// already been decided (and refused), so it would only preserve a fiction.
// `applyModeControls` still leaves the two live inputs (`#quietSecs`,
// `#schedCatchup`) alone if the caret is inside them — repainting under the
// operator's fingers is a different hazard, and this path does not license it.
async function resyncSendMode() {
  try {
    const m = await getJson('/api/send-mode');
    applyModeControls(m.mode, m.autoQuietSecs, m.scheduleTimes, m.scheduleCatchup);
  } catch (e) {
    console.error('[tosync] could not re-read the send mode:', e);
  }
}

// Add the time in the picker. Rejected client-side only for the two cases the
// operator can see and fix here (not a time / already listed); everything else is
// the server's call.
function schedAddTime() {
  const f = $('tosyncFlash');
  const raw = $('schedAdd').value;
  const t = normTime(raw);
  if (!t) { f.textContent = 'enter a time as HH:MM'; f.className = 'flash err'; return; }
  if (scheduleTimes.indexOf(t) >= 0) { f.textContent = t + ' is already scheduled'; f.className = 'flash'; return; }
  scheduleTimes = normalizeTimes(scheduleTimes.concat([t]));
  $('schedAdd').value = '';
  renderScheduleTimes();
  saveSendMode();
}

// Remove one point. Saving an EMPTY schedule while in scheduled mode is allowed
// to reach the server on purpose: it comes back as the validator's 422 with the
// file untouched, which is a truthful "you must keep at least one" — better than
// a client-side rule that silently disagrees with the config contract.
function schedRemoveTime(t) {
  const before = scheduleTimes.length;
  scheduleTimes = scheduleTimes.filter((x) => x !== t);
  if (scheduleTimes.length === before) return;   // nothing matched — no PUT
  renderScheduleTimes();
  saveSendMode();
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
  // Start from the un-armed state, so the marker in the markup and the flag can
  // only ever have been brought together by `setSchedArming`.
  setSchedArming(false);
  document.querySelectorAll('input[name="sendMode"]').forEach((r) => r.addEventListener('change', () => {
    const f = $('tosyncFlash');
    if (currentMode() === 'scheduled' && !sendCfgSeen) {
      // The node's schedule has not been read yet. Opening the editor now would
      // show an empty list that is NOT the node's, and every edit made in it
      // would speak about times the page has never seen. Undo the selection
      // back to the un-painted card and say why.
      $('modeScheduled').checked = false;
      updateModeVisibility(); updateSendButton();
      f.textContent = READING_HINT;
      f.className = 'flash';
      return;
    }
    updateModeVisibility(); updateSendButton();
    if (currentMode() !== 'scheduled') {
      // Leaving the schedule abandons whatever was held un-saved with it — the
      // save below carries the mode actually chosen.
      setSchedArming(false);
    } else if (!scheduleTimes.length) {
      // Hold the selection un-saved (see `schedArming`) and ask for the missing
      // piece instead of PUTting something the config would refuse.
      setSchedArming(true);
      f.textContent = 'add at least one send time — the switch is saved with it';
      f.className = 'flash';
      $('schedAdd').focus();
      return;
    }
    saveSendMode();
  }));
  $('quietSecs').addEventListener('change', saveSendMode);
  $('quietSecs').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); saveSendMode(); } });
  // Schedule editor. One delegated listener on the chip list (rows are re-rendered
  // on every change, so per-row listeners would have to be re-attached — and
  // leaked — each time).
  $('schedAddBtn').addEventListener('click', schedAddTime);
  $('schedAdd').addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); schedAddTime(); } });
  $('schedList').addEventListener('click', (e) => {
    const btn = e.target.closest('[data-sched-del]');
    if (btn) schedRemoveTime(btn.dataset.schedDel);
  });
  $('schedCatchup').addEventListener('change', saveSendMode);
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
  // §6: only a settled batch may be sent on — a live one is still serving from
  // the package dir this would copy (the route refuses it too).
  const stBtn = batchAllTerminal(b) && (b.targets || []).length
    ? `<button class="ghost" data-act="send-to" data-ref="${esc(b.packageRef)}" title="Send this batch to another device as a new transfer">Send to device…</button>`
    : '';
  const files = `${b.fileCount} file${b.fileCount === 1 ? '' : 's'}`;
  return `<div class="trow${open ? ' open' : ''}" data-batch-toggle data-ref="${esc(b.packageRef)}">
    <div class="trow-main">
      <span class="trow-name">${esc(transferName(b))}</span>
      ${markers.join(' ')}
      <span class="spacer"></span>
      <span class="trow-actions">${stBtn}${dfBtn}${dhBtn}</span>
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
  if (act === 'send-to') { sendToOpen(btn.dataset.ref); return; }
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

// ── send a recorded batch to another device (§6) ──
// The one action on this page that CREATES something, so it gets a real dialog
// rather than a one-line confirm: pick the device, read what would travel, then
// send. Two round trips against one route — the preview (`confirm:false`) counts
// resolvable sources and builds nothing, the send (`confirm:true`) performs —
// because "sends 2 of 3 (1 deleted locally)" has to be on screen BEFORE the
// operator commits. The counts can move between the two (a capture file can be
// deleted in between); the send's own numbers are the ones reported.
//
// Discipline mirrors the Library dialog: every round trip carries a ticket
// (`sendToSeq`) so a late response never paints into a closed or reopened
// dialog, Escape always closes (even mid-write — the request finishes on the
// server and the list shows the truth), and Enter is deliberately unbound.
let sendToDlg = null;   // the open dialog's whole state, or null
let sendToSeq = 0;      // monotonic; a stale response never paints

function sendToOpen(ref) {
  const b = transfersData.find((x) => x.packageRef === ref);
  if (!b) return;
  // Any of the batch's rows will do: every fan-out row shares the package dir,
  // and the server re-reads the batch from it.
  const source = (b.targets || [])[0];
  if (!source) return;
  if (sendToDlg) sendToClose();
  transferDialogOpen = true;          // freeze the list under the dialog
  const overlay = document.createElement('div');
  overlay.className = 'tconfirm-overlay';
  overlay.addEventListener('click', onSendToClick);
  document.body.appendChild(overlay);
  sendToDlg = {
    overlay, ref, id: source.rowId, name: transferName(b), fileCount: b.fileCount,
    seq: ++sendToSeq, phase: 'loading', busy: false, error: null,
    targets: [], targetsOk: false, waiting: [], picked: null,
    sent: 0, skipped: 0, newId: null,
  };
  document.addEventListener('keydown', onSendToKey);
  sendToRender();
  sendToLoadTargets();
}

function sendToClose() {
  if (!sendToDlg) return;
  const wasBusy = sendToDlg.busy;
  sendToSeq++;                        // orphan every in-flight response
  sendToDlg.overlay.remove();
  sendToDlg = null;
  document.removeEventListener('keydown', onSendToKey);
  transferDialogOpen = false;
  if (wasBusy) transferFlash('send still running — the list will show the result', true);
  refreshTransfers();                 // paint whatever the server now holds
}

function onSendToKey(e) {
  if (!sendToDlg) return;
  if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
  if (e.key !== 'Escape') return;
  e.preventDefault();
  sendToClose();
}

async function sendToLoadTargets() {
  const seq = sendToDlg.seq;
  const res = await loadSendTargets();
  if (!sendToDlg || sendToDlg.seq !== seq) return;
  sendToDlg.targetsOk = res.ok;
  sendToDlg.targets = res.targets;
  sendToDlg.waiting = res.waiting;
  sendToDlg.error = res.ok ? null : 'could not read the send targets: ' + res.error;
  // ONE device per send — this mints a transfer aimed at a single peer, not a
  // fan-out — so a single-target node is pre-picked and anything else asks.
  sendToDlg.picked = res.targets.length === 1 ? res.targets[0] : null;
  sendToDlg.phase = 'form';
  sendToRender();
}

// Focus is re-applied on EVERY render, not only on a phase change: a radio
// group is driven by the arrow keys, which fire clicks, which re-render — so
// without this the second arrow press would have nothing focused to move from.
// There is no text input in this dialog, so nothing can be interrupted.
function sendToRender() {
  const d = sendToDlg;
  if (!d) return;
  const parts = sendToParts(d);
  d.overlay.innerHTML = `
    <div class="tconfirm" role="dialog" aria-modal="true" aria-label="Send to device">
      <h3>Send to device</h3>
      <div class="tconfirm-body">${parts.body}</div>
      <div class="tconfirm-actions">${parts.actions}</div>
    </div>`;
  const focus = d.overlay.querySelector('#tsendFocus');
  if (focus && focus.focus) focus.focus();
}

// `focusHere` marks the element focus lands on after a render (see sendToRender).
const sendToCancelBtn = (label, focusHere) =>
  `<button class="ghost" data-tsend="cancel"${focusHere ? ' id="tsendFocus"' : ''}>${esc(label)}</button>`;
const sendToErrHtml = (msg) => (msg ? `<p class="tsend-err">${esc(msg)}</p>` : '');
const sendToFiles = (n) => n + (n === 1 ? ' file' : ' files');

function sendToParts(d) {
  const subject = `<b>${esc(d.name)}</b>`;
  if (d.phase === 'loading') {
    return { body: '<div class="empty">reading this node\'s send targets…</div>', actions: sendToCancelBtn('Cancel', true) };
  }

  if (d.phase === 'blocked') {
    // The batch itself cannot be sent (its files are gone) — no device choice
    // fixes that, so the picker is gone and only the way out remains.
    return { body: `<p>Send ${subject} to another device?</p>${sendToErrHtml(d.error)}`, actions: sendToCancelBtn('Close', true) };
  }

  if (d.phase === 'preview') {
    const of = d.skipped ? ` of ${d.sent + d.skipped}` : '';
    const gone = d.skipped
      ? `<p class="tconfirm-warn">${d.skipped} no longer on disk — ${d.skipped === 1 ? 'it is' : 'they are'} left out of this send.</p>`
      : '';
    return {
      body: `<p>Sends ${sendToFiles(d.sent)}${of} to <b>${esc(libTargetLabel(d.picked))}</b>, as a new transfer.</p>`
        + gone
        + '<p class="muted">The original batch and its history stay as they are; the receiving device skips whatever it already has.</p>'
        + sendToErrHtml(d.error),
      actions: sendToCancelBtn(d.busy ? 'Close' : 'Back', false)
        + `<button data-tsend="send" id="tsendFocus"${d.busy ? ' disabled' : ''}>${esc(d.busy ? 'Sending…' : 'Send ' + sendToFiles(d.sent))}</button>`,
    };
  }

  if (d.phase === 'result') {
    const gone = d.skipped ? ` (${d.skipped} no longer on disk)` : '';
    return {
      body: `<p class="tsend-ok">Sent ${sendToFiles(d.sent)}${gone} to ${esc(libTargetLabel(d.picked))} as transfer #${d.newId}.</p>`
        + '<p class="muted">It is queued now — the new row in this list carries its progress.</p>',
      actions: sendToCancelBtn('Close', true),
    };
  }

  // phase 'form' — the device picker. Focus rides the picked row (or the first
  // one) so the arrow keys walk the group.
  const focusOn = d.picked && d.targets.includes(d.picked) ? d.picked : d.targets[0];
  const rows = d.targets.map((t) => {
    const raw = String(t);
    const view = libTargetView(raw);
    return `<label class="tsend-target" data-tsend-target="${esc(raw)}">
      <input type="radio" name="tsendTarget"${raw === focusOn ? ' id="tsendFocus"' : ''}${d.picked === raw ? ' checked' : ''}${d.busy ? ' disabled' : ''} />
      <span>${esc(view.label)}</span>${view.hint
      ? `<span class="muted mono" title="${esc(view.hint)}">${esc(shortHex(view.hint))}…</span>` : ''}
    </label>`;
  }).join('');
  // Same two empties as the Library picker: a list that loaded empty has a fix
  // to point at, one that failed to load has nothing honest to say about it.
  const noTargets = d.targetsOk
    ? '<p class="tconfirm-warn">No device is reachable from this node right now — the sync engine is not running, or it has no send targets. Add one under Settings → Send Targets.</p>'
    : '';
  const premise = d.targets.length
    ? '<p class="muted">These are this node\'s configured send targets — one that failed to start will refuse the send and say so.</p>'
    : '';
  const waiting = d.waiting.length
    ? `<p class="muted">Saved but not applied yet: ${d.waiting.map((t) => esc(libTargetLabel(t))).join(', ')} — ${d.waiting.length === 1 ? 'it becomes' : 'they become'} sendable once the sync engine restarts.</p>`
    : '';
  return {
    body: `<p>Send ${subject} (${sendToFiles(d.fileCount)}) to:</p>`
      + (d.targets.length ? `<div class="tsend-targets">${rows}</div>` : noTargets)
      + premise + waiting + sendToErrHtml(d.error),
    actions: sendToCancelBtn('Cancel', !d.targets.length)
      + `<button data-tsend="preview"${(d.picked && !d.busy) ? '' : ' disabled'}>${esc(d.busy ? 'Checking…' : 'Continue')}</button>`,
  };
}

function onSendToClick(e) {
  const d = sendToDlg;
  if (!d) return;
  if (e.target === d.overlay || e.target.closest('[data-tsend="cancel"]')) {
    // From the preview, Cancel means "back to the picker" — nothing was built
    // yet, and re-opening the whole dialog to change device would be silly.
    if (d.phase === 'preview' && !d.busy) { d.phase = 'form'; d.error = null; sendToRender(); return; }
    sendToClose();
    return;
  }
  const pick = e.target.closest('[data-tsend-target]');
  if (pick) { d.picked = pick.dataset.tsendTarget; d.error = null; sendToRender(); return; }
  if (e.target.closest('[data-tsend="preview"]')) { sendToStep(false); return; }
  if (e.target.closest('[data-tsend="send"]')) { sendToStep(true); return; }
}

// One step of the two-step route. `confirm:false` fills the preview, `true`
// performs — the only difference on this side is which phase the answer lands in.
async function sendToStep(confirm) {
  const d = sendToDlg;
  if (!d || d.busy || !d.picked) return;
  const seq = d.seq;
  d.busy = true;
  d.error = null;
  sendToRender();

  let res = null;
  let error = null;
  try {
    res = await api('/api/transfers/send-to', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id: d.id, target: d.picked, confirm }),
    });
  } catch (e) { error = (e && e.message) ? e.message : String(e); }
  if (!sendToDlg || sendToDlg.seq !== seq) return;   // closed / reopened under us
  let gone = false;
  if (!error && !res.ok) {
    gone = res.status === 409;
    let body = '';
    try { body = await res.text(); } catch (e) { body = ''; }
    if (!sendToDlg || sendToDlg.seq !== seq) return;
    error = String(body || '').trim() || ('HTTP ' + res.status);
  }
  if (error) {
    console.error('[transfers] send-to failed:', error);
    sendToDlg.busy = false;
    sendToDlg.error = error;
    // A 409 is about the batch, not the chosen device: no other pick helps, so
    // the picker gives way to a dead end. Everything else stays editable.
    if (gone) sendToDlg.phase = 'blocked';
    sendToRender();
    transferFlash('send to device failed: ' + error, false);
    return;
  }
  let rep = null;
  try { rep = await res.json(); } catch (e) { rep = {}; }
  if (!sendToDlg || sendToDlg.seq !== seq) return;
  sendToDlg.busy = false;
  sendToDlg.sent = Number(rep.sent) || 0;
  sendToDlg.skipped = Number(rep.skipped) || 0;
  if (confirm) {
    sendToDlg.newId = rep.newId;
    sendToDlg.phase = 'result';
    transferFlash(`sent to ${libTargetLabel(sendToDlg.picked)} as a new transfer`, true);
    refreshTransfers();               // fetch now; the list paints on close
  } else {
    sendToDlg.phase = 'preview';
  }
  sendToRender();
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

// ── library: capture-root browser (roots → one directory at a time) ─────────
// A LAZY browser: exactly one GET /api/library per navigation (the server does a
// single read_dir, never a walk) and NO poll of its own — a capture root is
// routinely a NAS share and a 2 s re-listing would hammer it. The operator
// re-reads with the ↻ button; only the roots view's free-space chips ride the
// existing /api/status tick, which is memoized server-side.
//
// `libState` is the contract the preview pane below and the later send / delete
// tasks read:
//   root         index into the configured capture roots, or null = roots view
//   path         forward-slash wire rel-path of the listed directory ("" = root)
//   selected     Set of rel-paths picked IN THE CURRENT ROOT
//   entries      the current directory's file rows (T4 `LibraryEntry`)
//   selectedDirs the subset of `selected` that are directories — a directory
//                means "everything under it", resolved server-side, so the flag
//                has to travel with the item (see libSelectedItems)
// Selection deliberately survives navigation WITHIN a root (pick a few subs in
// one night's folder, descend into the next, act on both) and is cleared the
// moment the root changes, so every rel in `selected` belongs to `libState.root`.
let libState = {
  root: null,
  path: '',
  selected: new Set(),
  selectedDirs: new Set(),
  entries: [],
  dirs: [],
  error: null,
  loading: false,
};

let libRoots = [];          // configured capture roots; the index IS the API's `root`
let libVolumes = [];        // last /api/status `volumes` (free space per volume)
let libStatusSeen = false;  // a status landed → the roots view can speak honestly
let libStatusSig = null;    // skip the roots re-render when nothing moved
let libLoadSeq = 0;         // monotonic; a stale listing response never paints
// The PATH of the root being browsed, remembered on entry. `libState.root` is an
// index into a list the operator can edit under Settings, so the index alone is
// not an identity: dropping an earlier capture dir silently re-points it at a
// different folder. See the guard in libOnStatus.
let libRootPath = null;

// The chip tooltips. `delivered` is deliberately "at least one device": the
// listing status is ANY-of across a fan-out, and per-target detail is the
// Transfers tab's job, not this chip's.
const LIB_STATUS_TITLE = {
  unsent: 'never enqueued',
  queued: 'accumulated — waiting for the next flush',
  sending: 'in flight to at least one device',
  delivered: 'delivered to at least one device',
  declined: 'every device that answered declined it',
  sent: 'handed to the sync engine',
};

const libJoin = (path, name) => (path ? path + '/' + name : name);

// Boundary-aware prefix test over DISPLAY paths (a root may be `/mnt/…`,
// `C:\…` or a UNC share): `/mnt/data` must not match `/mnt/database`.
function libPathUnder(p, prefix) {
  if (!prefix || !p.startsWith(prefix)) return false;
  if (p.length === prefix.length) return true;
  const last = prefix[prefix.length - 1];
  if (last === '/' || last === '\\') return true;
  const next = p[prefix.length];
  return next === '/' || next === '\\';
}

// A `volumes` entry is labelled with the FIRST probed path that landed on that
// volume, so a root is matched by longest CONTAINING prefix, not equality — that
// is what puts a chip on a capture root sitting under the data dir's volume.
//
// A root with no containing label gets NO chip, and that is deliberate. Two
// SIBLING roots on one disk (`…/cam1`, `…/cam2`) collapse to a single entry
// labelled `…/cam1`, and nothing on the wire says the second one belongs to it:
// device ids never travel, and a volume whose probe FAILED (an offline share) is
// simply absent from the list. So "it must be the earlier volume" is a guess
// that, for an unmounted NAS root, would print the local disk's free space under
// the NAS's name — the exact wrong-disk reading `diskspace.rs` refuses to commit
// by never resolving a path to an ancestor. A missing chip is the honest answer;
// closing the gap needs the status DTO to say which volume each root is on.
function libVolumeFor(rootPath) {
  let best = null;
  for (const v of libVolumes) {
    const vr = String(v && v.root != null ? v.root : '');
    if (!libPathUnder(rootPath, vr)) continue;
    if (!best || vr.length > String(best.root).length) best = v;
  }
  return best;
}

// Crumb label for a root: its last path segment, with the full configured path
// in the title (a capture root is often a long share path).
function libRootName(idx) {
  const p = libRoots[idx];
  if (p == null) return 'root #' + idx;
  const segs = String(p).split(/[\\/]+/).filter(Boolean);
  return segs.length ? segs[segs.length - 1] : String(p);
}

// ── section markup ──
function renderLibraryTab() {
  $('tab-library').innerHTML = `
    <section id="library">
      <div class="lib-head">
        <h2>Library</h2>
        <div class="lib-crumbs" id="libCrumbs" aria-label="Location"></div>
        <span class="spacer"></span>
        <button class="ghost" id="libRefreshBtn" data-lib-act="refresh" title="Re-read this folder">&#8635;</button>
      </div>
      <div id="libBody"></div>
      <div class="flash" id="libFlash"></div>
      <div class="lib-footer" id="libFooter" hidden></div>
    </section>`;
}

// ── render: breadcrumbs ──
function renderLibCrumbs() {
  const el = $('libCrumbs');
  if (!el) return;
  if (libState.root === null) {
    el.innerHTML = '<span class="lib-crumb-cur">Capture roots</span>';
    return;
  }
  const rootPath = String(libRoots[libState.root] ?? '');
  const segs = libState.path ? libState.path.split('/') : [];
  const sep = '<span class="lib-crumb-sep">/</span>';
  const html = ['<button class="lib-crumb" data-lib-act="roots">Capture roots</button>', sep];
  html.push(segs.length
    ? `<button class="lib-crumb" data-lib-nav="" title="${esc(rootPath)}">${esc(libRootName(libState.root))}</button>`
    : `<span class="lib-crumb-cur" title="${esc(rootPath)}">${esc(libRootName(libState.root))}</span>`);
  let acc = '';
  segs.forEach((s, i) => {
    acc = libJoin(acc, s);
    html.push(sep);
    html.push(i === segs.length - 1
      ? `<span class="lib-crumb-cur">${esc(s)}</span>`
      : `<button class="lib-crumb" data-lib-nav="${esc(acc)}">${esc(s)}</button>`);
  });
  el.innerHTML = html.join('');
}

// ── render: the roots list (top level) ──
function renderLibRoots() {
  const body = $('libBody');
  if (!body) return;
  if (!libStatusSeen) { body.innerHTML = '<div class="empty">loading capture roots…</div>'; return; }
  if (!libRoots.length) {
    body.innerHTML = '<div class="empty">no capture directories configured — add one under Settings → Capture Directories</div>';
    return;
  }
  body.innerHTML = libRoots.map((p, i) => {
    const path = String(p);
    const v = libVolumeFor(path);
    let chip = '';
    if (v) {
      const low = Number(v.freeBytes) < FREE_LOW_BYTES;
      const title = `${v.root} — ${fmtSize(v.freeBytes)} free of ${fmtSize(v.totalBytes)}`;
      chip = `<span class="chip vol${low ? ' chip-danger' : ''}" title="${esc(title)}">Free: ${esc(fmtSize(v.freeBytes))}</span>`;
    }
    return `<button class="lib-root" data-lib-root="${i}">
      <span class="lib-caret" aria-hidden="true">&#9656;</span>
      <span class="mono">${esc(path)}</span>
      <span class="spacer"></span>
      ${chip}
    </button>`;
  }).join('');
}

// ── render: one directory ──
function libStatusChip(f) {
  const st = String(f.status || 'unsent');
  // Own-property lookup only: `status` is server-controlled, and a plain `[st]`
  // would resolve `toString` / `constructor` off Object.prototype and stringify
  // a function into the tooltip.
  const known = Object.prototype.hasOwnProperty.call(LIB_STATUS_TITLE, st);
  const title = known ? LIB_STATUS_TITLE[st] : st;
  // The class token rides the SAME whitelist. An unknown status echoed into
  // `class` would collide with unrelated rules on this page (`.chip.safe`,
  // `.chip.auto`, …) and paint a colour nobody derived — a neutral chip is the
  // honest rendering. The text is still echoed (escaped), so the operator sees
  // exactly what the server said.
  const cls = known ? ' ' + st : '';
  const n = Number(f.batches) || 0;
  const suffix = n > 0
    ? ` <span class="lib-batches" title="in ${n} batch${n === 1 ? '' : 'es'}">&times;${n}</span>`
    : '';
  return `<span class="chip lib-st${cls}" title="${esc(title)}">${esc(st)}</span>${suffix}`;
}

// The T15 per-file fate line: what retention intends to do with THIS file, as a
// muted line under the status chip. The server sends `null` under
// `keep_everything` (a node that deletes nothing has no fate to state), and only
// ever sends free text — so it is escaped, never classed on.
//
// It is a separate fact from the chip and may legitimately disagree with it: a
// re-captured frame reads `sending` while an older confirmed batch already makes
// it a deletion candidate.
function libFateLine(f) {
  const fate = f.retention;
  if (typeof fate !== 'string' || !fate) return '';
  return `<div class="lib-fate muted" title="What retention will do with this file — see Settings &rarr; Retention">${esc(fate)}</div>`;
}

function libRowMarkup(rel, cells) {
  const picked = libState.selected.has(rel);
  return `<tr${picked ? ' class="lib-picked"' : ''}>${cells(picked)}</tr>`;
}

function renderLibListing() {
  const body = $('libBody');
  if (!body) return;
  // An error NEVER degrades to an empty listing — an unmounted share must not
  // read as "this folder has nothing in it" (spec §7).
  if (libState.error) {
    body.innerHTML = `<div class="lib-error">
      <div>
        <div><b>Could not list this folder</b></div>
        <div class="lib-error-msg mono">${esc(libState.error)}</div>
      </div>
      <span class="spacer"></span>
      <button class="ghost" data-lib-act="retry">Retry</button>
    </div>`;
    return;
  }
  if (libState.loading) { body.innerHTML = '<div class="empty">listing…</div>'; return; }
  const dirs = libState.dirs || [];
  const files = libState.entries || [];
  if (!dirs.length && !files.length) { body.innerHTML = '<div class="empty">this folder is empty</div>'; return; }

  const dirRows = dirs.map((name) => {
    const rel = libJoin(libState.path, String(name));
    return libRowMarkup(rel, (picked) => `
      <td class="lib-sel"><input type="checkbox" data-lib-pick="${esc(rel)}" data-lib-dir="1"
        aria-label="Select folder ${esc(name)} and everything under it"${picked ? ' checked' : ''} /></td>
      <td><button class="lib-dir" data-lib-nav="${esc(rel)}">
        <span class="lib-caret" aria-hidden="true">&#9656;</span> ${esc(name)}/</button></td>
      <td class="muted">–</td><td class="muted">–</td><td class="muted">–</td>`);
  }).join('');

  const fileRows = files.map((f) => {
    const rel = libJoin(libState.path, String(f.name));
    return libRowMarkup(rel, (picked) => `
      <td class="lib-sel"><input type="checkbox" data-lib-pick="${esc(rel)}"
        aria-label="Select ${esc(f.name)}"${picked ? ' checked' : ''} /></td>
      <td><button class="lib-file mono" data-lib-pv="${esc(rel)}"
        title="Preview ${esc(f.name)}">${esc(f.name)}</button></td>
      <td>${esc(fmtSize(f.size))}</td>
      <td class="mono muted">${esc(fmtMtime(f.mtimeMs))}</td>
      <td>${libStatusChip(f)}${libFateLine(f)}</td>`);
  }).join('');

  body.innerHTML = `<div class="tablewrap"><table>
    <thead><tr><th class="lib-sel"></th><th>name</th><th>size</th><th>modified</th><th>status</th></tr></thead>
    <tbody>${dirRows}${fileRows}</tbody></table></div>`;
}

// ── render: selection footer ──
// Send to… / Delete open the T10 dialogs over the WHOLE selection (every root-
// relative pick, including the ones scrolled out of sight — hence the "N outside
// this folder" note beside the count). Preview is the odd one out (T7): it opens
// the first selected file OF THE LISTED FOLDER, because that folder's files are
// exactly what the pane's ←/→ walk covers — a selection reaching into a folder
// that is no longer on screen has no walk to join, so the button says so instead.
function renderLibFooter() {
  const el = $('libFooter');
  if (!el) return;
  const n = libState.selected.size;
  if (!n || libState.root === null) { el.hidden = true; el.innerHTML = ''; return; }
  // Selection spans the whole root, so say plainly how much of it is out of
  // sight — an operator must never act on rows they cannot see.
  const here = new Set([
    ...(libState.dirs || []).map((d) => libJoin(libState.path, String(d))),
    ...(libState.entries || []).map((f) => libJoin(libState.path, String(f.name))),
  ]);
  let outside = 0;
  libState.selected.forEach((rel) => { if (!here.has(rel)) outside++; });
  const note = outside ? ` <span class="muted">· ${outside} outside this folder</span>` : '';
  const pv = libPvFirstSelectedFile();
  const pvTitle = pv ? 'Preview ' + pv.name : 'select a file in this folder to preview';
  const sendTitle = 'Send this selection to a device now, as one batch';
  const delTitle = 'Delete this selection from disk (you see the consequences first)';
  el.hidden = false;
  el.innerHTML = `<span><b>${n}</b> selected${note}</span>
    <button class="ghost" data-lib-act="clear">Clear</button>
    <span class="spacer"></span>
    <button data-lib-act="send" title="${esc(sendTitle)}">Send to…</button>
    <button class="ghost" data-lib-act="delete" title="${esc(delTitle)}">Delete</button>
    <button class="ghost" data-lib-act="preview"${pv ? '' : ' disabled'} title="${esc(pvTitle)}">Preview</button>`;
}

function libRender() {
  renderLibCrumbs();
  // ↻ does different work per view (status re-poll vs. one listing GET), so it
  // says which — a button whose tooltip describes the other view reads as broken.
  const rb = $('libRefreshBtn');
  if (rb) rb.title = libState.root === null ? 'Re-read the capture roots' : 'Re-read this folder';
  if (libState.root === null) renderLibRoots(); else renderLibListing();
  renderLibFooter();
}

// ── selection ──
function libClearSelection() {
  libState.selected.clear();
  libState.selectedDirs.clear();
}

function libTogglePick(rel, isDir, on) {
  if (on) {
    libState.selected.add(rel);
    if (isDir) libState.selectedDirs.add(rel);
  } else {
    libState.selected.delete(rel);
    libState.selectedDirs.delete(rel);
  }
  // Only the footer depends on the selection — re-rendering the table here would
  // yank focus off the checkbox the operator just clicked.
  renderLibFooter();
}

// The current selection as the later tasks consume it. `dir: true` means "this
// rel is a directory — everything under it", expanded server-side.
function libSelectedItems() {
  if (libState.root === null) return [];
  return Array.from(libState.selected, (rel) => ({
    root: libState.root,
    rel,
    dir: libState.selectedDirs.has(rel),
  }));
}

// ── navigation ──
function libShowRoots() {
  libLoadSeq++;                 // abandon any in-flight listing
  // The pane walks ONE directory of ONE root, and a dialog holds a selection
  // snapshot taken inside one root. Leaving that root (the operator's click, or
  // the identity guard in libOnStatus retreating from a re-pointed index) leaves
  // the walk list addressing a folder nobody is browsing — and would leave the
  // dialog about to act on rel-paths whose root index now means something else.
  libPvClose();
  libDlgClose();
  libClearSelection();          // a rel only means something inside its root
  libState.root = null;
  libRootPath = null;
  libState.path = '';
  libState.dirs = [];
  libState.entries = [];
  libState.error = null;
  libState.loading = false;
  libRender();
}

async function libLoad(root, path) {
  const rel = path || '';
  if (root !== libState.root) libClearSelection();
  libState.root = root;
  libRootPath = String(libRoots[root] ?? '');
  libState.path = rel;
  libState.error = null;
  libState.loading = true;
  libRender();
  const seq = ++libLoadSeq;
  let listing = null;
  let error = null;
  try {
    listing = await getJson('/api/library?root=' + encodeURIComponent(root) + '&path=' + encodeURIComponent(rel));
  } catch (e) {
    error = (e && e.message) ? e.message : String(e);
  }
  if (seq !== libLoadSeq) return;   // a newer navigation already won — never paint
  libState.loading = false;
  if (error) {
    console.error('[library] listing failed:', root, rel, error);
    libState.error = error;
    libState.dirs = [];
    libState.entries = [];
  } else {
    libState.error = null;
    libState.dirs = Array.isArray(listing.dirs) ? listing.dirs : [];
    libState.entries = Array.isArray(listing.files) ? listing.files : [];
    // The server echoes the NORMALIZED rel-path; adopt it so the crumbs match
    // what the containment guard actually resolved.
    if (typeof listing.path === 'string') libState.path = listing.path;
  }
  libRender();
}

// Re-read what is on screen. A listing costs one GET; the roots view has no
// endpoint of its own — it is painted from `/api/status`, so re-reading it means
// re-polling status out of turn (libOnStatus then repaints only if something
// actually moved). Also the Retry button's action.
function libRefresh() {
  if (libState.root === null) { refreshStatus(); return; }
  libLoad(libState.root, libState.path);
}

// Fed by every /api/status poll. `captureDirs` is exactly the list the library
// `root` index addresses (both are `capture_dirs_resolved()`), so the roots view
// needs no fetch of its own.
function libOnStatus(s) {
  if (!s) {
    // Offline tick: drop the free-space chips rather than show a reading nobody
    // can vouch for, but KEEP the roots — the configured list did not vanish
    // because one poll hiccuped, and blanking it would read as "none configured".
    if (!libVolumes.length) return;
    libVolumes = [];
    libStatusSig = null;
    if (libState.root === null) renderLibRoots();
    return;
  }
  libStatusSeen = true;
  const roots = Array.isArray(s.captureDirs) ? s.captureDirs.map(String) : [];
  const vols = Array.isArray(s.volumes) ? s.volumes : [];
  const sig = JSON.stringify([roots, vols]);
  if (sig === libStatusSig) return;   // nothing moved → no reflow
  libStatusSig = sig;
  libRoots = roots;
  libVolumes = vols;
  // The browsed root is addressed by INDEX, and this list is what the index
  // addresses. An edit under Settings → Capture Directories can drop or reorder
  // an entry, after which the same index means a different folder — the crumbs
  // would relabel themselves and every rel in the selection would suddenly claim
  // to belong to a path nobody picked. Identity is the path, so when it stops
  // matching, retreat to the roots view (which clears the selection).
  if (libState.root !== null && String(libRoots[libState.root] ?? '') !== libRootPath) {
    libShowRoots();
    return;
  }
  // Only the roots view reads these; a listing keeps its own crumbs/rows.
  if (libState.root === null) { renderLibCrumbs(); renderLibRoots(); }
}

// ── preview pane: the pre-blink look at one frame (T7) ──────────────────────
// An overlay over `#libPreview` showing one frame from GET /api/library/preview,
// with ←/→ (buttons and keys) walking the listed folder. Four things shape it:
//
//  * The route is BEARER-GATED, so a bare `<img src=…>` cannot reach it on a
//    non-loopback node — the browser attaches no Authorization header to an
//    image load. Every frame therefore rides the same `api()` helper as the rest
//    of the page and reaches the <img> as an object URL. The detour keeps the
//    server's ETag fast path: `fetch` still revalidates out of the browser's
//    HTTP cache on its own, so a second pass over a folder costs 304s.
//  * Object URLs are owned, not leaked: exactly one is alive at a time, revoked
//    the moment it is replaced and again on close. A 200-frame blink walk holds
//    one blob, not two hundred.
//  * Arrow-mashing must never paint out of order. Every load takes a ticket
//    (`libPvSeq`, the guard `libLoad` already uses) and aborts the request it
//    replaces, so a slow first frame cannot overwrite the frame now on screen
//    and the server's single render slot is not queued full of junk nobody will
//    look at.
//  * An error NEVER closes the pane. The operator is mid-blink: a bad frame is
//    an inline message and the arrows keep working straight past it.
//
// The walk list is SNAPSHOT at open time (the listed directory's files, in
// listing order, directories skipped). A `404` re-reads the listing behind the
// pane, and a list re-shuffling under the operator's fingers mid-walk would be
// worse than a stale one — the position would jump and `→` would land somewhere
// nobody chose. The fresh listing is what they see when the pane closes.
//
// Every file is previewable-by-click, including a `.txt` the capture software
// dropped in: which extensions render is the server's contract (one list, in
// `watcher::is_eligible`), and a second copy here would drift. A non-frame comes
// back `415` and reads as "not a renderable frame" in the pane.
const LIB_PREVIEW_WIDTH = 1200;     // one width for every frame → one cache entry each

let libPvState = { open: false, root: null, path: '', files: [], idx: -1 };
let libPvSeq = 0;                   // monotonic; a stale frame never paints
let libPvAbort = null;              // in-flight request, aborted when superseded
let libPvUrl = null;                // the ONE live object URL
let libPvReturnFocus = null;        // element focus returns to on close
let libPvRefreshed = new Set();     // rels whose 404 already cost one listing re-read

// The listed folder's files as the pane walks them: display name + wire rel.
const libPvFiles = () => (libState.entries || []).map((f) => ({
  name: String(f.name),
  rel: libJoin(libState.path, String(f.name)),
}));

// The footer button's subject: the first SELECTED file that is in the folder on
// screen (listing order). Null → the button renders disabled.
function libPvFirstSelectedFile() {
  if (libState.root === null) return null;
  return libPvFiles().find((f) => libState.selected.has(f.rel)) || null;
}

function libPvOpen(rel) {
  if (libState.root === null) return;
  const files = libPvFiles();
  const idx = files.findIndex((f) => f.rel === rel);
  if (idx < 0) return;              // not a file of the listed folder — no walk to join
  const host = $('libPreview');
  if (!host) return;
  // Re-opening over an open pane would add a SECOND keydown listener (and orphan
  // the live object URL). The overlay covers the listing, so no click can do it
  // today; the guard is what keeps that true for the next caller.
  if (libPvState.open) libPvClose();
  libPvState = { open: true, root: libState.root, path: libState.path, files, idx };
  libPvRefreshed = new Set();
  libPvReturnFocus = document.activeElement || null;
  host.innerHTML = `
    <div class="lib-pv" id="libPvDialog" role="dialog" aria-modal="true" aria-label="Frame preview" tabindex="-1">
      <div class="lib-pv-head" id="libPvHead"></div>
      <div class="lib-pv-stage">
        <img class="lib-pv-img" id="libPvImg" alt="" hidden />
        <div class="lib-pv-note" id="libPvNote" role="status"></div>
      </div>
    </div>`;
  host.hidden = false;
  document.addEventListener('keydown', onLibPvKey);
  libPvShow(idx);
  const dlg = $('libPvDialog');
  if (dlg && dlg.focus) dlg.focus();
}

function libPvClose() {
  if (!libPvState.open) return;
  libPvSeq++;                       // orphan every in-flight load
  if (libPvAbort) { libPvAbort.abort(); libPvAbort = null; }
  if (libPvUrl) { URL.revokeObjectURL(libPvUrl); libPvUrl = null; }
  document.removeEventListener('keydown', onLibPvKey);
  libPvState = { open: false, root: null, path: '', files: [], idx: -1 };
  libPvRefreshed = new Set();
  const host = $('libPreview');
  // Emptying the host drops the <img> and with it the onload/onerror closures.
  if (host) { host.hidden = true; host.innerHTML = ''; }
  const back = libPvReturnFocus;
  libPvReturnFocus = null;
  // Only if it is still in the document — the row it came from may have been
  // re-rendered out from under it by the 404 refresh.
  if (back && back.focus && (!document.contains || document.contains(back))) back.focus();
}

// Stop at the ends rather than wrap: mid-walk a wrap is indistinguishable from
// "this folder is shorter than I thought", and the arrow that would do it is
// rendered disabled, so nothing about stopping is a surprise.
function libPvStep(delta) {
  if (!libPvState.open) return;
  const next = libPvState.idx + delta;
  if (next < 0 || next >= libPvState.files.length) return;
  libPvShow(next);
}

function libPvRenderHead() {
  const el = $('libPvHead');
  if (!el) return;
  const n = libPvState.files.length;
  const i = libPvState.idx;
  const cur = libPvState.files[i];
  el.innerHTML = `
    <span class="lib-pv-name mono" title="${esc(cur ? cur.rel : '')}">${esc(cur ? cur.name : '')}</span>
    <span class="spacer"></span>
    <span class="lib-pv-pos muted">${i + 1} / ${n}</span>
    <button class="ghost" data-pv-step="-1" title="Previous frame (←)" aria-label="Previous frame"${i <= 0 ? ' disabled' : ''}>&#8592;</button>
    <button class="ghost" data-pv-step="1" title="Next frame (→)" aria-label="Next frame"${i >= n - 1 ? ' disabled' : ''}>&#8594;</button>
    <button class="ghost" data-pv-act="close" title="Close (Esc)" aria-label="Close preview">&#10005;</button>`;
}

// The note is the pane's one status line (loading, then any failure). The
// element carries role="status", so each replacement is announced.
function libPvSay(html) {
  const el = $('libPvNote');
  if (!el) return;
  el.hidden = false;
  el.innerHTML = html;
}

function libPvFail(head, detail) {
  const img = $('libPvImg');
  if (img) img.hidden = true;
  libPvSay(`<div class="lib-pv-err"><b>${esc(head)}</b>`
    + (detail ? `<div class="lib-pv-errmsg mono">${esc(detail)}</div>` : '')
    + `<div class="lib-pv-errfoot"><button class="ghost" data-pv-act="retry">Retry</button></div></div>`);
}

// Show frame `idx`: paint the head from what is already known (name, position,
// which arrow is at an end), blank the stage, and start the load. The image is
// HIDDEN while the next frame renders rather than left dimmed underneath — a
// stale frame under a fresh caption is exactly the misread a blink comparison
// must not make. The stage keeps its height, so nothing jumps.
function libPvShow(idx) {
  const files = libPvState.files;
  if (!libPvState.open || !files.length) return;
  const i = Math.max(0, Math.min(files.length - 1, idx));
  libPvState.idx = i;
  const cur = files[i];
  const seq = ++libPvSeq;
  if (libPvAbort) { libPvAbort.abort(); libPvAbort = null; }
  libPvRenderHead();
  const img = $('libPvImg');
  if (img) { img.hidden = true; img.alt = 'Preview of ' + cur.name; }
  libPvSay('<span class="lib-pv-spin" aria-hidden="true"></span>rendering…');
  libPvLoad(seq, cur);
}

function libPvUrlFor(root, rel) {
  return '/api/library/preview?root=' + encodeURIComponent(root)
    + '&path=' + encodeURIComponent(rel) + '&w=' + LIB_PREVIEW_WIDTH;
}

// Map a refusal onto what the operator needs to read. 415 and 404 get plain
// language (they are the two an operator hits routinely — a log file in the
// capture dir, a frame deleted since the listing); everything else says the
// status out loud and echoes the server's own words rather than paraphrasing a
// failure nobody predicted.
function libPvHttpError(status, body, cur) {
  const msg = String(body || '').trim();
  if (status === 415) { libPvFail('Not a renderable frame', msg); return; }
  if (status === 404) {
    libPvFail('File gone', msg || 'the file is no longer on disk');
    // The listing behind the pane is now lying (spec §2). Re-read it ONCE per
    // file: an operator arrowing back and forth across a deleted frame must not
    // turn into one listing GET per keypress against a network share.
    if (!libPvRefreshed.has(cur.rel)
      && libState.root === libPvState.root && libState.path === libPvState.path) {
      libPvRefreshed.add(cur.rel);
      libRefresh();
    }
    return;
  }
  libPvFail('Preview failed (HTTP ' + status + ')', msg);
}

async function libPvLoad(seq, cur) {
  const ac = (typeof AbortController === 'function') ? new AbortController() : null;
  libPvAbort = ac;
  const url = libPvUrlFor(libPvState.root, cur.rel);
  let res = null;
  try {
    res = await api(url, ac ? { signal: ac.signal } : {});
  } catch (e) {
    if (ac && ac.signal.aborted) return;      // superseded — the newer frame owns the pane
    if (seq !== libPvSeq) return;
    console.error('[library] preview request failed:', cur.rel, e);
    libPvFail('Preview failed', (e && e.message) ? e.message : String(e));
    return;
  }
  if (seq !== libPvSeq) return;
  if (!res.ok) {
    let body = '';
    try { body = await res.text(); } catch (e) { body = ''; }
    if (seq !== libPvSeq) return;
    console.error('[library] preview failed:', cur.rel, res.status, body);
    libPvHttpError(res.status, body, cur);
    return;
  }
  let blob = null;
  try {
    blob = await res.blob();
  } catch (e) {
    if (ac && ac.signal.aborted) return;
    if (seq !== libPvSeq) return;
    console.error('[library] preview body failed:', cur.rel, e);
    libPvFail('Preview failed', (e && e.message) ? e.message : String(e));
    return;
  }
  if (seq !== libPvSeq) return;                // never mint a URL for a frame nobody wants
  const img = $('libPvImg');
  if (!img) return;
  const prev = libPvUrl;
  const next = URL.createObjectURL(blob);
  libPvUrl = next;
  // Both handlers answer for THIS source only: the ticket rules out a superseded
  // frame, the src comparison a handler left over from one. A browser that
  // reports the REPLACED load's abort as an error is the case neither can tell
  // apart — hence the revoke ordering below (nothing aborts in the first place),
  // and if one slipped through anyway the real onload that follows clears the
  // note, so the pane cannot end up stuck on a failure that did not happen.
  img.onload = () => {
    if (seq !== libPvSeq || img.src !== next) return;
    img.hidden = false;
    const note = $('libPvNote');
    if (note) note.hidden = true;
  };
  img.onerror = () => {
    if (seq !== libPvSeq || img.src !== next) return;
    console.error('[library] preview image did not decode:', cur.rel);
    libPvFail('Preview failed', 'the rendered image did not decode');
  };
  img.src = next;
  // Revoke the frame this one replaces only AFTER the swap. The <img> may still
  // have been decoding it (the operator arrowed on mid-decode), and pulling the
  // URL out from under a live load would surface as a decode failure for a frame
  // nobody is looking at any more.
  if (prev) URL.revokeObjectURL(prev);
}

// Bound only while the pane is open (added in libPvOpen, removed in libPvClose),
// so nothing on this page loses its arrow keys the rest of the time.
function onLibPvKey(e) {
  if (!libPvState.open) return;
  // A held modifier belongs to the browser (⌘←  is Back, Alt+→ is Forward) —
  // never steal it.
  if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
  if (e.key === 'Escape') { e.preventDefault(); libPvClose(); return; }
  if (e.key === 'ArrowLeft') { e.preventDefault(); libPvStep(-1); return; }
  if (e.key === 'ArrowRight') { e.preventDefault(); libPvStep(1); }
}

function onLibPreviewClick(e) {
  if (e.target === $('libPreview')) { libPvClose(); return; }   // the scrim
  const step = e.target.closest('[data-pv-step]');
  if (step) { libPvStep(Number(step.dataset.pvStep)); return; }
  const act = e.target.closest('[data-pv-act]');
  if (!act) return;
  if (act.dataset.pvAct === 'close') libPvClose();
  else if (act.dataset.pvAct === 'retry') libPvShow(libPvState.idx);
}

// ── library actions: the Send-to picker and the Delete confirm (T10) ────────
// Both footer actions drive ONE modal over `#libDialog`, a body-level host beside
// the preview overlay (same placement, same reason). Five rules shape it:
//
//  * The SELECTION IS SNAPSHOT at open (`libDlg.items`). The 2 s status tick can
//    retreat the browser out of its root (libOnStatus's identity guard), and a
//    dialog still acting on rel-paths whose root index has been re-pointed would
//    address a folder nobody picked — so `libShowRoots` and a tab switch close
//    it, and everything after open reads the snapshot, never the live Set.
//  * Every round trip takes a ticket (`libDlgSeq`, the guard `libLoad` and the
//    preview pane already use). A response that lands after the dialog closed —
//    or after it was reopened for the other action — neither paints nor acts.
//  * ONE in-flight write at a time (`libDlg.busy` disables the submit button and
//    the target boxes): a double-click must not enqueue two batches or delete
//    twice. Closing is NOT blocked though — a write that never answers (a wedged
//    network share) would otherwise leave a full-page scrim with no exit but a
//    reload. Closing over a live write leaves a standing flash saying it is still
//    running, and the response, when it lands, paints nothing (its ticket is
//    stale) but still re-reads the listing, which is where the truth now shows.
//  * Escape closes; **Enter never confirms**. The element focused on open is the
//    safe one (Cancel on the delete confirm), so a stray Return dismisses.
//  * No focus trap — Tab still reaches the page behind the scrim. That matches
//    the page's existing `.tconfirm` dialog and the preview pane; one shared fix
//    later beats three divergent ones now.
//
// The delete dialog is deliberately a TWO-CALL flow against one route: the
// preview (`confirm: false`) is what the operator reads, the confirm
// (`confirm: true`) is what they authorise. Both plan over the same selection,
// so the only difference between what the dialog said and what happened is what
// really changed on disk in between — and that lands as a per-file outcome.
const LIB_DLG_LIST_CAP = 200;   // rows shown per list before "…and N more"

let libDlg = null;              // the open dialog's whole state, or null
let libDlgSeq = 0;              // monotonic; a stale response never paints
let libDlgReturnFocus = null;   // SELECTOR (not node) focus returns to on close
let libDlgOrphanedWrite = false;  // a write outlived its dialog; converge on answer

const libNFiles = (n) => n + (n === 1 ? ' file' : ' files');
// A package ref is an absolute-ish path; its basename is the uuid directory the
// operator actually recognises from the Transfers tab. Full ref goes in `title`.
const libBaseName = (ref) => {
  const s = String(ref ?? '');
  const segs = s.split(/[\\/]+/).filter(Boolean);
  return segs.length ? segs[segs.length - 1] : s;
};

// The library's own status line (the page's `.flash` convention). Outcomes are
// written here as well as into the dialog, so the answer survives the close.
function libSay(msg, ok) {
  const f = $('libFlash');
  if (!f) return;
  f.textContent = msg;
  f.className = 'flash ' + (ok ? 'ok' : 'err');
}

// A configured target reads as its friendly device name when the Settings
// picker's device list can resolve it (id → name, the same lookup `tgLabel`
// does), with the id kept as a `hint` beside it; otherwise the raw entry,
// shortened when it is a bare peer id and with NO hint — repeating the same
// truncated hex twice says nothing. That device list is fetched by the Settings
// section on sign-in — the Library tab never makes a hub call of its own, so an
// unresolvable id is shown honestly rather than waited for.
function libTargetView(entry) {
  const raw = String(entry ?? '');
  const hit = tgOptions.find((d) => d.id === raw) || tgOptions.find((d) => d.name === raw);
  if (hit) return { label: hit.name, hint: hit.name === raw ? '' : raw };
  return { label: /^[0-9a-f]{16,}$/i.test(raw) ? shortHex(raw) + '…' : raw, hint: '' };
}
const libTargetLabel = (entry) => libTargetView(entry).label;

// ── open / close ──
function libDlgOpen(kind) {
  const items = libSelectedItems();
  if (!items.length) return;              // footer is hidden without a selection
  const host = $('libDialog');
  if (!host) return;
  if (libDlg) libDlgClose();              // never stack two (or two key listeners)
  const seq = ++libDlgSeq;
  libDlg = {
    kind, items, seq,
    phase: 'loading', busy: false, error: null,
    targets: [], picked: new Set(), waiting: [], targetsOk: false,   // send
    preview: null, outcomes: null, sent: null,
  };
  // Focus goes back to the FOOTER BUTTON this dialog was opened from — recorded
  // as a selector, not as the node. `libRefresh` re-renders `#libFooter` while
  // the dialog is still open, so by close time the clicked element is detached
  // and `.focus()` on it silently lands on <body>. Re-query at close instead.
  libDlgReturnFocus = `[data-lib-act="${kind}"]`;
  host.hidden = false;
  document.addEventListener('keydown', onLibDlgKey);
  libDlgRender({ focus: true });
  if (kind === 'send') libSendLoad(); else libDeleteLoad();
}

function libDlgClose() {
  if (!libDlg) return;
  // Closing over a live write does not stop it — the request is on the wire and
  // the server will finish it. Say so, standing, in the library's own flash: the
  // dialog was the only place that answer was going to be shown.
  if (libDlg.busy) {
    libDlgOrphanedWrite = true;
    libSay(`${libDlg.kind === 'send' ? 'Send' : 'Delete'} still running — refresh the listing to see the result.`, true);
  }
  libDlgSeq++;                            // orphan every in-flight response
  libDlg = null;
  document.removeEventListener('keydown', onLibDlgKey);
  const host = $('libDialog');
  if (host) { host.hidden = true; host.innerHTML = ''; }
  const sel = libDlgReturnFocus;
  libDlgReturnFocus = null;
  // Re-query rather than restore a held node (see libDlgOpen). No match — the
  // footer hides itself once the selection is empty — means there is genuinely
  // nothing to return to, so focus is left alone rather than forced somewhere.
  if (sel && document.querySelector) {
    const back = document.querySelector(sel);
    if (back && back.focus) back.focus();
  }
}

// The answer to a write whose dialog is gone. It must never paint into a dialog
// that closed (or reopened for the other action) — but the listing is now the
// only surface that can show what really happened, so re-read it.
function libDlgOrphanDrop() {
  if (!libDlgOrphanedWrite) return;
  libDlgOrphanedWrite = false;
  libRefresh();
}

// Escape closes — including mid-write, deliberately: a wedged POST must not trap
// the operator behind a scrim with no exit but F5 (libDlgClose leaves the
// standing flash). Enter is unbound — a destructive confirm must be clicked, or
// tabbed to and pressed, never fired by a stray Return while reading warnings.
function onLibDlgKey(e) {
  if (!libDlg) return;
  if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
  if (e.key !== 'Escape') return;
  e.preventDefault();
  libDlgClose();
}

// ── render ──
// One shell for both dialogs; the body/actions come from the per-kind builders.
// `focus` is passed only on a PHASE change: re-focusing on every render would
// yank focus off the target checkbox the operator just clicked.
function libDlgRender(opts) {
  const host = $('libDialog');
  if (!host || !libDlg) return;
  const d = libDlg;
  const title = d.kind === 'send' ? 'Send to…' : 'Delete from disk';
  const parts = d.kind === 'send' ? libSendDialogParts(d) : libDeleteDialogParts(d);
  host.innerHTML = `
    <div class="tconfirm lib-dlg" role="dialog" aria-modal="true" aria-label="${esc(title)}">
      <h3>${esc(title)}</h3>
      <div class="tconfirm-body lib-dlg-body">${parts.body}</div>
      <div class="tconfirm-actions">${parts.actions}</div>
    </div>`;
  if (opts && opts.focus) {
    const f = $('libDlgFocus');
    if (f && f.focus) f.focus();
  }
}

// The error line shared by both dialogs: the server's own words, escaped, never
// paraphrased — a 400 naming two ambiguous devices is the operator's fix.
const libDlgErrHtml = (msg) => (msg ? `<p class="lib-dlg-err">${esc(msg)}</p>` : '');

// Cancel stays LIVE while a write is in flight (see the section header): it does
// not abort the request — it gives up watching for the answer, which the standing
// flash then points at the listing for. The label says which of the two it is.
const libDlgCancelBtn = (label, d, focusHere) =>
  `<button class="ghost" data-dlg-act="cancel"${focusHere ? ' id="libDlgFocus"' : ''}`
  + `${d.busy ? ' title="the request keeps running — the listing will show the result"' : ''}>`
  + `${esc(d.busy ? 'Close' : label)}</button>`;

// ── send dialog ──
function libSendDialogParts(d) {
  const dirs = d.items.filter((i) => i.dir).length;
  const files = d.items.length - dirs;
  const subject = dirs
    ? `<b>${d.items.length}</b> selected ${d.items.length === 1 ? 'item' : 'items'}`
    : `<b>${files}</b> selected ${files === 1 ? 'file' : 'files'}`;
  const dirNote = dirs
    ? `<p class="muted">${dirs === 1 ? 'One folder is' : dirs + ' folders are'} in the selection — everything inside travels too.</p>`
    : '';

  if (d.phase === 'loading') {
    return {
      body: '<div class="empty">reading this node\'s send targets…</div>',
      actions: libDlgCancelBtn('Cancel', d, true),
    };
  }

  if (d.phase === 'result') {
    const rep = d.sent || {};
    const ref = String(rep.packageRef || '');
    return {
      body: `<p class="lib-dlg-ok">${esc(libSendMessage(rep))}</p>`
        + (ref
          ? `<p class="muted">batch <span class="mono" title="${esc(ref)}">${esc(libBaseName(ref))}</span></p>`
          : '')
        + '<p class="muted">The listing behind this dialog has been re-read — the status chips now show what the engine is doing with them.</p>',
      actions: libDlgCancelBtn('Close', d, false)
        + '<button data-dlg-act="transfers" id="libDlgFocus">View in Transfers</button>',
    };
  }

  // phase 'form' — the picker itself.
  const rows = d.targets.map((t, i) => {
    const raw = String(t);
    const view = libTargetView(raw);
    const idAttr = i === 0 ? ' id="libDlgFocus"' : '';
    return `<label class="inline-label lib-dlg-target">
      <input type="checkbox" data-dlg-target="${esc(raw)}"${idAttr}${d.picked.has(raw) ? ' checked' : ''}${d.busy ? ' disabled' : ''} />
      <span>${esc(view.label)}</span>${view.hint
        ? `<span class="muted mono" title="${esc(view.hint)}">${esc(shortHex(view.hint))}…</span>` : ''}
    </label>`;
  }).join('');
  // Two very different empties. The load SUCCEEDED with nothing configured →
  // the instruction below is the fix. The load FAILED → we know nothing about
  // this node's targets, and printing "add one under Settings" over a network
  // error would be a confident wrong diagnosis, so the error line stands alone.
  const noTargets = d.targetsOk
    ? `<p class="lib-dlg-warn">No device is reachable from this node right now — the sync engine is not running, or it has no send targets. Add one under Settings → Send Targets.</p>`
    : '';
  // The picker's honest premise (see libSendLoad): these rows are the CONFIGURED
  // send targets of the running launcher, not a list of live, proven engines.
  const premise = d.targets.length
    ? '<p class="muted">These are this node\'s configured send targets — one that failed to start will refuse the send and say so.</p>'
    : '';
  const waiting = d.waiting.length
    ? `<p class="muted">Saved but not applied yet: ${d.waiting.map((t) => esc(libTargetLabel(t))).join(', ')} — ${d.waiting.length === 1 ? 'it becomes' : 'they become'} sendable once the sync engine restarts.</p>`
    : '';
  const canSend = d.targets.length > 0 && d.picked.size > 0 && !d.busy;
  const label = dirs ? `Send ${d.items.length} items` : `Send ${libNFiles(files)}`;
  return {
    body: `<p>Send ${subject} to:</p>${dirNote}`
      + (d.targets.length ? `<div class="lib-dlg-targets">${rows}</div>` : noTargets)
      + premise
      + waiting
      + libDlgErrHtml(d.error),
    actions: libDlgCancelBtn('Cancel', d, !d.targets.length)
      + `<button data-dlg-act="send" id="libDlgSubmit"${canSend ? '' : ' disabled'}>`
      + `${esc(d.busy ? 'Sending…' : label)}</button>`,
  };
}

// "4 sent, 1 no longer on disk" — the skipped count is the server's own answer
// for named files that vanished under a stale listing, and saying it is the only
// way the operator can tell why fewer files shipped than were picked.
function libSendMessage(rep) {
  const enqueued = Number(rep.enqueued) || 0;
  const skipped = Number(rep.skipped) || 0;
  return `Sent ${libNFiles(enqueued)}` + (skipped ? ` (${skipped} no longer on disk)` : '');
}

// `runtime` is the LAUNCHER'S CONFIG verbatim — the target list the supervisor
// handed the running engines, not a list of engines that came up. `resolve_targets`
// SKIPS a target it cannot resolve (unknown device, no relay path), so the live
// engines are a subset of `runtime`, sometimes a strict one, and a row here that
// was skipped at launch answers `400 unknown send target: <name>` when picked.
// That is the honest best this page can do without a per-target liveness read:
// the rows are what this node is CONFIGURED to send to (said plainly under the
// picker), the 400 names the one that is not there, and a target saved under
// Settings but not yet applied is in `configured` only — offering it would be a
// guaranteed 400, so it is named as pending instead of listed.
// The one read of this node's send targets, shared by both "send somewhere"
// dialogs (Library §1a and Transfers §6) so they can never disagree about which
// devices are offerable. `ok:false` means the list is UNKNOWN, which is not the
// same as empty — the callers render those two very differently.
async function loadSendTargets() {
  try {
    const dto = await getJson('/api/targets');
    const runtime = Array.isArray(dto.runtime) ? dto.runtime.map(String) : [];
    const configured = Array.isArray(dto.configured) ? dto.configured.map(String) : [];
    // A config can legitimately hold one device twice (by id and by name); two
    // rows toggling the same key would be a lie about what is being sent.
    return {
      ok: true,
      targets: Array.from(new Set(runtime)),
      waiting: configured.filter((t) => !runtime.includes(t)),
      error: null,
    };
  } catch (e) {
    const error = (e && e.message) ? e.message : String(e);
    console.error('[targets] could not read the send targets:', error);
    return { ok: false, targets: [], waiting: [], error };
  }
}

async function libSendLoad() {
  const seq = libDlg.seq;
  const res = await loadSendTargets();
  if (!libDlg || libDlg.seq !== seq) return;
  if (!res.ok) {
    libDlg.error = 'could not read the send targets: ' + res.error;
    libDlg.targets = [];
    libDlg.picked = new Set();
    libDlg.targetsOk = false;   // an unread list is NOT an empty list
  } else {
    libDlg.targetsOk = true;
    libDlg.targets = res.targets;
    libDlg.picked = new Set(res.targets);
    libDlg.waiting = res.waiting;
  }
  libDlg.phase = 'form';
  libDlgRender({ focus: true });
}

async function libSendSubmit() {
  const d = libDlg;
  if (!d || d.kind !== 'send' || d.busy || d.phase !== 'form') return;
  const picked = d.targets.filter((t) => d.picked.has(t));
  if (!picked.length) return;
  // Everything checked → send the batcher's OWN fan-out set (`targets: []`),
  // which is what an auto or manual flush does. It is also the more robust wire:
  // a named target is resolved against the running engines through the hub's
  // device-name map, and the whole-set default cannot fail that resolution.
  const targets = picked.length === d.targets.length ? [] : picked;
  const seq = d.seq;
  d.busy = true;
  d.error = null;
  libDlgRender();
  let res = null;
  let error = null;
  try {
    res = await api('/api/library/send', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ targets, items: d.items }),
    });
  } catch (e) { error = (e && e.message) ? e.message : String(e); }
  // Closed (or reopened) under us: paint nothing, but converge the listing.
  if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
  if (!error && !res.ok) {
    let body = '';
    try { body = await res.text(); } catch (e) { body = ''; }
    if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
    error = String(body || '').trim() || ('HTTP ' + res.status);
  }
  if (error) {
    // Including the 400 that names a configured target the engines never got:
    // `busy` clears, the picker re-renders with the same boxes still editable,
    // and the server's own sentence is what the operator reads.
    console.error('[library] send failed:', error);
    libDlg.busy = false;
    libDlg.error = error;
    libDlgRender();
    libSay('send failed: ' + error, false);
    return;
  }
  let rep = null;
  try { rep = await res.json(); } catch (e) { rep = {}; }
  if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
  libDlg.busy = false;
  libDlg.sent = rep || {};
  libDlg.phase = 'result';
  libDlgRender({ focus: true });
  libSay(libSendMessage(libDlg.sent), true);
  // The selection is deliberately KEPT: re-sending, previewing or deleting the
  // same files next is a normal follow-up, and the operator cleared nothing.
  libRefresh();
}

// ── delete dialog ──
// The preview's rows are grouped, not listed one warning per file: an operator
// deleting a night's folder must read three sentences, not four hundred.
function libDeleteSummary(preview) {
  const rows = Array.isArray(preview) ? preview : [];
  const blocked = rows.filter((r) => r && r.blocked);
  const live = rows.filter((r) => r && !r.blocked);
  const queued = live.filter((r) => r.queued).length;
  const confirmed = live.filter((r) => (Number(r.confirmedBatches) || 0) > 0).length;
  // batch label -> how many of these files it carries. A Map, not an object: the
  // labels are server-controlled and `{}` would let `constructor` collide with
  // Object.prototype.
  const inFlight = new Map();
  live.forEach((r) => (Array.isArray(r.inFlightBatches) ? r.inFlightBatches : [])
    .forEach((b) => inFlight.set(String(b), (inFlight.get(String(b)) || 0) + 1)));
  return { rows, live, blocked, queued, confirmed, inFlight };
}

// A capped `<li>` list — a 5 000-frame folder must not paint 5 000 rows.
function libDlgList(rows, render) {
  const shown = rows.slice(0, LIB_DLG_LIST_CAP).map(render).join('');
  const more = rows.length > LIB_DLG_LIST_CAP
    ? `<li class="muted">…and ${rows.length - LIB_DLG_LIST_CAP} more</li>`
    : '';
  return `<ul class="lib-dlg-list">${shown}${more}</ul>`;
}

function libDeleteWarningsHtml(sum) {
  const out = [];
  if (sum.queued) {
    // T9b: a deleted file that is captured again at the same path IS enqueued
    // again (the watcher-forget seam), so this warning must not read as "gone
    // from the queue forever".
    out.push(`<p class="lib-dlg-warn">${libNFiles(sum.queued)} waiting in the send queue — deleting leaves the queue, unsent.</p>`
      + '<p class="muted">A frame captured again at the same path afterwards is picked up and sent as new.</p>');
  }
  sum.inFlight.forEach((n, label) => {
    out.push(`<p class="lib-dlg-warn">${libNFiles(n)} in transfer `
      + `<span class="mono" title="${esc(label)}">${esc(libBaseName(label))}</span>`
      + ' — that transfer completes from its packaged copy.</p>');
  });
  if (sum.confirmed) {
    out.push(`<p class="lib-dlg-warn">${libNFiles(sum.confirmed)} already delivered in a confirmed batch — `
      + 'the delivered copies stay put; a later re-send of those batches carries fewer files.</p>');
  }
  if (sum.blocked.length) {
    out.push(`<p class="lib-dlg-warn">${libNFiles(sum.blocked.length)} will NOT be deleted:</p>`
      + libDlgList(sum.blocked, (r) =>
        `<li class="mono">${esc(r.rel)} <span class="muted">— ${esc(r.blocked)}</span></li>`));
  }
  return out.join('');
}

function libDeleteDialogParts(d) {
  if (d.phase === 'loading') {
    return {
      body: '<div class="empty">checking what these files are involved in…</div>',
      actions: libDlgCancelBtn('Cancel', d, true),
    };
  }
  if (d.phase === 'error') {
    return {
      body: '<p>Nothing has been deleted.</p>' + libDlgErrHtml(d.error),
      actions: libDlgCancelBtn('Close', d, true) + '<button data-dlg-act="retry">Retry</button>',
    };
  }
  if (d.phase === 'result') {
    const res = libDeleteOutcome(d.outcomes);
    return {
      body: `<p class="${res.clean ? 'lib-dlg-ok' : 'lib-dlg-warn'}">${esc(res.summary)}</p>`
        + (res.problems.length
          ? libDlgList(res.problems, (p) =>
            `<li class="mono">${esc(p.rel)} <span class="muted">— ${esc(p.kind)}${p.reason ? ': ' + esc(p.reason) : ''}</span></li>`)
          : '')
        + (res.problems.length
          ? '<p class="muted">The files that survived are still selected, so you can look at them and try again.</p>'
          : ''),
      actions: libDlgCancelBtn('Close', d, true),
    };
  }

  // phase 'form' — the preview.
  const sum = libDeleteSummary(d.preview);
  const n = sum.live.length;
  const body = `<p>Delete <b>${n}</b> ${n === 1 ? 'file' : 'files'} from disk?</p>`
    + (n
      ? `<details class="lib-dlg-files"><summary>show ${n === 1 ? 'it' : 'them'}</summary>`
        + libDlgList(sum.live, (r) => `<li class="mono">${esc(r.rel)}</li>`)
        + '</details>'
      : '<p class="muted">Nothing in this selection can be deleted.</p>')
    + libDeleteWarningsHtml(sum)
    + libDlgErrHtml(d.error);
  return {
    body,
    actions: libDlgCancelBtn('Cancel', d, true)
      + `<button class="danger" data-dlg-act="delete" id="libDlgSubmit"${(n && !d.busy) ? '' : ' disabled'}>`
      + `${esc(d.busy ? 'Deleting…' : 'Delete ' + libNFiles(n))}</button>`,
  };
}

// The per-path outcomes as one honest line. Anything that is not a `deleted`
// counts as a problem, including a `kind` this build does not know — a file that
// was not confirmed deleted must never be summed into "Deleted N".
function libDeleteOutcome(outcomes) {
  const rows = Array.isArray(outcomes) ? outcomes : [];
  let deleted = 0, refused = 0, failed = 0;
  const problems = [];
  rows.forEach((o) => {
    const kind = (o && o.outcome && o.outcome.kind) ? String(o.outcome.kind) : 'error';
    const reason = (o && o.outcome && o.outcome.reason) ? String(o.outcome.reason) : '';
    if (kind === 'deleted') { deleted++; return; }
    if (kind === 'refused') refused++; else failed++;
    problems.push({ rel: String(o && o.rel ? o.rel : ''), kind, reason });
  });
  const parts = [`Deleted ${deleted}`];
  if (refused) parts.push(`${refused} refused`);
  if (failed) parts.push(`${failed} error${failed === 1 ? '' : 's'}`);
  let summary = parts.join(', ');
  // One problem → name its reason inline ("Deleted 9, 1 error — not found");
  // more than one and the list below carries them.
  if (problems.length === 1 && problems[0].reason) summary += ' — ' + problems[0].reason;
  return { deleted, refused, failed, problems, clean: problems.length === 0, summary };
}

// Drop only what is really gone. A file stays picked when its own outcome was
// not `deleted`; a FOLDER stays picked while anything under it survived, so the
// operator can retry exactly the failures. A rel nobody reported on is left
// alone — an unreported pick is not evidence of anything.
function libPruneSelection(outcomes) {
  const rows = (Array.isArray(outcomes) ? outcomes : []).filter((o) => o && o.root === libState.root);
  const deleted = new Set();
  const survived = new Set();
  rows.forEach((o) => {
    const kind = (o.outcome && o.outcome.kind) ? String(o.outcome.kind) : 'error';
    (kind === 'deleted' ? deleted : survived).add(String(o.rel));
  });
  const under = (set, rel) => {
    for (const r of set) if (r === rel || r.startsWith(rel + '/')) return true;
    return false;
  };
  Array.from(libState.selected).forEach((rel) => {
    const isDir = libState.selectedDirs.has(rel);
    const gone = isDir
      ? (!under(survived, rel) && under(deleted, rel))
      : deleted.has(rel);
    if (gone) {
      libState.selected.delete(rel);
      libState.selectedDirs.delete(rel);
    }
  });
}

async function libDeletePost(confirm) {
  return api('/api/library/delete', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ items: libDlg.items, confirm }),
  });
}

// Step one: `confirm: false` touches nothing and answers with one row per file.
async function libDeleteLoad() {
  const seq = libDlg.seq;
  let res = null;
  let error = null;
  try { res = await libDeletePost(false); }
  catch (e) { error = (e && e.message) ? e.message : String(e); }
  if (!libDlg || libDlg.seq !== seq) return;
  if (!error && !res.ok) {
    let body = '';
    try { body = await res.text(); } catch (e) { body = ''; }
    if (!libDlg || libDlg.seq !== seq) return;
    error = String(body || '').trim() || ('HTTP ' + res.status);
  }
  if (error) {
    console.error('[library] delete preview failed:', error);
    libDlg.error = error;
    libDlg.phase = 'error';
    libDlgRender({ focus: true });
    return;
  }
  let rep = null;
  try { rep = await res.json(); } catch (e) { rep = {}; }
  if (!libDlg || libDlg.seq !== seq) return;
  libDlg.preview = Array.isArray(rep.preview) ? rep.preview : [];
  libDlg.phase = 'form';
  libDlgRender({ focus: true });
}

// Step two: `confirm: true` performs, and the per-path outcomes are the record.
async function libDeleteSubmit() {
  const d = libDlg;
  if (!d || d.kind !== 'delete' || d.busy || d.phase !== 'form') return;
  if (!libDeleteSummary(d.preview).live.length) return;
  const seq = d.seq;
  d.busy = true;
  d.error = null;
  libDlgRender();
  let res = null;
  let error = null;
  try { res = await libDeletePost(true); }
  catch (e) { error = (e && e.message) ? e.message : String(e); }
  // Closed under us: the per-file outcomes are lost to the operator, so the
  // listing re-read is the only thing that can still tell them what happened.
  if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
  if (!error && !res.ok) {
    let body = '';
    try { body = await res.text(); } catch (e) { body = ''; }
    if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
    error = String(body || '').trim() || ('HTTP ' + res.status);
  }
  if (error) {
    console.error('[library] delete failed:', error);
    libDlg.busy = false;
    libDlg.error = error;
    libDlgRender();
    libSay('delete failed: ' + error, false);
    return;
  }
  let rep = null;
  try { rep = await res.json(); } catch (e) { rep = {}; }
  if (!libDlg || libDlg.seq !== seq) { libDlgOrphanDrop(); return; }
  libDlg.busy = false;
  libDlg.outcomes = Array.isArray(rep.outcomes) ? rep.outcomes : [];
  libDlg.phase = 'result';
  const res2 = libDeleteOutcome(libDlg.outcomes);
  libPruneSelection(libDlg.outcomes);
  libDlgRender({ focus: true });
  libSay(res2.summary, res2.clean);
  libRefresh();     // the listing is now lying about files that are gone
}

// ── dialog event handlers (bound once to the stable host in wireLibrary) ──
function onLibDialogClick(e) {
  if (!libDlg) return;
  if (e.target === $('libDialog')) { libDlgClose(); return; }   // scrim — live mid-write
  const act = e.target.closest('[data-dlg-act]');
  if (!act) return;
  switch (act.dataset.dlgAct) {
    case 'cancel': libDlgClose(); break;
    case 'send': libSendSubmit(); break;
    case 'delete': libDeleteSubmit(); break;
    case 'retry':
      libDlg.error = null;
      libDlg.phase = 'loading';
      libDlgRender({ focus: true });
      if (libDlg.kind === 'send') libSendLoad(); else libDeleteLoad();
      break;
    case 'transfers': libDlgClose(); setTab('transfers'); break;
  }
}

// The picker's checkboxes patch the Set + the submit button in place. A full
// re-render here would move focus and collapse the list's scroll position.
function onLibDialogChange(e) {
  if (!libDlg || libDlg.kind !== 'send') return;
  const box = e.target.closest('[data-dlg-target]');
  if (!box) return;
  const key = String(box.dataset.dlgTarget);
  if (box.checked) libDlg.picked.add(key); else libDlg.picked.delete(key);
  const btn = $('libDlgSubmit');
  if (btn) btn.disabled = libDlg.picked.size === 0 || libDlg.busy;
}

// ── delegated handlers (bound once to the stable panel, never per render) ──
function onLibraryClick(e) {
  const rootBtn = e.target.closest('[data-lib-root]');
  if (rootBtn) { libLoad(Number(rootBtn.dataset.libRoot), ''); return; }
  const nav = e.target.closest('[data-lib-nav]');
  if (nav) {
    if (libState.root === null) return;   // a rel is meaningless without its root
    libLoad(libState.root, nav.dataset.libNav || '');
    return;
  }
  // A file's NAME opens the preview; its checkbox is a separate element, so
  // picking a file never opens the pane and vice versa.
  const pv = e.target.closest('[data-lib-pv]');
  if (pv) { libPvOpen(pv.dataset.libPv); return; }
  const act = e.target.closest('[data-lib-act]');
  if (!act) return;
  switch (act.dataset.libAct) {
    case 'roots': libShowRoots(); break;
    case 'refresh': case 'retry': libRefresh(); break;
    case 'clear': libClearSelection(); libRender(); break;
    case 'preview': { const f = libPvFirstSelectedFile(); if (f) libPvOpen(f.rel); break; }
    case 'send': libDlgOpen('send'); break;
    case 'delete': libDlgOpen('delete'); break;
  }
}

function onLibraryChange(e) {
  const box = e.target.closest('[data-lib-pick]');
  if (!box) return;
  libTogglePick(box.dataset.libPick, box.dataset.libDir === '1', box.checked);
  const row = box.closest('tr');
  if (row) row.classList.toggle('lib-picked', box.checked);
}

function wireLibrary() {
  const panel = $('tab-library');
  panel.addEventListener('click', onLibraryClick);
  panel.addEventListener('change', onLibraryChange);
  // The preview overlay and the action dialogs live outside the panel
  // (index.html), so they get their own delegated handlers — bound once to the
  // stable hosts, never per open.
  $('libPreview').addEventListener('click', onLibPreviewClick);
  $('libDialog').addEventListener('click', onLibDialogClick);
  $('libDialog').addEventListener('change', onLibDialogChange);
}

// ── boot ────────────────────────────────────────────────────────────────────
// The shared helpers + the live refreshTransfers renderer are exposed on
// PerseusApp so the poll tick and the To-Sync manual flush can drive the list.
window.PerseusApp = {
  api, getJson, $, esc, shortHex, fmtSize, fmtDur, fmtCountdown, fmtMtime,
  setTab,
  refreshTransfers,
  // Library tab surface for the later send / delete tasks: the live
  // state object, the loader, and the selection as [{root, rel, dir}].
  libState, libLoad, libRefresh, libSelectedItems,
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
  renderLibraryTab();
  renderSettingsTab();
  mountBanner();
  // wireTabs() restores the saved tab, so every panel's markup must exist first.
  wireTabs();
  wireTosync();
  wireTransfers();
  wireLibrary();
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
