// Metadata mode for a dual-pane.
//
// Shows aggregate metadata for the OTHER pane's selection plus a bulk-edit
// form that writes to the catalog only (no FITS-on-disk modifications).
// Edits go through `bulk_update_frame_metadata` Tauri/web command — see
// CLAUDE.md and the file_op extension docs.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Save, Loader2, AlertTriangle, ExternalLink, RotateCcw, Crosshair } from 'lucide-react';
import { useNavigate } from 'react-router-dom';
import { api } from '../../api';
import type {
  CalibrationSetMembership,
  DirectoryListing,
  FrameMembershipsSummary,
  FrameMetadataRelations,
  FrameOriginalSnapshot,
  FrameSetMembership,
  UsedInFrameSetMembership,
} from './types';
import {
  PlateSolveBatchPanel,
  type PlateSolveBatchPanelHandle,
} from '../plate-solve/PlateSolveBatchPanel';
import type { PlateSolveConfig, PlateSolveRecord } from '../../types/plate-solve';
import { formatTimestamp } from '../../utils/dateFormatting';

interface Props {
  /** Listing of the FILE pane (the other side). */
  otherListing: DirectoryListing | null;
  /** Selection of the FILE pane (paths). */
  otherSelection: Set<string>;
  /** Called after a successful save so the caller can refresh listings. */
  onSaved: () => void;
}

interface EditState {
  enabled: boolean;
  value: string;
}

const VARIES = '__VARIES__';

function commonValueStr(values: Array<string | null | undefined>): string {
  const set = new Set(values.map((v) => (v == null ? '' : v)));
  if (set.size === 1) {
    const only = Array.from(set)[0];
    return only ?? '';
  }
  return VARIES;
}

function commonValueNum(values: Array<number | null | undefined>): string {
  const set = new Set(values.map((v) => (v == null ? '' : String(v))));
  if (set.size === 1) {
    const only = Array.from(set)[0];
    return only ?? '';
  }
  return VARIES;
}

/** True when two ISO-ish datetime strings represent the same instant.
 *  Handles the catalog-vs-FITS-header mismatch where the catalog adds a
 *  trailing `Z` (RFC3339, UTC) and the FITS header has the naive form
 *  without a timezone marker. Both are treated as UTC since FITS DATE-OBS
 *  is conventionally UTC. */
function sameDateInstant(a: string, b: string): boolean {
  const norm = (s: string) => {
    const t = s.trim();
    if (!t) return null;
    // If the string already has a timezone marker (Z or ±HH:MM after the
    // 'T...' portion), parse as-is. Otherwise treat as UTC.
    const tail = t.slice(10);
    const hasTz = /Z$/i.test(t) || /[+-]\d{2}:?\d{2}$/.test(tail);
    const parsed = new Date(hasTz ? t : t + 'Z');
    const ms = parsed.getTime();
    return Number.isFinite(ms) ? ms : null;
  };
  const ma = norm(a);
  const mb = norm(b);
  if (ma == null || mb == null) return false;
  return ma === mb;
}

function display(v: string): string {
  return v === VARIES ? '(varies)' : v || '(empty)';
}

/** True for IMAGETYP values that represent on-sky light frames (single
 *  exposures or stacked masters). Coordinates / plate-solve only make
 *  sense for these — calibration frames legitimately have no RA/Dec.
 *  Match the same uppercase + space-stripping normalization used by the
 *  imagetyp edit state, so 'Light', 'MASTERLIGHT', 'Master Light' all
 *  compare equal. */
function isLightLike(v: string | null | undefined): boolean {
  if (!v) return false;
  const norm = String(v).toUpperCase().replace(/\s+/g, '');
  return norm === 'LIGHT' || norm === 'MASTERLIGHT';
}


