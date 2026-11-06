import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import { Play, Square, ChevronDown, ChevronRight, FolderOpen, AlertTriangle, Loader2, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useStackingContext } from '../../contexts/StackingContext';
import { useNotifications } from '../../contexts/NotificationContext';
import { subjectKey, type StackSubject, type SubjectKey } from './subject';
import { useStackingSubjectApi, type StackingSubjectApi, type SubjectConfig } from './useStackingSubjectApi';
import type {
  NamedPreset,
  Stage,
  StackingConfig,
  StackingPlan,
  StackingPreset,
  StackingPresets,
  StackingRunDetail,
} from '../../types/stacking';
import { PipelineBoard } from './PipelineBoard';
import { GroupsTable } from './GroupsTable';
import { StageInspector } from './StageInspector';
import { FramesTable, type LightFrameRef } from './FramesTable';
import { ZipExtractionLine } from '../zip/ZipExtractionLine';
import { ResultsPanel } from './ResultsPanel';
import { boardMode, stableStringify, stageLabel, withoutPaths, type BoardStage } from './stageSummary';
import {
  readSelectedStage,
  writeSelectedStage,
  readFramesCollapsed,
  writeFramesCollapsed,
  readInspectorCollapsed,
  writeInspectorCollapsed,
} from './stackingPrefs';

export interface StackingTabProps {
  /** What the tab stacks: a frame set (carrying the set's LIGHT frames) or a
   *  collaboration project. Every command, the run state and the pending
   *  config write are keyed by `subjectKey(subject)`. */
  subject: StackSubject;
}

/** What a write still waiting for its debounce (or its flush) is about: the
 *  adapter and key of the subject it was made for, captured when it was
 *  scheduled — never looked up again at send time, so it cannot land on the
 *  subject the tab shows by then. */
interface PendingWrite {
  api: StackingSubjectApi;
  kind: StackSubject['kind'];
  key: SubjectKey;
  value: SubjectConfig;
}

// Stable empty lists for a project subject (no light-frame list, no numeric
// exclusions): a fresh `[]` per render would re-run every memo and effect
// keyed on them.
const NO_LIGHT_FRAMES: LightFrameRef[] = [];
const NO_FRAME_IDS: number[] = [];

const PRESET_LABEL: Record<StackingPreset, string> = {
  default: 'Default',
  fastPreview: 'Fast preview',
  maximumQuality: 'Maximum quality',
};

/** Mirrors `api::stacking::PRESET_NAME_MAX` (M4d Task 4, ruling R-M4d-6) —
 *  the backend refuses anything longer, so the field simply cannot produce
 *  one. The bound is enforced on BOTH sides on purpose: a `maxLength` is a
 *  courtesy, the server's check is the rule. */
const PRESET_NAME_MAX = 60;

