import { useCallback, useEffect, useState } from 'react';
import { api } from '../../api';
import type {
  CalibrationSetParameters,
  CalibrationSetWithScore,
  LightFrameParameters,
} from '../../types/models';
import {
  requirementFromLights,
  requirementFromSet,
  slotsFor,
  type PickerSubject,
  type Requirement,
  type SlotKind,
} from './pickerModel';

/** The wire name of a slot, as the assignment commands spell it. */
const WIRE_TYPE: Record<SlotKind, string> = {
  flat: 'Flat',
  darkflat: 'DarkFlat',
  dark: 'Dark',
  bias: 'Bias',
};

export interface PickerData {
  requirement: Requirement | null;
  /** Candidates per slot, in engine order (compatible first, near misses next). */
  candidates: Record<SlotKind, CalibrationSetWithScore[]>;
  /** What is linked right now, per slot — the "Current" badge. */
  current: Record<SlotKind, number | null>;
  loading: boolean;
  error: string | null;
  reload: () => void;
}

const NO_CANDIDATES: Record<SlotKind, CalibrationSetWithScore[]> = {
  flat: [],
  darkflat: [],
  dark: [],
  bias: [],
};
const NO_CURRENT: Record<SlotKind, number | null> = {
  flat: null,
  darkflat: null,
  dark: null,
  bias: null,
};

/**
 * Load everything the picker shows for one subject: what it needs, what is
 * linked now, and the candidates for each of its slots.
 *
 * Always asks for the FULL list (`showAll`), and the picker narrows it on the
 * client: every candidate carries `compatible`, so the "only sets that fit"
 * toggle needs no round trip — and, more importantly, the counter beside it
 * can say "3 of 711" honestly. Asking the backend to filter made the total
 * unknowable, which read as "0 of 1" next to a list of one.
 */
export function useCalibrationPicker(subject: PickerSubject | null): PickerData {
  const [requirement, setRequirement] = useState<Requirement | null>(null);
  const [candidates, setCandidates] = useState(NO_CANDIDATES);
  const [current, setCurrent] = useState(NO_CURRENT);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  const reload = useCallback(() => setTick(t => t + 1), []);

  useEffect(() => {
    if (!subject) return;
    let cancelled = false;
    const slots = slotsFor(subject);

    const load = async () => {
      setLoading(true);
      setError(null);
      try {
        const fetched: Record<SlotKind, CalibrationSetWithScore[]> = { ...NO_CANDIDATES };
        let req: Requirement;
        let linked: Record<SlotKind, number | null> = { ...NO_CURRENT };

        if (subject.kind === 'lights') {
          const params = await api.invoke<LightFrameParameters>('get_light_frame_parameters', {
            frameIds: subject.frameIds,
          });
          req = requirementFromLights(params);
          linked = {
            ...NO_CURRENT,
            flat: params.current_flat_set_id ?? subject.current.flat,
            dark: params.current_dark_set_id ?? subject.current.dark,
            bias: params.current_bias_set_id ?? subject.current.bias,
          };
          const lists = await Promise.all(
            slots.map(slot =>
              api.invoke<CalibrationSetWithScore[]>('get_calibration_sets_for_manual_selection', {
                frameIds: subject.frameIds,
                calibrationType: slot,
                showAll: true,
              }),
            ),
          );
          slots.forEach((slot, i) => {
            fetched[slot] = lists[i];
          });
        } else {
          const params = await api.invoke<CalibrationSetParameters>(
            'get_calibration_set_parameters',
            { setId: subject.setId },
          );
          req = requirementFromSet(params);
          linked = {
            ...NO_CURRENT,
            darkflat: params.current_darkflat_set_id,
            dark: params.current_dark_set_id,
            bias: params.current_bias_set_id,
          };
          const lists = await Promise.all(
            slots.map(slot =>
              api.invoke<CalibrationSetWithScore[]>(
                'get_subcalibration_sets_for_manual_selection',
                { setId: subject.setId, calibrationType: slot, showAll: true },
              ),
            ),
          );
          slots.forEach((slot, i) => {
            fetched[slot] = lists[i];
          });
        }

        if (cancelled) return;
        setRequirement(req);
        setCandidates(fetched);
        setCurrent(linked);
      } catch (err) {
        if (cancelled) return;
        console.error('[CalibrationPicker] load failed:', err);
        setError(typeof err === 'string' ? err : (err as Error)?.message ?? String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    void load();
    return () => {
      cancelled = true;
    };
    // `subject` is rebuilt by the opener each render; the identity that matters
    // is what it addresses.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    subject?.kind,
    subject?.kind === 'lights' ? subject.frameIds.join(',') : subject?.setId,
    tick,
  ]);

  return { requirement, candidates, current, loading, error, reload };
}

/**
 * Write the chosen links for a `set` subject. Lights are saved by the caller
 * (the hierarchy owns that transaction and its refresh); a sub-calibration
 * has no such owner, so the picker performs it: clear every override, then
 * assign whichever slot the user chose.
 */
export async function saveSubCalibration(
  setId: number,
  picks: Partial<Record<SlotKind, number | null>>,
): Promise<void> {
  await api.invoke('clear_subcalibration_override', {
    sourceSetId: setId,
    calibrationType: null,
  });
  // A flat takes ONE of dark flat / dark / bias — the fallback chain, in
  // order — so the first chosen slot wins and the rest are left cleared.
  for (const slot of ['darkflat', 'dark', 'bias'] as SlotKind[]) {
    const chosen = picks[slot];
    if (chosen) {
      await api.invoke('manual_assign_subcalibration', {
        sourceSetId: setId,
        calibrationSetId: chosen,
        calibrationType: WIRE_TYPE[slot],
      });
      return;
    }
  }
}