export default function MetadataPane({ otherListing, otherSelection, onSaved }: Props) {
  // Resolve selected FileWithFrame entries.
  const selectedItems = useMemo(() => {
    if (!otherListing) return [];
    return otherListing.files.filter((f) => otherSelection.has(f.file.path));
  }, [otherListing, otherSelection]);

  const frameIds = useMemo(
    () =>
      selectedItems
        .map((f) => f.frame?.id)
        .filter((id): id is number => id != null),
    [selectedItems],
  );

  // Common values across the selection.
  const common = useMemo(() => {
    return {
      object: commonValueStr(selectedItems.map((f) => f.frame?.object)),
      filter: commonValueStr(selectedItems.map((f) => f.frame?.filter)),
      imagetyp: commonValueStr(selectedItems.map((f) => f.frame?.imagetyp)),
      instrume: commonValueStr(selectedItems.map((f) => f.frame?.instrume)),
      telescop: commonValueStr(selectedItems.map((f) => f.frame?.telescop)),
      focallen: commonValueNum(selectedItems.map((f) => f.frame?.focallen)),
      xpixsz: commonValueNum(selectedItems.map((f) => f.frame?.xpixsz)),
      dateObs: commonValueStr(selectedItems.map((f) => f.frame?.date_obs)),
      gain: commonValueNum(selectedItems.map((f) => f.frame?.gain)),
      offset: commonValueNum(selectedItems.map((f) => f.frame?.offset)),
      binning: commonValueStr(selectedItems.map((f) => f.frame?.binning)),
      exptime: commonValueNum(selectedItems.map((f) => f.frame?.exptime)),
      ccdTemp: commonValueNum(selectedItems.map((f) => f.frame?.ccd_temp)),
      // Coordinates are stored in the catalog as decimal degrees. The UI
      // renders them as sexagesimal — see formatRa/DecSexagesimal in the row
      // path. There is no text input for these: the only way to write a new
      // value is plate-solving (see the "Plate Solve" button below).
      ra: commonValueNum(selectedItems.map((f) => f.frame?.ra)),
      dec: commonValueNum(selectedItems.map((f) => f.frame?.dec)),
      // FITS-header sexagesimal strings, kept separate from the numeric
      // ra/dec columns. Used to render a forensic secondary line on the
      // RA/DEC rows when the literal header string disagrees with what the
      // numeric (plate-solved) value would format to.
      objctra: commonValueStr(selectedItems.map((f) => f.frame?.objctra)),
      objctdec: commonValueStr(selectedItems.map((f) => f.frame?.objctdec)),
      rotation: commonValueNum(selectedItems.map((f) => f.frame?.rotation)),
    };
  }, [selectedItems]);

  // Edit state per field. Reset whenever selection changes.
  const initialEdits = useCallback(
    (): Record<string, EditState> => ({
      object: { enabled: false, value: common.object === VARIES ? '' : common.object },
      filter: { enabled: false, value: common.filter === VARIES ? '' : common.filter },
      imagetyp: { enabled: false, value: common.imagetyp === VARIES ? '' : common.imagetyp },
      instrume: { enabled: false, value: common.instrume === VARIES ? '' : common.instrume },
      telescop: { enabled: false, value: common.telescop === VARIES ? '' : common.telescop },
      focallen: { enabled: false, value: common.focallen === VARIES ? '' : common.focallen },
      xpixsz: { enabled: false, value: common.xpixsz === VARIES ? '' : common.xpixsz },
      gain: { enabled: false, value: common.gain === VARIES ? '' : common.gain },
      offset: { enabled: false, value: common.offset === VARIES ? '' : common.offset },
      binning: { enabled: false, value: common.binning === VARIES ? '' : common.binning },
      exptime: { enabled: false, value: common.exptime === VARIES ? '' : common.exptime },
      ccdTemp: { enabled: false, value: common.ccdTemp === VARIES ? '' : common.ccdTemp },
      dateObs: { enabled: false, value: common.dateObs === VARIES ? '' : common.dateObs },
      // ra/dec are populated only by the ↺ revert handler — no text input.
      ra: { enabled: false, value: '' },
      dec: { enabled: false, value: '' },
      rotation: { enabled: false, value: common.rotation === VARIES ? '' : common.rotation },
    }),
    [common],
  );
  const [edits, setEdits] = useState<Record<string, EditState>>(initialEdits);
  const [saving, setSaving] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const [pendingPayload, setPendingPayload] = useState<Record<string, unknown> | null>(null);
  const [pendingRelations, setPendingRelations] = useState<FrameMetadataRelations | null>(null);
  const [memberships, setMemberships] = useState<FrameMembershipsSummary | null>(null);
  const [originals, setOriginals] = useState<FrameOriginalSnapshot[] | null>(null);

  // Plate-solve goes through the global queue at usePlateSolveQueue (mounted
  // via PlateSolveProgressProvider in Layout.tsx) — same pipeline as the
  // missing-metadata "Plate Solve" toolbar button. The panel below the field
  // list owns the progress bar, the per-frame solve/fail counts, the
  // completion banner, and the cancel button. We trigger it via the
  // imperative `start()` handle from a normal button so the layout stays
  // tight (panel renders nothing when idle).
  const plateSolveRef = useRef<PlateSolveBatchPanelHandle>(null);

  // Reset edit state ONLY when the actual selection changes — keyed by the
  // sorted frame_ids. Including `initialEdits` in the deps was a bug: that
  // callback's identity flips whenever `common` changes, which happens on
  // every render where the parent passes a fresh `otherListing` ref. The
  // result was that every keystroke nuked the edits state via the parent
  // re-render before React could commit the typed value. selectionKey is
  // a stable string keyed on the actual selection.
  const selectionKey = frameIds.slice().sort().join(',');
  // initialEdits is captured via a ref so the latest version is read inside
  // the effect without making it a dep (and re-firing the reset).
  const initialEditsRef = useRef(initialEdits);
  initialEditsRef.current = initialEdits;
  useEffect(() => {
    setEdits(initialEditsRef.current());
    setSavedAt(null);
    // Reset the "last common" baseline so the next sync effect doesn't
    // think the user manually typed the post-selection initial values.
    lastCommonRef.current = null;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectionKey]);

  // Sync the input values from `common` when the catalog data refreshes
  // *without* the selection changing — e.g. after a plate-solve writes
  // OBJECT/RA/DEC, or any other parent-driven save. Without this, the input
  // boxes are pinned to the value `initialEdits` captured at selection time
  // and would never reflect the new catalog state. We preserve user typing
  // by skipping any field whose current input value diverges from the
  // previously-observed common value (i.e. the user has changed it).
  const lastCommonRef = useRef<typeof common | null>(null);
  useEffect(() => {
    const last = lastCommonRef.current;
    lastCommonRef.current = common;
    if (last == null) return; // first render after a selection — initialEdits already populated
    setEdits(prev => {
      let next = prev;
      for (const k of Object.keys(common) as Array<keyof typeof common>) {
        const ks = k as string;
        const cur = prev[ks];
        if (!cur || cur.enabled) continue;
        const lastV = last[k] === VARIES ? '' : String(last[k] ?? '');
        // imagetyp's edit state stores UPPERCASE+collapsed-spaces (matches
        // the dropdown options), so compare against the same form.
        const lastFinal = k === 'imagetyp' ? lastV.toUpperCase().replace(/\s+/g, '') : lastV;
        if (cur.value !== lastFinal) continue; // user typed something — leave alone
        const newV = common[k] === VARIES ? '' : String(common[k] ?? '');
        const newFinal = k === 'imagetyp' ? newV.toUpperCase().replace(/\s+/g, '') : newV;
        if (cur.value === newFinal) continue;
        if (next === prev) next = { ...prev };
        next[ks] = { ...cur, value: newFinal };
      }
      return next;
    });
  }, [common]);

  // Fetch memberships whenever the selection changes. Guarded against
  // out-of-order resolution via a generation counter.
  const memberGenRef = useRef(0);
  useEffect(() => {
    const ids = frameIds;
    if (ids.length === 0) {
      setMemberships(null);
      return;
    }
    const gen = ++memberGenRef.current;
    let cancelled = false;
    api
      .invoke<FrameMembershipsSummary>('get_frame_memberships', { frameIds: ids })
      .then((s) => {
        if (cancelled || gen !== memberGenRef.current) return;
        setMemberships(s);
      })
      .catch((e) => {
        console.error('get_frame_memberships failed:', e);
        if (!cancelled) setMemberships(null);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectionKey]);

  // Single-frame plate-solve record (read-only view). Fetched only when
  // exactly one frame is selected — multi-select aggregation would just
  // be noise here. Mirrors the memberships generation-counter guard so
  // stale fetches don't clobber newer state.
  const [plateSolve, setPlateSolve] = useState<PlateSolveRecord | null>(null);
  const plateSolveGenRef = useRef(0);

  // Per-camera XPIXSZ default — inline action shown when the selected
  // frame's header lacks XPIXSZ but has INSTRUME, so focallen cannot be
  // algebraically derived from the solved arcsec/px. The user supplies the
  // missing input and we persist it under `PlateSolveConfig.camera_defaults`
  // (next solve fills focallen for any frame from this camera).
  const [cdInput, setCdInput] = useState<string>('');
  const [cdSaving, setCdSaving] = useState(false);
  const [cdSavedAt, setCdSavedAt] = useState<number | null>(null);
  useEffect(() => {
    setCdInput('');
    setCdSavedAt(null);
  }, [selectionKey]);

  const saveCameraDefault = useCallback(async () => {
    const sel = selectedItems[0];
    const instrume = sel?.frame?.instrume?.trim();
    if (!instrume) return;
    const xpixsz_um = parseFloat(cdInput);
    if (!Number.isFinite(xpixsz_um) || xpixsz_um <= 0) return;
    setCdSaving(true);
    try {
      // Atomic-from-user-POV: load → patch → save. The backend round-trips
      // the full config, so unknown fields the TS interface doesn't model
      // are preserved by JSON.stringify.
      const cfg = await api.invoke<PlateSolveConfig>('get_plate_solve_config');
      const updated: PlateSolveConfig = {
        ...cfg,
        camera_defaults: { ...(cfg.camera_defaults ?? {}), [instrume]: xpixsz_um },
      };
      await api.invoke('set_plate_solve_config', { config: updated });
      setCdSavedAt(Date.now());
      setCdInput('');
    } catch (e) {
      console.error('set camera default failed:', e);
    } finally {
      setCdSaving(false);
    }
  }, [selectedItems, cdInput]);
  useEffect(() => {
    const ids = frameIds;
    if (ids.length !== 1) {
      setPlateSolve(null);
      return;
    }
    const gen = ++plateSolveGenRef.current;
    let cancelled = false;
    api
      .invoke<PlateSolveRecord | null>('get_plate_solve_result', { frameId: ids[0] })
      .then((rec) => {
        if (cancelled || gen !== plateSolveGenRef.current) return;
        setPlateSolve(rec);
      })
      .catch((e) => {
        console.error('get_plate_solve_result failed:', e);
        if (!cancelled) setPlateSolve(null);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectionKey]);

  // Originals fetch — only fires when at least one selected frame has the
  // override flag set, so panes with no edits don't pay the parsing cost.
  const anyOverridden = useMemo(
    () => selectedItems.some((it) => it.frame?.override_ === true),
    [selectedItems],
  );
  const originalsGenRef = useRef(0);
  useEffect(() => {
    if (!anyOverridden || frameIds.length === 0) {
      setOriginals(null);
      return;
    }
    const gen = ++originalsGenRef.current;
    let cancelled = false;
    api
      .invoke<FrameOriginalSnapshot[]>('get_frame_metadata_originals', { frameIds })
      .then((s) => {
        if (cancelled || gen !== originalsGenRef.current) return;
        setOriginals(s);
      })
      .catch((e) => {
        console.error('get_frame_metadata_originals failed:', e);
        if (!cancelled) setOriginals(null);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectionKey, anyOverridden]);

  /** Aggregate the originals across the selection: returns the common value
   *  for `field` if all snapshots agree, VARIES otherwise, '' if empty. */
  const originalForField = useCallback(
    (field: keyof FrameOriginalSnapshot): string => {
      if (!originals || originals.length === 0) return '';
      const values = originals.map((o) => {
        const v = o[field];
        if (v == null) return '';
        return String(v);
      });
      const set = new Set(values);
      if (set.size === 1) return Array.from(set)[0] ?? '';
      return VARIES;
    },
    [originals],
  );

  /** Pre-fill an editor row with the original value and enable its apply
   *  checkbox so a single click of "Apply changes" reverts the field.
   *  The IMAGETYP dropdown is keyed on uppercase strings (LIGHT / MASTERDARK /
   *  …) but the snapshot returns the canonical PascalCase form (Light /
   *  MasterDark / …) — normalize so the dropdown actually selects the
   *  reverted option. */
  const revertField = useCallback(
    (key: string, fieldOnSnapshot: keyof FrameOriginalSnapshot) => {
      const orig = originalForField(fieldOnSnapshot);
      if (orig === VARIES) {
        alert(
          'Original values differ across the selection — revert one frame at a time.',
        );
        return;
      }
      let revertValue = orig;
      if (key === 'imagetyp') {
        revertValue = orig.toUpperCase().replace(/\s+/g, '');
      }
      setEdits((prev) => ({
        ...prev,
        [key]: { enabled: true, value: revertValue },
      }));
    },
    [originalForField],
  );

  const setField = (key: string, patch: Partial<EditState>) => {
    setEdits((prev) => ({ ...prev, [key]: { ...prev[key], ...patch } }));
  };

  /** True when at least one field's "apply" checkbox is enabled. Used to
   *  grey out the Apply button when there's nothing to save. */
  const hasPendingChanges = Object.values(edits).some((e) => e.enabled);

  const onSave = async () => {
    if (frameIds.length === 0) return;
    const payload: Record<string, unknown> = {};
    for (const [k, e] of Object.entries(edits)) {
      if (!e.enabled) continue;
      const trimmed = e.value.trim();

      // Numeric fields with bounded ranges. Empty string clears the value.
      const numericFields: Record<string, { label: string; min?: number; max?: number; positive?: boolean }> = {
        focallen: { label: 'Focal length', min: 0, max: 100000 },
        xpixsz:   { label: 'Pixel size',   min: 0, max: 100, positive: true },
        gain:     { label: 'Gain',         min: 0, max: 1000 },
        offset:   { label: 'Offset',       min: 0, max: 100000 },
        exptime:  { label: 'Exposure',     positive: true },
        ccdTemp:  { label: 'CCD temp',     min: -100, max: 100 },
        // RA/DEC are decimal degrees. Only set by the revert path — never
        // typed in. Backend `bulk_update_frame_metadata` validates the same
        // ranges and also writes the parallel sexagesimal OBJCTRA/OBJCTDEC.
        ra:       { label: 'RA',           min: 0,    max: 360 },
        dec:      { label: 'DEC',          min: -90,  max: 90 },
        rotation: { label: 'Rotation',     min: -360, max: 360 },
      };
      if (numericFields[k]) {
        const cfg = numericFields[k];
        // Wire payload uses camelCase (matches backend serde rename_all).
        // The local edit-state key already is camelCase ("ccdTemp", etc.).
        if (trimmed === '') {
          payload[k] = null;
          continue;
        }
        const n = Number(trimmed);
        if (!Number.isFinite(n)) {
          alert(`${cfg.label} must be a number.`);
          return;
        }
        if (cfg.positive && n <= 0) {
          alert(`${cfg.label} must be greater than 0.`);
          return;
        }
        if (cfg.min !== undefined && n < cfg.min) {
          alert(`${cfg.label} must be ≥ ${cfg.min}.`);
          return;
        }
        if (cfg.max !== undefined && n > cfg.max) {
          alert(`${cfg.label} must be ≤ ${cfg.max}.`);
          return;
        }
        payload[k] = n;
      } else if (k === 'binning') {
        if (trimmed === '') {
          payload.binning = null;
          continue;
        }
        if (!/^\d+x\d+$/i.test(trimmed)) {
          alert('Binning must be in "AxB" form (e.g. "1x1", "2x2").');
          return;
        }
        const [xs, ys] = trimmed.toLowerCase().split('x');
        const xb = Number(xs);
        const yb = Number(ys);
        if (xb < 1 || yb < 1 || xb > 16 || yb > 16) {
          alert('Binning components must be between 1 and 16.');
          return;
        }
        payload.binning = `${xb}x${yb}`;
      } else {
        payload[k] = trimmed === '' ? null : trimmed;
      }
    }
    if (Object.keys(payload).length === 0) return;

    // Probe for relations the cascade will sever. Any non-zero count opens
    // a warning dialog before we actually write.
    setSaving(true);
    let relations: FrameMetadataRelations;
    try {
      relations = await api.invoke<FrameMetadataRelations>(
        'count_frame_metadata_relations',
        { frameIds },
      );
    } catch (e) {
      console.error('count_frame_metadata_relations failed:', e);
      setSaving(false);
      alert(`Could not check relations: ${e}`);
      return;
    }

    if (relations.affectedFrameCount > 0) {
      setPendingPayload(payload);
      setPendingRelations(relations);
      setSaving(false);
      return;
    }
    await commitSave(payload);
  };

  const commitSave = useCallback(
    async (payload: Record<string, unknown>) => {
      setSaving(true);
      try {
        await api.invoke<number>('bulk_update_frame_metadata', {
          frameIds,
          edits: payload,
        });
        setSavedAt(Date.now());
        onSaved();
      } catch (e) {
        console.error('bulk_update_frame_metadata failed:', e);
        alert(`Save failed: ${e}`);
      } finally {
        setSaving(false);
      }
    },
    [frameIds, onSaved],
  );

  const onConfirmUnlink = useCallback(async () => {
    if (!pendingPayload) return;
    const payload = pendingPayload;
    setPendingPayload(null);
    setPendingRelations(null);
    await commitSave(payload);
  }, [pendingPayload, commitSave]);

  const onCancelUnlink = useCallback(() => {
    setPendingPayload(null);
    setPendingRelations(null);
  }, []);

  if (selectedItems.length === 0) {
    return (
      <div className="p-4 text-sm text-content-muted">
        Select files in the other pane to view and edit their metadata.
      </div>
    );
  }

  // RA / DEC and the plate-solve action only make sense for on-sky light
  // frames. For calibration frames (Dark / Flat / Bias / DarkFlat and their
  // masters) the coordinates are meaningless, so we hide both. If the user
  // is in the middle of changing IMAGETYP, treat the pending value as the
  // effective type — flipping a calibration frame to LIGHT in the edit row
  // immediately re-exposes RA/DEC + Plate Solve.
  const effectiveImagetypIsLight = (() => {
    const editState = edits.imagetyp;
    if (editState?.enabled && typeof editState.value === 'string' && editState.value !== '') {
      return isLightLike(editState.value);
    }
    return selectedItems.some((it) => isLightLike(it.frame?.imagetyp ?? null));
  })();

  /** Field row spec — numeric, free-text, or dropdown. `originalKey` maps
   *  the field to the matching `FrameOriginalSnapshot` field for the revert
   *  button (omit for fields the snapshot doesn't carry). */
  const fields: Array<{
    key: keyof typeof common;
    label: string;
    numeric?: boolean;
    step?: string;
    min?: number;
    max?: number;
    placeholder?: string;
    maxLength?: number;
    /** When set, renders a <select> with these options instead of an <input>. */
    options?: string[];
    /** Snapshot key — when present a 🔄 revert button appears for overridden frames. */
    originalKey?: keyof FrameOriginalSnapshot;
    /** RA/DEC: render as read-only, no input/checkbox. The only
     *  way to write a new value is plate-solving (see button below the
     *  field rows). The ↺ revert button still works — it pre-fills the edit
     *  state with the FITS-header decimal value so a normal save restores
     *  the original. */
    coordinate?: 'ra' | 'dec';
    /** Generic read-only flag. Same render path as `coordinate` (no input,
     *  no checkbox, just a value + optional revert) but without the numeric
     *  comparison logic — used for the literal OBJCTRA/OBJCTDEC strings
     *  which compare as plain strings. */
    readOnly?: boolean;
  }> = [
    { key: 'object',   label: 'OBJECT',           maxLength: 64, originalKey: 'object' },
    { key: 'filter',   label: 'FILTER',           maxLength: 32, originalKey: 'filter' },
    {
      key: 'imagetyp',
      label: 'IMAGETYP',
      options: [
        'LIGHT',
        'DARK',
        'FLAT',
        'BIAS',
        'DARKFLAT',
        'MASTERLIGHT',
        'MASTERDARK',
        'MASTERFLAT',
        'MASTERBIAS',
        'MASTERDARKFLAT',
      ],
      originalKey: 'imagetyp',
    },
    { key: 'instrume', label: 'CAMERA (INSTRUME)', maxLength: 64, originalKey: 'instrume' },
    { key: 'telescop', label: 'TELESCOP',         maxLength: 64, originalKey: 'telescop' },
    { key: 'focallen', label: 'FOCALLEN (mm)',    numeric: true, step: '0.1', min: 0,    max: 100000, originalKey: 'focallen' },
    { key: 'gain',     label: 'GAIN',             numeric: true, step: '1',   min: 0,    max: 1000,    originalKey: 'gain' },
    { key: 'offset',   label: 'OFFSET',           numeric: true, step: '1',   min: 0,    max: 100000,  originalKey: 'offset' },
    { key: 'xpixsz',   label: 'XPIXSZ (µm)',      numeric: true, step: '0.01', min: 0,   max: 100,     originalKey: 'xpixsz' },
    {
      key: 'binning',
      label: 'BINNING',
      options: ['1x1', '2x2', '3x3', '4x4', '5x5', '6x6'],
      originalKey: 'binning',
    },
    { key: 'exptime',  label: 'EXPTIME (s)',      numeric: true, step: '0.001', min: 0.001, originalKey: 'exptime' },
    { key: 'ccdTemp',  label: 'CCD-TEMP (°C)',    numeric: true, step: '0.1',   min: -100, max: 100, originalKey: 'ccdTemp' },
    { key: 'dateObs',  label: 'DATE-OBS (ISO)',   maxLength: 32, placeholder: 'YYYY-MM-DDTHH:MM:SS', originalKey: 'dateObs' },
    ...(effectiveImagetypIsLight
      ? [
          { key: 'ra' as const,       label: 'RA',       originalKey: 'ra' as const,  coordinate: 'ra' as const },
          // OBJCTRA/OBJCTDEC: read-only literal strings from the catalog.
          // No `originalKey` because the FITS-header snapshot only stores
          // parsed numeric ra/dec — reverting RA already re-derives OBJCTRA
          // on save, so a separate revert here would be redundant.
          { key: 'objctra' as const,  label: 'OBJCTRA',  readOnly: true },
          { key: 'dec' as const,      label: 'DEC',      originalKey: 'dec' as const, coordinate: 'dec' as const },
          { key: 'objctdec' as const, label: 'OBJCTDEC', readOnly: true },
        ]
      : []),
    { key: 'rotation', label: 'ROTATION (°)',     numeric: true, step: '0.1', min: -360, max: 360, originalKey: 'rotation' },
  ];

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-y-auto p-4 text-sm space-y-4">
        <div className="text-content-muted">
          {selectedItems.length} file{selectedItems.length === 1 ? '' : 's'} selected
          {frameIds.length < selectedItems.length && (
            <span className="ml-2 text-amber-400">
              ({selectedItems.length - frameIds.length} without catalog metadata)
            </span>
          )}
        </div>

        {anyOverridden && (
          <div className="text-xs text-amber-400/90 bg-amber-400/10 border border-amber-400/30 rounded px-2 py-1.5 flex items-center gap-2">
            <AlertTriangle size={12} />
            {(() => {
              const overridden = selectedItems.filter((it) => it.frame?.override_).length;
              return overridden === selectedItems.length
                ? 'Custom metadata applied. Click ↺ next to a field to revert it to the value from the original file header.'
                : `${overridden} of ${selectedItems.length} frames have custom metadata. Originals shown below; revert per field with ↺.`;
            })()}
          </div>
        )}

        <div className="space-y-2">
          {fields.map((f) => {
            const cv = common[f.key];
            const e = edits[f.key as string];
            const origVal = f.originalKey && originals
              ? originalForField(f.originalKey)
              : '';
            // Only surface the original / ↺ revert button when the original
            // actually differs from the current value. If they match, this
            // field wasn't touched (or was reverted) and showing the marker
            // would just be visual clutter. Compare as strings — both common
            // and originalForField produce string-form values via the same
            // shape (numbers get String(v)), so exact equality works for
            // the cases the user cares about. VARIES on either side is
            // inherently uncertain, so we treat it as "show" so the user
            // still sees the per-frame originals on hover.
            const currentStr = String(cv ?? '');
            // For RA/DEC the underlying values are decimal degrees — show
            // them verbatim so the user sees exactly what the catalog has.
            // We still compute a numeric `coordsMatch` (±1e-6) so the revert
            // marker doesn't fire on display-only rounding noise between the
            // catalog value and the FITS-header snapshot.
            const displayCurrent = display(cv);
            const displayOriginal = display(origVal);
            let coordsMatch: boolean | null = null;
            if (
              f.coordinate &&
              cv !== VARIES && currentStr !== '' &&
              origVal !== VARIES && origVal !== ''
            ) {
              const cn = Number(currentStr);
              const on = Number(origVal);
              if (Number.isFinite(cn) && Number.isFinite(on)) {
                coordsMatch = Math.abs(cn - on) < 1e-6;
              }
            }
            // For DATE-OBS the catalog has been through parse-and-reformat
            // (RFC3339 with `Z`) while the snapshot has the raw FITS string
            // (often no timezone marker). Compare them as instants instead
            // of strings so identical timestamps don't show as a diff.
            const semanticEqual =
              f.coordinate
                ? coordsMatch === true
                : f.key === 'dateObs'
                  ? sameDateInstant(currentStr, origVal)
                  : origVal === currentStr;
            const valuesMatch =
              origVal !== VARIES &&
              currentStr !== VARIES &&
              semanticEqual;
            // Show the revert affordance whenever originals have actually
            // loaded (not just `origVal !== ''` — that was conflating "no
            // originals fetched yet" with "FITS header had no value for this
            // keyword", so auto-filled-from-null fields like Find Object's
            // OBJECT couldn't be reverted back to empty).
            const showRevert =
              anyOverridden &&
              f.originalKey != null &&
              originals != null &&
              origVal !== VARIES &&
              !valuesMatch;
            // Three-column row layout:
            //   - Left (140px): label only.
            //   - Middle (1fr): current value (and original when overridden).
            //     `break-all` lets long timestamps and sexagesimal RA/DEC
            //     wrap if the column gets squeezed instead of truncating
            //     with an ellipsis.
            //   - Right (320px fixed): checkbox + input + revert. Capped
            //     width so short numeric values (gain=56, offset=30) don't
            //     get a half-screen-wide input box, and so the middle
            //     column always has room to display the current value.
            return (
              <div key={f.key as string} className="grid grid-cols-[140px_1fr_320px] gap-3 items-center">
                <label className="text-xs font-mono text-content-muted">{f.label}</label>
                <div
                  className={`text-xs font-mono break-all min-w-0 ${cv === VARIES ? 'text-amber-400' : 'text-content/80'}`}
                  title={displayCurrent + (showRevert ? `  ·  original: ${displayOriginal}` : '')}
                >
                  {displayCurrent}
                  {showRevert && (
                    <span className="ml-1 text-amber-400/70 text-[10px]" title={`Original: ${displayOriginal}`}>
                      ↺ {displayOriginal}
                    </span>
                  )}
                </div>
                {(f.coordinate || f.readOnly) ? (
                  // Coordinate rows: read-only. New values come from plate-
                  // solving (button below the field list); the only direct
                  // affordance here is the ↺ revert button when the FITS
                  // header had a value to revert to.
                  <div className="flex items-center gap-2 justify-end">
                    {showRevert && f.originalKey && (
                      <button
                        type="button"
                        onClick={() => revertField(f.key as string, f.originalKey!)}
                        title={`Revert to original (${displayOriginal})`}
                        className="p-1 rounded hover:bg-surface-hover text-amber-400 flex-shrink-0"
                      >
                        <RotateCcw size={12} />
                      </button>
                    )}
                  </div>
                ) : (
                <div className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={e?.enabled ?? false}
                    onChange={(ev) => setField(f.key as string, { enabled: ev.target.checked })}
                    title="Include this field when applying changes"
                    className="accent-accent flex-shrink-0"
                  />
                  {f.options ? (
                    <select
                      value={e?.value ?? ''}
                      onChange={(ev) =>
                        setField(f.key as string, {
                          value: ev.target.value,
                          // Selecting the placeholder ("") clears the apply
                          // flag; selecting any real option enables it.
                          enabled: ev.target.value !== '',
                        })
                      }
                      className="flex-1 min-w-0 px-2 py-1 rounded border border-border bg-surface text-content text-xs font-mono outline-none focus:border-accent"
                    >
                      <option value="">— no change —</option>
                      {f.options.map((opt) => (
                        <option key={opt} value={opt}>
                          {opt}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <input
                      type={f.numeric ? 'number' : 'text'}
                      inputMode={f.numeric ? 'decimal' : undefined}
                      step={f.step}
                      min={f.min}
                      max={f.max}
                      maxLength={f.maxLength}
                      value={e?.value ?? ''}
                      // Typing auto-enables the "apply" checkbox for this field.
                      // Clearing back to empty leaves the checkbox checked so
                      // the user can explicitly null the value out on save.
                      onChange={(ev) =>
                        setField(f.key as string, { value: ev.target.value, enabled: true })
                      }
                      placeholder={
                        f.placeholder ??
                        (cv === VARIES ? '(varies — type to set)' : '(no change)')
                      }
                      className="flex-1 min-w-0 px-2 py-1 rounded border border-border bg-surface text-content text-xs font-mono outline-none focus:border-accent"
                    />
                  )}
                  {showRevert && f.originalKey && (
                    <button
                      type="button"
                      onClick={() => revertField(f.key as string, f.originalKey!)}
                      title={`Revert to original (${displayOriginal})`}
                      className="p-1 rounded hover:bg-surface-hover text-amber-400 flex-shrink-0"
                    >
                      <RotateCcw size={12} />
                    </button>
                  )}
                </div>
                )}
              </div>
            );
          })}
        </div>

        {/* Coordinates can only be (re)written by plate-solving — there is no
            text input for them. The button enqueues every selected frame
            through the global plate-solve queue (PlateSolveProgressProvider).
            The panel directly below renders progress / completion / errors,
            and the global PlateSolveQueueIndicator picks the batch up too.
            On completion we fire `onSaved` so this pane re-loads frame
            values + originals. Hidden when the selection has no light-like
            frames — calibration frames don't need plate solving. */}
        {frameIds.length > 0 && effectiveImagetypIsLight && (
          <div className="space-y-2">
            <div className="flex items-center justify-between gap-3 rounded-md border border-border bg-surface-elevated/40 px-3 py-2">
              <div className="text-xs text-content-muted">
                <span className="text-content font-mono">RA / DEC</span> are written by plate solving.
              </div>
              <button
                type="button"
                onClick={() => {
                  if (frameIds.length === 0) return;
                  plateSolveRef.current?.start(frameIds);
                }}
                disabled={saving || frameIds.length === 0}
                title="Enqueue every selected frame on the global plate-solve queue. Progress shows here and in the sidebar queue indicator."
                className="flex items-center gap-2 px-2.5 py-1 rounded bg-surface hover:bg-surface-hover border border-border text-content text-xs disabled:opacity-40 disabled:cursor-not-allowed"
              >
                <Crosshair size={12} />
                Plate Solve {frameIds.length} frame{frameIds.length === 1 ? '' : 's'}
              </button>
            </div>
            <PlateSolveBatchPanel
              ref={plateSolveRef}
              hideTriggerButtons
              onSolveComplete={onSaved}
            />
            {/* Read-only solved-WCS summary for the single-selection case.
                Surfaces the exact `plate_solves.pixel_scale_arcsec` (and the
                rest of the WCS) for frames whose FITS header lacks XPIXSZ
                or FOCALLEN — in that case `frames.focallen` legitimately
                stays NULL (the algebra needs both), but the solved scale
                is still known precisely and worth showing. */}
            {plateSolve && (
              <div className="rounded-md border border-border bg-surface-elevated/40 px-3 py-2">
                <div className="text-xs text-content-muted mb-1">Plate-solve result</div>
                <div className="grid grid-cols-2 gap-x-4 gap-y-1 text-xs">
                  <div>
                    <span className="text-content-muted">Pixel scale: </span>
                    <span className="font-mono text-content">
                      {plateSolve.pixel_scale_arcsec.toFixed(3)}″/px
                    </span>
                  </div>
                  <div>
                    <span className="text-content-muted">Rotation: </span>
                    <span className="font-mono text-content">
                      {plateSolve.field_rotation_deg.toFixed(2)}°
                    </span>
                  </div>
                  <div>
                    <span className="text-content-muted">Centre RA: </span>
                    <span className="font-mono text-content">
                      {plateSolve.crval1.toFixed(4)}°
                    </span>
                  </div>
                  <div>
                    <span className="text-content-muted">Centre Dec: </span>
                    <span className="font-mono text-content">
                      {plateSolve.crval2.toFixed(4)}°
                    </span>
                  </div>
                  <div>
                    <span className="text-content-muted">RMS: </span>
                    <span className="font-mono text-content">
                      {plateSolve.rms_residual_arcsec.toFixed(2)}″ ({plateSolve.rms_residual_px.toFixed(2)}px)
                    </span>
                  </div>
                  <div>
                    <span className="text-content-muted">Inliers: </span>
                    <span className="font-mono text-content">
                      {plateSolve.matched_stars}
                      {plateSolve.expected_catalog_stars_in_fov != null
                        ? ` / ${plateSolve.expected_catalog_stars_in_fov}`
                        : ''}
                    </span>
                  </div>
                  <div className="col-span-2">
                    <span className="text-content-muted">Solved: </span>
                    <span className="font-mono text-content">
                      {formatTimestamp(plateSolve.solved_at)}
                    </span>
                    <span className="text-content-muted">
                      {' '}via {plateSolve.algorithm_used} ({plateSolve.catalog_used})
                    </span>
                  </div>
                </div>
              </div>
            )}
            {/* Per-camera XPIXSZ default — only when the user has algebra
                blocking the focallen back-fill (header lacks XPIXSZ but
                INSTRUME is set). Saving stores the default in
                PlateSolveConfig.camera_defaults; next solve fills focallen
                for this camera. */}
            {plateSolve &&
              selectedItems.length === 1 &&
              selectedItems[0].frame?.xpixsz == null &&
              selectedItems[0].frame?.instrume?.trim() && (
                <div className="rounded-md border border-border bg-surface-elevated/40 px-3 py-2 text-xs">
                  <div className="text-content-muted mb-2">
                    Frame's <span className="font-mono">XPIXSZ</span> is missing — focallen can't
                    be derived from arcsec/px alone. Set a default for{' '}
                    <span className="font-mono text-content">
                      {selectedItems[0].frame!.instrume!.trim()}
                    </span>
                    {' '}so future solves fill it automatically.
                  </div>
                  <div className="flex items-center gap-2">
                    <input
                      type="number"
                      step="0.01"
                      min="0"
                      placeholder="XPIXSZ µm"
                      value={cdInput}
                      onChange={(e) => setCdInput(e.target.value)}
                      disabled={cdSaving}
                      className="flex-1 px-2 py-1 rounded bg-surface border border-border text-content text-xs font-mono disabled:opacity-50"
                    />
                    <button
                      type="button"
                      onClick={saveCameraDefault}
                      disabled={
                        cdSaving ||
                        !Number.isFinite(parseFloat(cdInput)) ||
                        parseFloat(cdInput) <= 0
                      }
                      className="px-2.5 py-1 rounded bg-accent text-white hover:bg-accent-hover disabled:opacity-40 disabled:cursor-not-allowed text-xs"
                    >
                      {cdSaving ? 'Saving…' : 'Save default'}
                    </button>
                    {cdSavedAt && Date.now() - cdSavedAt < 4000 && (
                      <span className="text-green-500">Saved.</span>
                    )}
                  </div>
                </div>
              )}
          </div>
        )}

        <MembershipsPanel memberships={memberships} />
      </div>

      <div className="border-t border-border p-3 flex items-center justify-between">
        <div className="text-xs text-content-muted">
          {savedAt && Date.now() - savedAt < 4000 && (
            <span className="text-green-500">Saved.</span>
          )}
        </div>
        <button
          onClick={onSave}
          disabled={saving || frameIds.length === 0 || !hasPendingChanges}
          title={!hasPendingChanges ? 'Tick a field checkbox to enable an edit first' : undefined}
          className="flex items-center gap-2 px-3 py-1.5 rounded bg-accent text-white hover:bg-accent-hover disabled:opacity-40 disabled:cursor-not-allowed disabled:hover:bg-accent text-sm"
        >
          {saving ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}
          Apply changes
        </button>
      </div>

      {pendingRelations && (
        <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/50" onClick={onCancelUnlink}>
          <div
            className="bg-surface-elevated border border-amber-600 rounded-md p-5 max-w-md w-full shadow-xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center gap-2 mb-3 text-amber-400 font-semibold">
              <AlertTriangle size={18} />
              Editing will unlink these frames
            </div>
            <p className="text-sm text-content-muted mb-3">
              Saving these metadata changes will remove existing relations for{' '}
              <span className="text-content font-mono">{pendingRelations.affectedFrameCount}</span>{' '}
              frame{pendingRelations.affectedFrameCount === 1 ? '' : 's'}. The catalog will lose:
            </p>
            <ul className="text-sm text-content space-y-1 mb-4 list-disc list-inside">
              {pendingRelations.calibrationSetMemberCount > 0 && (
                <li>
                  <span className="font-mono">{pendingRelations.calibrationSetMemberCount}</span>{' '}
                  membership(s) in calibration sets
                </li>
              )}
              {pendingRelations.calibrationSetConsumerCount > 0 && (
                <li>
                  <span className="font-mono">{pendingRelations.calibrationSetConsumerCount}</span>{' '}
                  link(s) to a consumed calibration set
                </li>
              )}
              {pendingRelations.sessionMemberCount > 0 && (
                <li>
                  <span className="font-mono">{pendingRelations.sessionMemberCount}</span>{' '}
                  session/imaging-night/frame-set placement(s)
                </li>
              )}
            </ul>
            <p className="text-xs text-content-muted mb-4">
              The calibration sets and frame sets themselves are kept — only this frame's
              membership is removed. You can rebuild relations later via the existing tools.
            </p>
            <div className="flex gap-2 justify-end">
              <button
                onClick={onCancelUnlink}
                className="px-3 py-1.5 rounded border border-border hover:bg-surface text-sm"
              >
                Cancel
              </button>
              <button
                onClick={onConfirmUnlink}
                className="px-3 py-1.5 rounded bg-amber-600 text-white hover:bg-amber-700 text-sm"
              >
                Apply and unlink
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** "Memberships" panel — shows which frame sets / calibration sets the
 *  selection is part of and which calibration sets it consumes. */
function MembershipsPanel({ memberships }: { memberships: FrameMembershipsSummary | null }) {
  const navigate = useNavigate();
  const goToObject = (frameSetId: number) => navigate(`/objects/${frameSetId}`);
  if (!memberships) {
    return (
      <div className="border-t border-border pt-3 text-xs text-content-muted">
        Memberships: loading…
      </div>
    );
  }
  const {
    frameSets,
    calibrationSetMember,
    calibrationSetConsumer,
    usedInFrameSets,
    frameCountTotal,
    frameCountUnaffiliated,
  } = memberships;
  const hasAny =
    frameSets.length > 0 ||
    calibrationSetMember.length > 0 ||
    calibrationSetConsumer.length > 0 ||
    usedInFrameSets.length > 0;
  return (
    <div className="border-t border-border pt-3 space-y-3">
      <div className="text-xs font-semibold text-content uppercase tracking-wide">Memberships</div>
      {!hasAny ? (
        <div className="text-xs text-content-muted">
          {frameCountUnaffiliated === frameCountTotal
            ? 'None of the selected frames are in any frame set or calibration set.'
            : 'No memberships found.'}
        </div>
      ) : (
        <>
          {frameSets.length > 0 && (
            <div>
              <div className="text-xs text-content-muted mb-1">Frame sets (Objects):</div>
              <ul className="space-y-1">
                {frameSets.map((fs) => (
                  <FrameSetRow
                    key={fs.frameSetId}
                    entry={fs}
                    onOpen={() => goToObject(fs.frameSetId)}
                  />
                ))}
              </ul>
            </div>
          )}
          {frameCountUnaffiliated > 0 && frameSets.length > 0 && (
            <div className="text-xs text-content-muted italic">
              {frameCountUnaffiliated} frame{frameCountUnaffiliated === 1 ? '' : 's'} not in any frame set
            </div>
          )}
          {calibrationSetMember.length > 0 && (
            <div>
              <div className="text-xs text-content-muted mb-1">Calibration set member of:</div>
              <ul className="space-y-1">
                {calibrationSetMember.map((cs) => (
                  <CalibrationRow key={`m-${cs.calibrationSetId}`} entry={cs} />
                ))}
              </ul>
            </div>
          )}
          {usedInFrameSets.length > 0 && (
            <div>
              <div className="text-xs text-content-muted mb-1">Used to calibrate:</div>
              <ul className="space-y-1">
                {usedInFrameSets.map((u) => (
                  <UsedInFrameSetRow
                    key={`${u.frameSetId}-${u.calibrationType}`}
                    entry={u}
                    onOpen={() => goToObject(u.frameSetId)}
                  />
                ))}
              </ul>
            </div>
          )}
          {calibrationSetConsumer.length > 0 && (
            <div>
              <div className="text-xs text-content-muted mb-1">Uses calibration sets:</div>
              <ul className="space-y-1">
                {calibrationSetConsumer.map((cs) => (
                  <CalibrationRow
                    key={`c-${cs.calibrationSetId}-${cs.calibrationType ?? ''}`}
                    entry={cs}
                    showCalibrationType
                  />
                ))}
              </ul>
            </div>
          )}
        </>
      )}
    </div>
  );
}

function FrameSetRow({
  entry,
  onOpen,
}: {
  entry: FrameSetMembership;
  onOpen: () => void;
}) {
  return (
    <li>
      <button
        onClick={onOpen}
        title={`Open ${entry.name ?? `frame set #${entry.frameSetId}`}`}
        className="w-full flex items-baseline gap-3 text-xs text-left rounded px-1 py-0.5 hover:bg-surface-hover group"
      >
        <span className="font-mono text-content truncate flex-1 group-hover:text-accent">
          {entry.name ?? `(unnamed #${entry.frameSetId})`}
          <ExternalLink size={10} className="inline ml-1 opacity-0 group-hover:opacity-70" />
        </span>
        <span className="text-content-muted whitespace-nowrap">
          {entry.selectedCount} frame{entry.selectedCount === 1 ? '' : 's'}
        </span>
      </button>
    </li>
  );
}

function UsedInFrameSetRow({
  entry,
  onOpen,
}: {
  entry: UsedInFrameSetMembership;
  onOpen: () => void;
}) {
  return (
    <li>
      <button
        onClick={onOpen}
        title={`Open ${entry.name ?? `frame set #${entry.frameSetId}`}`}
        className="w-full flex items-baseline gap-3 text-xs text-left rounded px-1 py-0.5 hover:bg-surface-hover group"
      >
        <span className="font-mono text-content truncate flex-1 group-hover:text-accent">
          <span className="text-content-muted">{entry.calibrationType}:</span>{' '}
          {entry.name ?? `(unnamed #${entry.frameSetId})`}
          <ExternalLink size={10} className="inline ml-1 opacity-0 group-hover:opacity-70" />
        </span>
        <span className="text-content-muted whitespace-nowrap">
          calibrates {entry.usedForFrameCount} frame
          {entry.usedForFrameCount === 1 ? '' : 's'}
        </span>
      </button>
    </li>
  );
}

function CalibrationRow({
  entry,
  showCalibrationType,
}: {
  entry: CalibrationSetMembership;
  showCalibrationType?: boolean;
}) {
  // Build a compact, type-aware description. Different calibration kinds
  // care about different parameters (Bias has no exptime, Flat has filter
  // but no temp matter, etc.).
  const bits: string[] = [];
  if (entry.instrume) bits.push(entry.instrume);
  if (entry.binning) bits.push(entry.binning);
  if (entry.gain != null) bits.push(`gain ${entry.gain}`);
  if (entry.imagetyp.toLowerCase().includes('flat') && entry.filter) {
    bits.push(entry.filter);
  }
  if (
    !entry.imagetyp.toLowerCase().includes('bias') &&
    entry.exptime != null
  ) {
    bits.push(`${entry.exptime}s`);
  }
  if (entry.ccdTemp != null && entry.imagetyp.toLowerCase().includes('dark')) {
    bits.push(`${entry.ccdTemp}°C`);
  }
  // Date last
  if (entry.date) bits.push(entry.date);

  const label = showCalibrationType
    ? `${entry.calibrationType ?? entry.imagetyp}: ${entry.imagetyp}`
    : entry.imagetyp;
  return (
    <li className="flex items-baseline gap-3 text-xs">
      <span className="font-mono text-content truncate flex-1" title={bits.join(' · ')}>
        <span className="text-content-muted">{label}</span> {bits.length > 0 && '— '}
        {bits.join(' · ')}
      </span>
      <span className="text-content-muted whitespace-nowrap">
        {entry.selectedCount} frame{entry.selectedCount === 1 ? '' : 's'}
      </span>
    </li>
  );
}