function formatGB(bytes: number): string {
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

/**
 * The Stacking tab (spec §11) — plans, configures, runs and watches an M1
 * stacking run for one subject: a frame set or a collaboration project. Owns
 * the plan fetch, the (unsaved) config draft used to preview the plan, the
 * run state from `useStackingContext`, and the board/inspector layout. Task 3
 * fills the inspector; Task 4 adds the Frames table and Results panel. The
 * root carries no `title` (fix round 1 item 6): on a page-sized `div` it pops
 * a native tooltip over the ENTIRE tab on any hover, not just the header.
 */
export function StackingTab({ subject }: StackingTabProps) {
  const navigate = useNavigate();
  const { notify } = useNotifications();
  const { progress, lastOutcome, startRun, cancelRun, isRunning } = useStackingContext();
  // Every stacking command of this subject with its id bound; stable per key.
  const subjectApi = useStackingSubjectApi(subject);
  const stackKey = subjectKey(subject);
  // The set's LIGHT frames (Task 4, Decisions item 3) — `FrameSetDetail.tsx`
  // derives them from its own `detail.nights` tree, the same source every
  // other tab on this page reads. Used by the Frames table both before any
  // run exists (the only frame list available) and after one, as the
  // filename fallback for a row a run's summary hasn't reached yet. A
  // project has none: its table is keyed by frame uuid, not by these ids.
  const lightFrames = subject.kind === 'frameSet' ? subject.lightFrames : NO_LIGHT_FRAMES;

  const [draftConfig, setDraftConfig] = useState<StackingConfig | null>(null);
  // The subject's exclusion keys (`SubjectConfig.excluded`): `String(frameId)`
  // for a set, the frame uuid for a project.
  const [excluded, setExcluded] = useState<string[]>([]);
  // A project's manually pinned reference frame, loaded with its config and
  // sent back with every write (the command stores whatever it is given, so a
  // write that left it out would clear the pin). Always `null` for a set.
  const [referenceFrameUuid, setReferenceFrameUuid] = useState<string | null>(null);
  const [presets, setPresets] = useState<StackingPresets | null>(null);
  // M4d Task 4 (ruling R-M4d-6): the user's OWN presets, a second list
  // beside the three built-ins above. Kept as component state rather than
  // context — the list is one settings row, only this menu reads it, and a
  // save/delete already returns the full list so it never needs a refetch.
  const [userPresets, setUserPresets] = useState<NamedPreset[]>([]);
  const [savePresetOpen, setSavePresetOpen] = useState(false);
  const [presetNameDraft, setPresetNameDraft] = useState('');
  const [presetBusy, setPresetBusy] = useState(false);
  /** The user preset whose inline "Delete '<name>'?" confirm is showing —
   *  the brief's no-browser-dialog rule (no `confirm()`), so the row itself
   *  turns into the question. */
  const [presetPendingDelete, setPresetPendingDelete] = useState<string | null>(null);
  const [plan, setPlan] = useState<StackingPlan | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [selectedStage, setSelectedStage] = useState<BoardStage>(readSelectedStage);
  const [starting, setStarting] = useState(false);
  const [rerunMenuOpen, setRerunMenuOpen] = useState(false);
  const [presetMenuOpen, setPresetMenuOpen] = useState(false);
  const [framesCollapsed, setFramesCollapsed] = useState<boolean>(readFramesCollapsed);
  // Task 5, Decisions item 3: below the `lg` breakpoint the inspector moves
  // under the board as its own collapsible disclosure (the board/inspector
  // side-by-side split no longer has the width for both) — same
  // collapsed/expanded convention as the Frames table above, persisted the
  // same way.
  const [inspectorCollapsed, setInspectorCollapsed] = useState<boolean>(readInspectorCollapsed);
  // Lifted up from `ResultsPanel` (a sibling of `FramesTable`, not its
  // parent) so the Frames table can join its rows against the Results
  // panel's currently-selected run without either component reaching into
  // the other directly.
  const [selectedRunDetail, setSelectedRunDetail] = useState<StackingRunDetail | null>(null);

  const runProgress = progress.get(stackKey);
  const runOutcome = lastOutcome.get(stackKey);
  const running = isRunning(stackKey) || plan?.activeRunId != null;

  // The board's per-row cancelled/failed derivation (A5) needs the SAME
  // run's finished-stage list `outcome` describes — `selectedRunDetail`
  // (Results panel's own fetch, shared here) only qualifies when its run id
  // actually matches; a user browsing an OLDER run there must never leak
  // into the board's read of the LATEST run's outcome.
  const boardFinishedStages: Stage[] | null =
    runOutcome && selectedRunDetail?.run.id === runOutcome.runId
      ? (selectedRunDetail.summary?.stages.map((s) => s.stage) ?? [])
      : null;

  // `refetchPlan` is called from four overlapping triggers (the mount
  // effect below calls the endpoint directly, not through here; but the
  // `library-updated` listener, the 300 ms debounce, and the outcome effect
  // all go through this one function) — an older response can resolve
  // after a newer one already landed, and after navigating to a different
  // subject (the tab may be reused for it, no `key` is promised) a stale
  // response for the PREVIOUS subject can still be in flight. `planSeqRef`
  // drops a response that is no longer the latest call; `subjectKeyRef`
  // (always current, unlike the `stackKey` this closure captured at
  // creation) drops one whose subject has since changed (fix round 1,
  // Important #2).
  const planSeqRef = useRef(0);
  const subjectKeyRef = useRef(stackKey);
  useEffect(() => { subjectKeyRef.current = stackKey; }, [stackKey]);

  // Fix round 1 (Critical #1/#2, Important #3): which subject `draftConfig`
  // belongs to, and whether its most recent change was USER-originated
  // (an inspector edit / preset apply / toggle) as opposed to the load
  // effect's own seed/reset. `dirtyRef` is set ONLY by `setUserConfig`
  // below — never by the load effect — and cleared only once a write for
  // it is actually sent (by the persist effect's timer, or by the flush
  // effect on a set switch/unmount), not merely scheduled, so a pending
  // edit survives long enough to be flushed instead of silently lost.
  const dirtyRef = useRef(false);
  const draftForKeyRef = useRef<SubjectKey | null>(null);

  // Fix round 1, Minor #4: whether the currently-pending write includes an
  // EXCLUSION change — set only by `setUserExcludedFrameIds`, cleared only
  // once the write carrying it is actually dispatched (same "clear at send
  // time, not schedule time" discipline as `dirtyRef` itself). `build_plan`
  // reads `excluded_frame_ids` from the STORED row, never a client override
  // (`crates/athenaeum-core/src/stacking/plan.rs`), so only a write that
  // changed the excluded-id list needs the post-write `refetchPlan` below —
  // a pure config edit is already re-planned by the 300 ms draft-change
  // effect using a client override, and re-planning it again after the
  // persist lands would just be a redundant round trip.
  const exclusionDirtyRef = useRef(false);

  /** The only way `draftConfig` should change as a result of a USER action.
   *  The load effect calls `setDraftConfig` directly (bypassing this) so it
   *  can never mark the draft dirty — visiting a subject with no stored
   *  override must never materialize one (Critical #1). */
  const setUserConfig = useCallback<typeof setDraftConfig>((value) => {
    dirtyRef.current = true;
    setDraftConfig(value);
  }, []);

  /** The exclusion-list analogue of `setUserConfig` above — Ruling 7's ONE
   *  frame-level write. The load effect calls `setExcluded` directly
   *  (bypassing this), so opening a subject never marks the draft dirty on
   *  its own; only the Frames table's include checkbox, through
   *  `handleToggleExcludeFrame` below, goes through here. */
  const setUserExcluded = useCallback<typeof setExcluded>((value) => {
    dirtyRef.current = true;
    exclusionDirtyRef.current = true;
    setExcluded(value);
  }, []);

  // The Frames table still speaks a set's numeric frame ids; a project's
  // table is keyed by frame uuid, so it toggles nothing here (its checkbox is
  // disabled below as well).
  const handleToggleExcludeFrame = useCallback((frameId: number) => {
    if (subject.kind !== 'frameSet') return;
    const key = String(frameId);
    setUserExcluded((prev) => (prev.includes(key) ? prev.filter((k) => k !== key) : [...prev, key]));
  }, [subject.kind, setUserExcluded]);
  const excludedFrameIds = useMemo(
    () => (subject.kind === 'frameSet' ? excluded.map(Number) : NO_FRAME_IDS),
    [excluded, subject.kind],
  );

  const handleToggleFramesCollapsed = useCallback(() => {
    setFramesCollapsed((prev) => {
      const next = !prev;
      writeFramesCollapsed(next);
      return next;
    });
  }, []);

  const handleToggleInspectorCollapsed = useCallback(() => {
    setInspectorCollapsed((prev) => {
      const next = !prev;
      writeInspectorCollapsed(next);
      return next;
    });
  }, []);

  const refetchPlan = useCallback(async (configOverride?: StackingConfig) => {
    const seq = ++planSeqRef.current;
    const forKey = stackKey;
    try {
      const p = await subjectApi.getPlan(configOverride);
      if (seq !== planSeqRef.current || forKey !== subjectKeyRef.current) {
        return null; // superseded by a later call, or the subject changed
      }
      setPlan(p);
      return p;
    } catch (err) {
      console.error('[StackingTab] failed to refresh the stacking plan:', err);
      return null;
    }
  }, [subjectApi, stackKey]);

  // Mount / subject-change: load the subject's config, then the plan built
  // from it. Fix round 1, Critical #2: `draftConfig`/`excluded` are
  // reset to null/[] FIRST (synchronously, before the async fetch even
  // starts) so nothing on screen — and nothing the persist effect could act
  // on — still points at the PREVIOUS subject while this fetch is in flight;
  // `draftForKeyRef` is set only once the config response actually lands,
  // guarded by this effect's own `cancelled` flag (a fresh effect run for a
  // newer subject already flips the older run's `cancelled` to `true`
  // before its `await` can resume — the standard React guard, sufficient
  // here since only this one effect ever loads the stored config).
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    setDraftConfig(null);
    setExcluded([]);
    setReferenceFrameUuid(null);
    (async () => {
      try {
        const cfg = await subjectApi.getConfig();
        if (cancelled) return;
        setDraftConfig(cfg.config);
        setExcluded(cfg.excluded);
        setReferenceFrameUuid(cfg.referenceFrameUuid);
        draftForKeyRef.current = stackKey;
        const presetsResult = await api.invoke<StackingPresets>('get_stacking_presets', {});
        if (cancelled) return;
        setPresets(presetsResult);
        // The user's own presets are a separate, catalog-backed list. A
        // failure here must NOT take the whole tab down with it (the
        // built-ins and the plan below are what the tab actually needs to
        // work), so it is caught on its own and only logged — the menu
        // simply shows no user presets.
        try {
          const saved = await api.invoke<NamedPreset[]>('list_stacking_presets', {});
          if (cancelled) return;
          setUserPresets(saved);
        } catch (err) {
          console.error('[StackingTab] list_stacking_presets failed:', err);
        }
        // This plan fetch is one of four call sites that can write `plan`
        // (the other three go through `refetchPlan`) — guarded by the same
        // `planSeqRef`/`subjectKeyRef` those share, so whichever response
        // is actually the most recent wins regardless of which code path
        // issued it (a `library-updated` event or the outcome effect can
        // race against this very fetch).
        const seq = ++planSeqRef.current;
        const forKey = stackKey;
        const p = await subjectApi.getPlan(cfg.config);
        if (cancelled) return;
        if (seq !== planSeqRef.current || forKey !== subjectKeyRef.current) {
          return; // superseded by a later refetchPlan/mount call
        }
        setPlan(p);
      } catch (err) {
        console.error('[StackingTab] failed to load stacking config/plan:', err);
        if (!cancelled) setLoadError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, [subjectApi, stackKey]);

  // Always-current mirror of `draftConfig` for effects that must react to
  // something ELSE (a DOM event, a finished run) without themselves
  // re-running on every keystroke of config editing.
  const draftConfigRef = useRef<StackingConfig | null>(draftConfig);
  useEffect(() => { draftConfigRef.current = draftConfig; }, [draftConfig]);

  // A master build (or another completed stacking run) changed the catalog
  // shape underneath us — re-plan.
  useEffect(() => {
    const handler = () => { void refetchPlan(draftConfigRef.current ?? undefined); };
    window.addEventListener('library-updated', handler);
    return () => window.removeEventListener('library-updated', handler);
  }, [refetchPlan]);

  // Re-plan (debounced) whenever the draft config changes — skip the very
  // first assignment, which is the mount effect's own initial fetch.
  const skipFirstDraftEffect = useRef(true);
  useEffect(() => {
    if (skipFirstDraftEffect.current) {
      skipFirstDraftEffect.current = false;
      return;
    }
    if (!draftConfig) return;
    const t = setTimeout(() => { void refetchPlan(draftConfig); }, 300);
    return () => clearTimeout(t);
  }, [draftConfig, refetchPlan]);

  // A run just finished (success or otherwise) — the plan's stale-stage /
  // cached-artifact state is now out of date.
  const lastOutcomeRunId = runOutcome?.runId;
  useEffect(() => {
    if (lastOutcomeRunId == null) return;
    void refetchPlan(draftConfigRef.current ?? undefined);
  }, [lastOutcomeRunId, refetchPlan]);

  // The lights the plan includes, and the masters their calibration reads: what a run would extract from
  // zips before it can read them. A frame set only: a project's zip-resident own frames are counted by the
  // plan's `prepare.atRun`.
  const zipScope = useMemo(() => {
    if (subject.kind !== 'frameSet') return null;
    const excludedSet = new Set(excludedFrameIds);
    return { frameIds: lightFrames.map((f) => f.frameId).filter((id) => !excludedSet.has(id)), withLinkedMasters: true };
  }, [subject.kind, lightFrames, excludedFrameIds]);

  // The most recent user-originated write still waiting to go out — set by
  // the persist effect below when it schedules the 500 ms timer, read by
  // both that timer and the flush effect (Important #3), cleared by
  // whichever of them actually sends it.
  const pendingWriteRef = useRef<PendingWrite | null>(null);

  const sendPendingWrite = useCallback((notifyOnFailure: boolean) => {
    const payload = pendingWriteRef.current;
    if (!payload) return;
    dirtyRef.current = false;
    const shouldRefetchPlan = exclusionDirtyRef.current;
    exclusionDirtyRef.current = false;
    pendingWriteRef.current = null;
    payload.api.setConfig(payload.value)
      .then(() => {
        // `build_plan` computes `includedCount` (and everything else the
        // excluded-frame list affects) from the STORED row, never a client
        // override (`crates/athenaeum-core/src/stacking/plan.rs`) — a
        // config edit alone already re-plans via the 300 ms draft-change
        // effect using a client override, but the Frames table's include
        // checkbox only ever changes the exclusion list, which that effect
        // does not watch. Re-plan only when THIS write actually changed the
        // excluded-id list (fix round 1, Minor #4) — a pure config write
        // skips this, since the draft effect already covers it and a
        // second refetch here would just be a redundant round trip. And
        // only while the tab still shows the subject the write was made
        // for: a slow write of the subject just left must not re-plan it
        // (that would bump `planSeqRef` and drop the new subject's own
        // plan fetch still on its way).
        if (shouldRefetchPlan && payload.key === subjectKeyRef.current) {
          void refetchPlan(payload.value.config);
        }
      })
      .catch((err) => {
        console.error(
          `[StackingTab] ${payload.kind === 'frameSet' ? 'set_stacking_config' : 'set_project_stacking_config'} failed:`,
          err,
        );
        if (notifyOnFailure) {
          notify({
            tone: 'warning',
            kind: 'stacking',
            toast: true,
            title: 'Stacking settings not saved',
            detail: String(err),
          });
        }
      });
  }, [notify, refetchPlan]);

  // Persist (debounced) whenever the draft config or the excluded-frame list
  // changes. Fix round 1, Critical #1: writes only when `dirtyRef.current`
  // is true — the load effect's own seed/reset never sets it, so opening
  // the tab on a set with no stored override can never materialize one.
  // Critical #2: also skipped when `draftForKeyRef` still names a
  // DIFFERENT subject than the current `stackKey` — the one-commit window
  // between a subject switch and the new subject's load effect resetting
  // the draft, where this effect would otherwise still see the OLD
  // subject's draftConfig alongside the NEW key and write the wrong one.
  // The scheduled write carries the adapter and key it was made under
  // (`PendingWrite`), so the flush below reaches the old subject even
  // though the tab already shows the next one.
  // Submit state, never a re-read (spec §11.2): a failed write logs and
  // warns but never rolls the draft back or re-fetches. The adapter takes
  // the whole document; `isDefault`/`updatedAt` are placeholders, neither
  // command sends them.
  useEffect(() => {
    if (!draftConfig) return;
    if (!dirtyRef.current) return;
    if (draftForKeyRef.current !== stackKey) return;
    pendingWriteRef.current = {
      api: subjectApi,
      kind: subject.kind,
      key: stackKey,
      value: { config: draftConfig, excluded, referenceFrameUuid, isDefault: false, updatedAt: null },
    };
    const t = setTimeout(() => sendPendingWrite(true), 500);
    return () => clearTimeout(t);
  }, [draftConfig, excluded, referenceFrameUuid, stackKey, subject.kind, subjectApi, sendPendingWrite]);

  // Flush instead of cancel (Important #3): a subject switch or unmount must
  // not silently drop an edit the 500 ms timer above hadn't reached yet.
  // This effect's own cleanup — which fires exactly when `stackKey` is
  // about to change, or on true unmount — sends whatever the persist
  // effect last scheduled, fire-and-forget (no `notify`; the tab the user
  // is leaving isn't the place to toast a failure).
  useEffect(() => {
    return () => {
      if (dirtyRef.current && pendingWriteRef.current) {
        sendPendingWrite(false);
      }
    };
  }, [stackKey, sendPendingWrite]);

  const handleConfigChange = useCallback((next: StackingConfig) => {
    setUserConfig(next);
  }, [setUserConfig]);

  const handleToggleDrizzle = useCallback((checked: boolean) => {
    setUserConfig((prev) =>
      prev ? { ...prev, drizzle: { ...prev.drizzle, enabled: checked } } : prev,
    );
  }, [setUserConfig]);

  const handleSelectStage = useCallback((stage: BoardStage) => {
    setSelectedStage(stage);
    writeSelectedStage(stage);
  }, []);

  // Both toolbar dropdowns (preset selector, Re-run from) close on an
  // outside click or Escape (fix round 1, Minor #8 — named for the Re-run
  // menu specifically, applied to both since they share the exact same
  // open/close pattern in this same file).
  const presetMenuRef = useRef<HTMLDivElement>(null);
  const rerunMenuRef = useRef<HTMLDivElement>(null);
  // M4d Task 4: closing the menu also discards whatever inline form was
  // open inside it — a half-typed preset name or an unanswered delete
  // confirm must not still be waiting there the next time it opens.
  const closePresetMenu = useCallback(() => {
    setPresetMenuOpen(false);
    setSavePresetOpen(false);
    setPresetNameDraft('');
    setPresetPendingDelete(null);
  }, []);
  useEffect(() => {
    if (!presetMenuOpen && !rerunMenuOpen) return;
    const handlePointerDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (presetMenuOpen && presetMenuRef.current && !presetMenuRef.current.contains(target)) {
        closePresetMenu();
      }
      if (rerunMenuOpen && rerunMenuRef.current && !rerunMenuRef.current.contains(target)) {
        setRerunMenuOpen(false);
      }
    };
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      closePresetMenu();
      setRerunMenuOpen(false);
    };
    document.addEventListener('mousedown', handlePointerDown);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handlePointerDown);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [presetMenuOpen, rerunMenuOpen, closePresetMenu]);

  // Preset selector (plan Ruling 1): the label is computed, never stored —
  // comparing the draft (minus its per-set folder override) against each
  // built-in preset via canonical JSON. M4d Task 4 (ruling R-M4d-6) extends
  // it to the user's own presets by exactly the same rule; built-ins win a
  // tie, since a user preset saved as a copy of "Default" IS Default and
  // showing the built-in name is the less surprising of the two answers.
  // Canonicalized once per list change, not once per draft edit: `presetLabel`
  // below re-runs on every keystroke in any inspector field, and there can be
  // up to 50 user presets whose configs are the same size as the draft's.
  const userPresetKeys = useMemo(
    () => userPresets.map((p) => [stableStringify(withoutPaths(p.config)), p.name] as const),
    [userPresets],
  );
  const presetLabel = useMemo<string>(() => {
    if (!draftConfig || !presets) return 'Custom';
    const draftKey = stableStringify(withoutPaths(draftConfig));
    if (draftKey === stableStringify(withoutPaths(presets.default))) return 'Default';
    if (draftKey === stableStringify(withoutPaths(presets.fastPreview))) return 'Fast preview';
    if (draftKey === stableStringify(withoutPaths(presets.maximumQuality))) return 'Maximum quality';
    const mine = userPresetKeys.find(([key]) => key === draftKey);
    if (mine) return `'${mine[1]}'`;
    return 'Custom';
  }, [draftConfig, presets, userPresetKeys]);

  const applyPreset = useCallback((preset: StackingPreset) => {
    setUserConfig((prev) => {
      if (!presets || !prev) return prev;
      return { ...presets[preset], paths: prev.paths };
    });
    closePresetMenu();
  }, [presets, setUserConfig, closePresetMenu]);

  // A saved preset never carries folders (the backend strips `paths` before
  // storing), so applying one keeps THIS set's own working/output override —
  // same rule as a built-in above.
  const applyUserPreset = useCallback((saved: NamedPreset) => {
    setUserConfig((prev) => (prev ? { ...saved.config, paths: prev.paths } : prev));
    closePresetMenu();
  }, [setUserConfig, closePresetMenu]);

  const handleSavePreset = useCallback(async () => {
    const name = presetNameDraft.trim();
    if (!draftConfig || !name || presetBusy) return;
    setPresetBusy(true);
    try {
      const saved = await api.invoke<NamedPreset[]>('save_stacking_preset', {
        name,
        config: draftConfig,
      });
      setUserPresets(saved);
      closePresetMenu();
      notify({
        tone: 'success',
        kind: 'stacking',
        toast: true,
        title: `Preset '${name}' saved`,
        detail: 'The current stacking settings, without the folders.',
      });
    } catch (err) {
      console.error('[StackingTab] save_stacking_preset failed:', err);
      notify({
        tone: 'warning',
        kind: 'stacking',
        toast: true,
        title: 'Preset not saved',
        detail: String(err),
      });
    } finally {
      setPresetBusy(false);
    }
  }, [presetNameDraft, draftConfig, presetBusy, notify, closePresetMenu]);

  const handleDeletePreset = useCallback(async (name: string) => {
    if (presetBusy) return;
    setPresetBusy(true);
    try {
      const left = await api.invoke<NamedPreset[]>('delete_stacking_preset', { name });
      setUserPresets(left);
      setPresetPendingDelete(null);
      notify({
        tone: 'info',
        kind: 'stacking',
        toast: true,
        title: `Preset '${name}' deleted`,
        detail: left.length === 1 ? '1 preset left' : `${left.length} presets left`,
      });
    } catch (err) {
      console.error('[StackingTab] delete_stacking_preset failed:', err);
      notify({
        tone: 'warning',
        kind: 'stacking',
        toast: true,
        title: 'Preset not deleted',
        detail: String(err),
      });
    } finally {
      setPresetBusy(false);
    }
  }, [presetBusy, notify]);

  // `starting` bridges the click-to-run gap. `startRun`'s invoke does not
  // resolve until the backend has synchronously built the WHOLE plan
  // (gate + per-frame hashing) and spawned the run thread (see
  // `routes/stacking.rs`'s doc comment on `start_stacking`) — clearing
  // `starting` the instant that promise resolves (the old behavior) left a
  // real gap, between the invoke resolving and the run's first
  // `stacking-progress` event actually landing, where BOTH `starting` and
  // `running` (`isRunning`, which only becomes true once a progress event
  // arrives) read false — re-enabling the Run button and allowing a
  // double-start (fix round 1, Minor #5). `starting` now stays true until
  // either a progress event or an outcome for the SPECIFIC run just started
  // arrives (tracked by `startingRunIdRef`, not just "any progress/outcome
  // for this set", since `lastOutcome` never clears and would otherwise
  // read as "already finished" the instant a NEW run starts).
  const startingRunIdRef = useRef<number | null>(null);

  const beginStarting = useCallback(async (invoke: () => Promise<number>) => {
    setStarting(true);
    try {
      startingRunIdRef.current = await invoke();
    } catch {
      // `useStackingRuns`'s `startRun` already logs + notifies before
      // rethrowing (fix round 1, Minor #4) — nothing left to do here but
      // stop showing "starting".
      startingRunIdRef.current = null;
      setStarting(false);
    }
  }, []);

  useEffect(() => {
    if (!starting) return;
    const waitingFor = startingRunIdRef.current;
    if (waitingFor == null) return; // still awaiting `startRun`'s invoke itself
    if (runProgress?.runId === waitingFor || runOutcome?.runId === waitingFor) {
      startingRunIdRef.current = null;
      setStarting(false);
    }
  }, [starting, runProgress, runOutcome]);

  const handleRun = useCallback(() => {
    if (!plan || plan.blockers.length > 0 || running || starting) return;
    void beginStarting(() => startRun(subject, draftConfig ?? undefined));
  }, [plan, running, starting, beginStarting, startRun, subject, draftConfig]);

  const handleCancel = useCallback(async () => {
    const runId = runProgress?.runId ?? plan?.activeRunId ?? null;
    if (runId == null) return;
    try {
      await cancelRun(runId);
    } catch {
      // `useStackingRuns`'s `cancelRun` already logs + notifies.
    }
  }, [runProgress, plan, cancelRun]);

  const handleRerunFrom = useCallback((stage: Stage) => {
    setRerunMenuOpen(false);
    if (running || starting) return;
    void beginStarting(() => startRun(subject, draftConfig ?? undefined, stage));
  }, [running, starting, beginStarting, startRun, subject, draftConfig]);

  // Measure panel's own "Re-measure" button (spec: rerunFrom: 'measure').
  // Gates on the plan's blockers and on a live run, the same way the
  // toolbar's "Re-run from" menu does since v0.6.3.
  const remeasureDisabled = running || starting || (plan?.blockers.length ?? 0) > 0;
  const handleRemeasure = useCallback(() => {
    void handleRerunFrom('measure');
  }, [handleRerunFrom]);

  // `FrameSetDetail.tsx`'s searchParams effect only highlights a Coverage
  // row when BOTH `highlightSet` and `kind` are present and `kind` parses
  // as `'flat' | 'dark' | 'bias'` — `&kind=` was missing (fix round 1,
  // Minor #7). `StackingPlan.readiness` (`ExportReadiness`) carries no
  // set→kind map the way `ExportTab.tsx`'s own `summary`-derived
  // `setKindMap` does, so the real kind of the first set without a master
  // can't be resolved here without a second fetch — `'dark'` is exactly
  // ExportTab's OWN fallback (`setKindMap.get(setId) ?? 'dark'`) for a set
  // its map doesn't know either, so this matches its behavior rather than
  // inventing a new default.
  const handleCoverageClick = useCallback(() => {
    const setId = plan && plan.readiness.rawSetsWithoutMaster > 0
      ? plan.readiness.rawSetIdsWithoutMaster[0]
      : undefined;
    if (setId !== undefined) {
      navigate(`?tab=calibration&highlightSet=${setId}&kind=dark`, { replace: true });
    } else {
      navigate('?tab=calibration', { replace: true });
    }
  }, [navigate, plan]);

  if (loading) {
    return (
      <div className="text-center py-12">
        <Loader2 size={28} className="animate-spin mx-auto mb-3 text-content-muted" />
        <p className="text-content-muted">Loading stacking plan…</p>
      </div>
    );
  }

  if (loadError || !plan || !draftConfig || !presets) {
    return (
      <div className="text-center py-12 text-content-muted">
        <p>Failed to load the stacking plan{loadError ? `: ${loadError}` : '.'}</p>
      </div>
    );
  }

  // Spec §8: "Re-run from" lists the stages whose cached outputs would be
  // REUSED above it — the cacheable stages a run would not otherwise redo,
  // each forcing itself and everything after it fresh. Plan 5b built the
  // menu the other way round (`staleStages ∪ {integrate}`, disabled when
  // nothing was stale): forcing a stage that is already stale is exactly
  // what ▶ Run does, so every entry was a no-op and the one case a user
  // reaches for it — "just re-integrate, everything is cached" — was the
  // one case the menu refused to open. Stale stages stay listed but inert,
  // saying so; Integrate has no cache and is always there (its own re-run
  // IS ▶ Run, stated explicitly). Where ▶ Run actually starts is shown on
  // its tooltip and under the menu — after a `deleteIntermediates` run
  // that is Calibrate whatever the menu says, and `plan.warnings` names
  // the run that deleted the caches.
  const lnActive =
    draftConfig.normalization.local.enabled || draftConfig.normalization.rejection === 'local';
  const cacheableStages: Stage[] = (['calibrate', 'measure', 'register', 'normalize'] as Stage[])
    .filter((s) => s !== 'normalize' || lnActive);
  const firstStale = cacheableStages.find((s) => plan.staleStages.includes(s));
  const rerunOptions: { stage: Stage; stale: boolean }[] = [
    ...cacheableStages.map((stage) => ({ stage, stale: plan.staleStages.includes(stage) })),
    { stage: 'integrate', stale: false },
  ];
  // Blocked exactly like ▶ Run: every entry ends in `start_stacking`, which
  // refuses a blocked plan, so an open menu would only offer failures.
  const rerunDisabled = running || starting || plan.blockers.length > 0;
  const measureCached = !plan.staleStages.includes('measure');
  // Wave 6c: a project or an attested set shows Prepare where a frame set shows Calibrate; every name and
  // sentence here follows the board.
  const mode = boardMode(plan);
  const firstStage = stageLabel('calibrate', mode);
  const runStartsAt =
    firstStale === undefined
      ? 'Every cached stage is reused — the run re-integrates from the cached registered frames'
      : firstStale === 'calibrate'
        ? measureCached
          ? `No ${mode === 'prepare' ? 'prepared' : 'calibrated'} or registered frames are cached — the run starts from ${firstStage}; the measurements are reused`
          : `Nothing is cached — the run starts from ${firstStage}`
        : `Starts from ${stageLabel(firstStale, mode)} — the stages before it are reused from cache`;
  // Fix round 1, Minor #8: APPLYING a preset edits the config the same way
  // every inspector panel does, so it is disabled while a run is active for
  // the same reason those panels are (`StageInspector`'s own `disabled`
  // prop below).
  //
  // M4d Task 4 fix round 1 (review I1): that gate used to close the whole
  // MENU, which took the new Save-as and delete actions down with it — for
  // the length of a run, which on a real set is the better part of an hour,
  // and which is exactly the moment a user wants to save the settings they
  // just launched with. Neither of those two touches the draft config, so
  // only APPLY is gated now: the menu opens during a run, the preset rows
  // are inert with the existing disabled styling and a title saying why,
  // and Save-as / delete stay live.
  const presetApplyDisabled = running || starting;
  const presetApplyTitle = presetApplyDisabled
    ? 'A run is in progress — the settings cannot change until it finishes'
    : undefined;
  const runDisabled = plan.blockers.length > 0 || running || starting;
  const runTooltip = plan.blockers.length > 0 ? plan.blockers[0].message : runStartsAt;
  const freeLabel = plan.freeBytes == null ? 'free space unknown' : `Free ${formatGB(plan.freeBytes)}`;

  return (
    // Plan 5b final fix wave, review finding B7 (supersedes click-through
    // item A4): `min-w-0` on the tab's own root — a flex child (the tab
    // content area FrameSetDetail.tsx renders this into) defaults to
    // `min-width: auto`, which lets a wide descendant (the Frames table)
    // grow the WHOLE PAGE instead of scrolling inside its own
    // `overflow-x-auto` wrapper. The table already has that wrapper and
    // `min-w-0`; this and the board/inspector split's own `min-w-0` below
    // are what actually let it apply.
    <div className="space-y-3 min-w-0">
      {/* Toolbar. Final fix wave, A4: at the acceptance run's click-through
          (~1250 CSS px), this row overflowed the pane and the whole page
          scrolled horizontally — `flex-wrap` on the two flex containers
          below was already in place, but neither folder-path span could
          actually SHRINK (a flex item's default `min-width: auto` beats
          `truncate`), so the left group's own content forced the row wide
          rather than wrapping. `min-w-0` on both flex containers plus
          `min-w-0 truncate` on each path span fixes that; `shrink-0` on the
          button group keeps Run/Cancel/Re-run intact on their own line once
          the row does wrap. */}
      <div className="flex flex-wrap items-center justify-between gap-3 bg-surface-elevated rounded-lg px-4 py-3">
        <div className="flex flex-wrap items-center gap-4 min-w-0 text-sm text-content-secondary">
          <div className="relative shrink-0" ref={presetMenuRef}>
            <button
              type="button"
              onClick={() => (presetMenuOpen ? closePresetMenu() : setPresetMenuOpen(true))}
              className="flex items-center gap-1 font-medium transition-colors text-content hover:text-content-secondary"
            >
              {presetLabel}
              <ChevronDown size={14} />
            </button>
            {presetMenuOpen && (
              <div className="absolute left-0 mt-1 w-64 bg-surface-elevated border border-border rounded-lg shadow-lg z-10 py-1">
                {(Object.keys(PRESET_LABEL) as StackingPreset[]).map((p) => (
                  <button
                    key={p}
                    type="button"
                    onClick={() => applyPreset(p)}
                    disabled={presetApplyDisabled}
                    title={presetApplyTitle}
                    className={`w-full text-left px-3 py-1.5 text-sm ${
                      presetApplyDisabled
                        ? 'text-content-muted cursor-not-allowed'
                        : 'text-content-secondary hover:bg-surface-hover'
                    }`}
                  >
                    {PRESET_LABEL[p]}
                  </button>
                ))}

                {/* M4d Task 4 (ruling R-M4d-6): the user's own presets below
                    a divider. Each row applies on click; the trash icon
                    turns that same row into an inline confirm — no browser
                    `confirm()`, which would sit outside the app's own
                    surface and cannot be styled or dismissed with Escape
                    like everything else here. */}
                {userPresets.length > 0 && (
                  <>
                    <div className="my-1 border-t border-border" />
                    {userPresets.map((saved) =>
                      presetPendingDelete === saved.name ? (
                        <div
                          key={saved.name}
                          className="px-3 py-1.5 text-sm text-content-secondary"
                        >
                          <div className="truncate" title={saved.name}>
                            Delete preset '{saved.name}'?
                          </div>
                          <div className="mt-1 flex items-center gap-2">
                            <button
                              type="button"
                              disabled={presetBusy}
                              onClick={() => void handleDeletePreset(saved.name)}
                              className="px-2 py-0.5 rounded text-xs font-medium bg-error text-white hover:brightness-110 disabled:opacity-50"
                            >
                              Delete
                            </button>
                            <button
                              type="button"
                              onClick={() => setPresetPendingDelete(null)}
                              className="px-2 py-0.5 rounded text-xs text-content-muted hover:text-content"
                            >
                              Cancel
                            </button>
                          </div>
                        </div>
                      ) : (
                        <div
                          key={saved.name}
                          className={`flex items-center ${
                            presetApplyDisabled ? '' : 'hover:bg-surface-hover'
                          }`}
                        >
                          <button
                            type="button"
                            onClick={() => applyUserPreset(saved)}
                            disabled={presetApplyDisabled}
                            title={presetApplyTitle ?? saved.name}
                            className={`flex-1 min-w-0 text-left px-3 py-1.5 text-sm truncate ${
                              presetApplyDisabled
                                ? 'text-content-muted cursor-not-allowed'
                                : 'text-content-secondary'
                            }`}
                          >
                            {saved.name}
                          </button>
                          <button
                            type="button"
                            onClick={() => setPresetPendingDelete(saved.name)}
                            title={`Delete preset '${saved.name}'`}
                            aria-label={`Delete preset '${saved.name}'`}
                            className="px-2 py-1.5 text-content-muted hover:text-error shrink-0"
                          >
                            <Trash2 size={14} />
                          </button>
                        </div>
                      ),
                    )}
                  </>
                )}

                <div className="my-1 border-t border-border" />
                {savePresetOpen ? (
                  <div className="px-3 py-1.5">
                    <input
                      type="text"
                      autoFocus
                      value={presetNameDraft}
                      maxLength={PRESET_NAME_MAX}
                      placeholder="Preset name"
                      onChange={(e) => setPresetNameDraft(e.target.value)}
                      onKeyDown={(e) => {
                        // Escape is handled by the menu-wide listener; Enter
                        // is the form's own submit, so it must not bubble up
                        // into anything else on the toolbar.
                        if (e.key === 'Enter') {
                          e.preventDefault();
                          void handleSavePreset();
                        }
                      }}
                      className="w-full px-2 py-1 text-sm rounded border border-border bg-surface text-content placeholder:text-content-muted focus:outline-none focus:border-accent"
                    />
                    <div className="mt-1.5 flex items-center gap-2">
                      <button
                        type="button"
                        disabled={presetBusy || presetNameDraft.trim().length === 0}
                        onClick={() => void handleSavePreset()}
                        className="px-2 py-0.5 rounded text-xs font-medium bg-accent text-surface hover:bg-accent-hover disabled:opacity-50 disabled:cursor-not-allowed"
                      >
                        Save
                      </button>
                      <button
                        type="button"
                        onClick={() => {
                          setSavePresetOpen(false);
                          setPresetNameDraft('');
                        }}
                        className="px-2 py-0.5 rounded text-xs text-content-muted hover:text-content"
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                ) : (
                  <button
                    type="button"
                    onClick={() => {
                      setPresetPendingDelete(null);
                      setSavePresetOpen(true);
                    }}
                    className="w-full text-left px-3 py-1.5 text-sm text-content-secondary hover:bg-surface-hover"
                  >
                    Save current as…
                  </button>
                )}
              </div>
            )}
          </div>
          <span
            className="flex items-center gap-1.5 min-w-0 max-w-xs truncate"
            title={plan.workingDir ?? undefined}
          >
            <FolderOpen size={14} className="text-content-muted shrink-0" />
            <span className="truncate">{plan.workingDir ?? 'Choose a working folder'}</span>
          </span>
          <span
            className="flex items-center gap-1.5 min-w-0 max-w-xs truncate"
            title={plan.outputDir ?? undefined}
          >
            <FolderOpen size={14} className="text-content-muted shrink-0" />
            <span className="truncate">{plan.outputDir ?? 'Choose an output folder'}</span>
          </span>
          <span className="text-content-muted shrink-0">
            {freeLabel} · estimate {formatGB(plan.estimateBytes)}
          </span>
          <ZipExtractionLine scope={zipScope} refreshKey={lastOutcomeRunId ?? 0} className="shrink-0" />
        </div>

        <div className="flex items-center gap-2 shrink-0">
          <button
            type="button"
            onClick={() => void handleRun()}
            disabled={runDisabled}
            title={runTooltip}
            className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors ${
              runDisabled
                ? 'bg-surface text-content-muted cursor-not-allowed'
                : 'bg-accent text-surface hover:bg-accent-hover'
            }`}
          >
            {starting ? <Loader2 size={14} className="animate-spin" /> : <Play size={14} />}
            Run stacking
          </button>

          {running && (
            <button
              type="button"
              onClick={() => void handleCancel()}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border border-border text-content-secondary hover:bg-surface-hover transition-colors"
            >
              <Square size={14} />
              Cancel
            </button>
          )}

          <div className="relative" ref={rerunMenuRef}>
            <button
              type="button"
              onClick={() => setRerunMenuOpen((v) => !v)}
              disabled={rerunDisabled}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border transition-colors ${
                rerunDisabled
                  ? 'border-border text-content-muted cursor-not-allowed'
                  : 'border-border text-content-secondary hover:bg-surface-hover'
              }`}
            >
              Re-run from
              <ChevronDown size={14} />
            </button>
            {rerunMenuOpen && !rerunDisabled && (
              <div className="absolute right-0 mt-1 w-64 bg-surface-elevated border border-border rounded-lg shadow-lg z-10 py-1">
                {rerunOptions.map(({ stage, stale }) => (
                  <button
                    key={stage}
                    type="button"
                    disabled={stale}
                    onClick={() => void handleRerunFrom(stage)}
                    title={
                      stale
                        ? 'Not cached — every run redoes this stage already'
                        : stage === 'integrate'
                          ? 'Reuse every cached stage and integrate again'
                          : `Redo ${stageLabel(stage, mode)} and everything after it; reuse the stages before`
                    }
                    className={`w-full text-left px-3 py-1.5 text-sm ${
                      stale
                        ? 'text-content-muted cursor-not-allowed'
                        : 'text-content-secondary hover:bg-surface-hover'
                    }`}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span>{stageLabel(stage, mode)}</span>
                      {stale && <span className="text-xs">stale</span>}
                    </span>
                  </button>
                ))}
                <p className="px-3 pt-1.5 pb-1 text-xs text-content-muted border-t border-border mt-1">
                  {runStartsAt}
                </p>
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Blockers */}
      {plan.blockers.length > 0 && (
        <div className="space-y-1">
          {plan.blockers.map((b, i) => (
            <div key={`${b.code}-${i}`} className="flex items-center gap-2 text-sm text-error">
              <AlertTriangle size={14} className="shrink-0" />
              <span>{b.message}</span>
              {/* A project has no Calibration tab to go to: its frames come calibrated. */}
              {subject.kind !== 'project' &&
                (b.code === 'masters' || b.code === 'links' || b.code === 'masterFiles') && (
                  <button
                    type="button"
                    className="underline hover:no-underline text-content-secondary"
                    onClick={handleCoverageClick}
                  >
                    → Coverage
                  </button>
                )}
            </div>
          ))}
        </div>
      )}

      {/* Warnings */}
      {plan.warnings.length > 0 && (
        <div className="space-y-1">
          {plan.warnings.map((w, i) => (
            <p key={i} className="text-sm text-warning">{w}</p>
          ))}
        </div>
      )}

      {/* Board (62%) / Inspector (38%) */}
      <div className="flex flex-col lg:flex-row gap-4">
        <div className="lg:w-[62%] min-w-0 space-y-3">
          <PipelineBoard
            plan={plan}
            config={draftConfig}
            progress={runProgress}
            outcome={runOutcome}
            finishedStages={boardFinishedStages}
            selectedStage={selectedStage}
            onSelectStage={handleSelectStage}
            onToggleDrizzle={handleToggleDrizzle}
          />
          <div className="bg-surface-elevated rounded-lg p-3">
            <h4 className="text-sm font-medium text-content mb-2">Groups ({plan.groups.length})</h4>
            <GroupsTable groups={plan.groups} />
          </div>
        </div>

        <div className="lg:w-[38%] min-w-0 space-y-2">
          {/* Below `lg` only: a summary button naming the selected stage,
           *  toggling the panel below it — `lg` and up hides this button
           *  entirely and the panel is always shown (Task 2/3's original
           *  side-by-side shape, unchanged). One `StageInspector` mount
           *  either way, so a panel with its own fetch on mount (Output's
           *  `get_stacking_paths`) never runs twice. */}
          <button
            type="button"
            onClick={handleToggleInspectorCollapsed}
            aria-expanded={!inspectorCollapsed}
            className="lg:hidden w-full flex items-center gap-1.5 px-4 py-2.5 rounded-lg bg-surface-elevated text-sm font-medium text-content hover:text-content-secondary transition-colors"
          >
            {inspectorCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
            {stageLabel(selectedStage, mode)}
          </button>
          <div className={`${inspectorCollapsed ? 'hidden' : 'block'} lg:block`}>
            <StageInspector
              stage={selectedStage}
              config={draftConfig}
              onChange={handleConfigChange}
              plan={plan}
              disabled={running || starting}
              presetDefault={presets.default}
              onRemeasure={handleRemeasure}
              remeasureDisabled={remeasureDisabled}
            />
          </div>
        </div>
      </div>

      {/* Frames + Results — full width, below the board/inspector split
       *  (the Frames table's nine columns need the room; the Results
       *  panel's master cards read better at full width too). */}
      <FramesTable
        lightFrames={lightFrames}
        runDetail={selectedRunDetail}
        excludedFrameIds={excludedFrameIds}
        onToggleExclude={handleToggleExcludeFrame}
        collapsed={framesCollapsed}
        onToggleCollapsed={handleToggleFramesCollapsed}
        disabled={running || subject.kind !== 'frameSet'}
      />

      <ResultsPanel
        subject={subject}
        running={running}
        onSelectedRunDetailChange={setSelectedRunDetail}
      />
    </div>
  );
}
