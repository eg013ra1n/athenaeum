// General tab, "Sky background" (issue #18): the online switch for the Sky
// Chart's HiPS background, and per survey its credit, license, the space its
// stored tiles take, and Clear. Clear is an action, not a setting.
import { useCallback, useEffect, useState } from 'react';
import { Trash2 } from 'lucide-react';
import { api } from '../../../api';
import type { HipsSurveyInfo } from '../../../types/models';
import { useHipsPack } from '../../../hooks/useHipsPack';
import { useNotifications } from '../../../contexts/NotificationContext';
import { fieldMeta } from '../../../settings/registry';
import { ConfirmDialog } from '../../ConfirmDialog';
import { Checkbox } from '../Checkbox';
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';

function formatBytes(n: number): string {
  if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// Pixel size of one 512 px tile pixel per HiPS order.
const ORDER_PIXEL_SIZE = ['412″', '206″', '103″', '51″', '26″', '13″'];
const ORDERS = [0, 1, 2, 3, 4, 5];

function PackControls({ survey, onFinished }: { survey: HipsSurveyInfo; onFinished: () => void }) {
  const { selection, setSelection, estimate, progress, start, cancel } = useHipsPack(survey.id, onFinished);
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
  };

  const meta = fieldMeta('general.skyBackground', 'surveys');

  return (
    <SettingsSection id="general.skyBackground">
      <div className="space-y-4">
        <SettingToggle section="general.skyBackground" field="online" settingKey="skychart.hips_online" />

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
                      {s.attribution}
                      {s.attributionUrl && (
                        <>
                          {' · '}
                          <a href={s.attributionUrl} target="_blank" rel="noreferrer" className="text-accent hover:underline">
                            terms
                          </a>
                        </>
                      )}
                      {' · '}
                      {s.license}
                    </p>
                    <p className="text-xs text-content-secondary mt-0.5">
                      {s.cachedTiles === 0
                        ? 'No tiles stored.'
                        : `${s.cachedTiles.toLocaleString()} tiles stored, ${formatBytes(s.cachedBytes)}.`}
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
                  <PackControls survey={s} onFinished={load} />
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>

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
