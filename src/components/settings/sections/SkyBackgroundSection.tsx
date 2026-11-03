// General tab, "Sky background" (issue #18): the online switch for the Sky
// Chart's HiPS background, the browsing cache limit (KV
// `skychart.hips_cache_limit_gb`, enforced by `hips::cache` in the core), the
// space all surveys take with Clear all, and per survey its credit, license,
// servers, the space its stored tiles take, and Clear. Clear is an action, not
// a setting.
import { useCallback, useEffect, useRef, useState } from 'react';
import { Trash2 } from 'lucide-react';
import { api } from '../../../api';
import { openUrl } from '../../../api/desktop';
import type { HipsSurveyInfo } from '../../../types/models';
import { useHipsPack } from '../../../hooks/useHipsPack';
import { useNotifications } from '../../../contexts/NotificationContext';
import { fieldMeta } from '../../../settings/registry';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Checkbox } from '../Checkbox';
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';

function formatBytes(n: number): string {
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Pixel size of one 512 px tile pixel per HiPS order.
const ORDER_PIXEL_SIZE = ['412″', '206″', '103″', '51″', '26″', '13″'];
const ORDERS = [0, 1, 2, 3, 4, 5];

function PackControls({ survey, onFinished, refreshToken }: {
  survey: HipsSurveyInfo;
  onFinished: () => void;
  /** Changes when the survey's store was cleared: the status and the estimate are read again. */
  refreshToken: number;
}) {
  const { selection, setSelection, estimate, progress, start, cancel, refresh } = useHipsPack(survey.id, onFinished);
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  useEffect(() => {
    if (refreshToken > 0) void refreshRef.current();
  }, [refreshToken]);
  const meta = fieldMeta('general.skyBackground', 'pack');
  const state = progress?.state ?? 'idle';
  const active = state === 'running' || state === 'cancelling';
  const resumable = state === 'interrupted' || state === 'cancelled' || state === 'failed';
  const nothingToDo = estimate !== null && estimate.missing === 0;
  const blocked = estimate === null || !estimate.fits || nothingToDo;
  const done = progress ? progress.present + progress.fetched + progress.absent + progress.failed : 0;
  const pct = progress && progress.total > 0 ? Math.min(100, (done / progress.total) * 100) : 0;
  const saved = progress?.selection;
  const sameSelection = saved != null
    && saved.surveyId === selection.surveyId
    && saved.skyOrder === selection.skyOrder
    && saved.aroundLocations === selection.aroundLocations
    && saved.aroundMessier === selection.aroundMessier
    && saved.aroundNgcIc === selection.aroundNgcIc;
  const showProgress = progress !== null && state !== 'idle' && progress.total > 0;

  const checkbox = (key: 'aroundLocations' | 'aroundMessier' | 'aroundNgcIc', label: string) => (
    <Checkbox
      size="sm"
      checked={selection[key]}
      disabled={active}
      onChange={(checked) => setSelection({ ...selection, [key]: checked })}
      label={label}
    />
  );

  return (
    <div className="mt-2 border-t border-border pt-2 space-y-2">
      <div>
        <p className="text-xs text-content">{meta.label}</p>
        <p className="text-xs text-content-muted">{meta.help}</p>
      </div>
      <label className="flex items-center gap-2 text-xs text-content-secondary">
        Whole sky detail
        <select
          value={selection.skyOrder}
          disabled={active}
          onChange={(e) => setSelection({ ...selection, skyOrder: Number(e.target.value) })}
          className="rounded border border-border bg-surface px-1.5 py-0.5 text-xs text-content"
        >
          {ORDERS.map((o) => (
            <option key={o} value={o}>{`${o} — ${ORDER_PIXEL_SIZE[o]}/px`}</option>
          ))}
        </select>
      </label>
      <div className="space-y-1">
        {checkbox('aroundLocations', 'Detail around my imaging locations')}
        {checkbox('aroundMessier', 'Detail around Messier objects')}
        {checkbox('aroundNgcIc', 'Detail around NGC/IC brighter than 10ᵐ')}
      </div>
      {estimate && (
        <p className={`text-xs ${estimate.fits || nothingToDo ? 'text-content-muted' : 'text-error'}`}>
          {nothingToDo
            ? 'Everything selected is already on this computer.'
            : `${estimate.missing.toLocaleString()} tiles to download, about ${formatBytes(estimate.estimatedBytes)}${
                estimate.freeBytes === null ? '' : `; ${formatBytes(estimate.freeBytes)} free`
              }${estimate.fits ? '' : ' — not enough space (512 MB is kept free)'}`}
        </p>
      )}
      {showProgress && (
        <div className="space-y-1">
          <div className="h-1.5 w-full rounded bg-surface-hover overflow-hidden">
            <div className="h-full bg-accent" style={{ width: `${pct}%` }} />
          </div>
          <p className="text-xs text-content-muted">
            {state === 'cancelling' && 'Cancelling… '}
            {progress.fetched.toLocaleString()} downloaded · {progress.failed.toLocaleString()} failed
            {progress.waitingS != null && ` · Survey busy, waiting ${progress.waitingS} s`}
          </p>
        </div>
      )}
      {state === 'failed' && progress?.error && <p className="text-xs text-error">{progress.error}</p>}
      <div>
        {active ? (
          <button
            onClick={() => void cancel()}
            disabled={state === 'cancelling'}
            className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-xs bg-surface-hover text-content-secondary hover:text-error disabled:opacity-50 disabled:cursor-not-allowed"
          >
            Cancel
          </button>
        ) : (
          <button
            onClick={() => void start()}
            disabled={blocked}
            className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-xs bg-surface-hover text-content-secondary hover:text-content disabled:opacity-50 disabled:cursor-not-allowed"
          >
            {resumable && sameSelection ? 'Resume' : 'Download'}
          </button>
        )}
      </div>
    </div>
  );
}

export function SkyBackgroundSection() {
  const { notify } = useNotifications();
  const [surveys, setSurveys] = useState<HipsSurveyInfo[] | null>(null);
  const [clearing, setClearing] = useState<HipsSurveyInfo | null>(null);
  const [clearingAll, setClearingAll] = useState(false);
  /** The survey last cleared (`'*'`: all of them), so its pack row reads its status again. */
  const [cleared, setCleared] = useState<{ id: string; n: number } | null>(null);

  const load = useCallback(() => {
    api.invoke<HipsSurveyInfo[]>('get_hips_surveys')
      .then((list) => setSurveys(Array.isArray(list) ? list : []))
      .catch((err) => {
        console.error('[Settings] get_hips_surveys failed:', err);
        setSurveys([]);
      });
  }, []);

  useEffect(load, [load]);

  const clear = async (s: HipsSurveyInfo) => {
    setClearing(null);
    try {
      await api.invoke<number>('clear_hips_cache', { surveyId: s.id });
    } catch (err) {
      console.error('[Settings] clear_hips_cache failed:', err);
      notify({ kind: 'generic', tone: 'warning', title: 'Sky tiles not cleared', detail: `${s.label}: ${err}` });
    }
    load();
    setCleared((c) => ({ id: s.id, n: (c?.n ?? 0) + 1 }));
  };

  const clearAll = async () => {
    setClearingAll(false);
    const failed: string[] = [];
    for (const s of surveys ?? []) {
      if (s.cachedTiles === 0) continue;
      try {
        await api.invoke<number>('clear_hips_cache', { surveyId: s.id });
      } catch (err) {
        console.error(`[Settings] clear_hips_cache ${s.id} failed:`, err);
        failed.push(`${s.label}: ${err}`);
      }
    }
    if (failed.length) {
      notify({ kind: 'generic', tone: 'warning', title: 'Some sky tiles not cleared', detail: failed.join('\n') });
    }
    load();
    setCleared((c) => ({ id: '*', n: (c?.n ?? 0) + 1 }));
  };

  const meta = fieldMeta('general.skyBackground', 'surveys');
  const totalMeta = fieldMeta('general.skyBackground', 'cacheTotal');
  const totalBytes = (surveys ?? []).reduce((a, s) => a + s.cachedBytes, 0);
  const totalTiles = (surveys ?? []).reduce((a, s) => a + s.cachedTiles, 0);
  const packBytes = (surveys ?? []).reduce((a, s) => a + s.packBytes, 0);

  return (
    <SettingsSection id="general.skyBackground">
      <div className="space-y-4">
        <SettingToggle section="general.skyBackground" field="online" settingKey="skychart.hips_online" />
        <SettingNumber
          section="general.skyBackground"
          field="cacheLimitGb"
          settingKey="skychart.hips_cache_limit_gb"
          codec={intCodec(1, 4096)}
          unit="GB"
        />

        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <p className="text-sm text-content">{totalMeta.label}</p>
            <p className="text-xs text-content-muted">{totalMeta.help}</p>
            <p className="mt-0.5 text-xs text-content-secondary" data-testid="hips-cache-total">
              {surveys === null
                ? 'Loading…'
                : totalTiles === 0
                  ? 'No tiles stored.'
                  : `${formatBytes(totalBytes)} in ${totalTiles.toLocaleString()} tiles`
                    + (packBytes > 0 ? `, of which ${formatBytes(packBytes)} from offline packs.` : '.')}
            </p>
          </div>
          <button
            onClick={() => setClearingAll(true)}
            disabled={surveys === null || totalTiles === 0}
            className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-xs bg-surface-hover text-content-secondary hover:text-error disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Trash2 className="w-3.5 h-3.5" />
            Clear all
          </button>
        </div>

        <div>
          <p className="text-sm text-content">{meta.label}</p>
          <p className="text-xs text-content-muted mb-2">{meta.help}</p>
          {surveys === null ? (
            <p className="text-xs text-content-muted">Loading…</p>
          ) : (
            <ul className="space-y-2">
              {surveys.map((s) => (
                <li key={s.id} className="rounded border border-border px-3 py-2">
                  <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <p className="text-sm text-content">{s.label}</p>
                    <p className="text-xs text-content-muted">
                      {s.attributionUrl ? (
                        <button
                          onClick={() => void openUrl(s.attributionUrl!).catch((err) => console.error('[Settings] opening the survey credit link failed:', err))}
                          className="text-accent hover:underline text-left"
                        >
                          {s.attribution}
                        </button>
                      ) : s.attribution}
                      {' · '}
                      {s.license}
                    </p>
                    {s.servers.length > 0 && (
                      <p className="text-xs text-content-muted">
                        {s.servers.length === 1 ? 'Server: ' : 'Servers: '}{s.servers.join(', ')}
                      </p>
                    )}
                    <p className="text-xs text-content-secondary mt-0.5">
                      {s.cachedTiles === 0
                        ? 'No tiles stored.'
                        : `${s.cachedTiles.toLocaleString()} tiles stored, ${formatBytes(s.cachedBytes)}`
                          + (s.packBytes > 0 ? ` (${formatBytes(s.packBytes)} from the offline pack, kept).` : '.')}
                    </p>
                  </div>
                  <button
                    onClick={() => setClearing(s)}
                    disabled={s.cachedTiles === 0}
                    className="shrink-0 inline-flex items-center gap-1.5 px-2.5 py-1 rounded text-xs bg-surface-hover text-content-secondary hover:text-error disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    <Trash2 className="w-3.5 h-3.5" />
                    Clear
                  </button>
                  </div>
                  <PackControls survey={s} onFinished={load} refreshToken={cleared && (cleared.id === s.id || cleared.id === '*') ? cleared.n : 0} />
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>

      <ConfirmDialog
        isOpen={clearingAll}
        title="Clear the tiles of every survey?"
        message="Deletes every sky tile stored on this computer, offline packs included. The Sky Chart fetches them again as you browse while fetching is on."
        confirmText="Clear all"
        confirmDanger
        onConfirm={() => void clearAll()}
        onCancel={() => setClearingAll(false)}
      />
      <ConfirmDialog
        isOpen={clearing !== null}
        title={`Clear ${clearing?.label ?? ''} tiles?`}
        message="Deletes the survey tiles stored on this computer. The Sky Chart fetches them again as you browse while fetching is on."
        confirmText="Clear"
        confirmDanger
        onConfirm={() => clearing && clear(clearing)}
        onCancel={() => setClearing(null)}
      />
    </SettingsSection>
  );
}
