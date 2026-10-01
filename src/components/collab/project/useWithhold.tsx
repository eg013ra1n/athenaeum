import { useCallback, useRef, useState, type JSX } from 'react';
import { api } from '../../../api';
import { ConfirmDialog } from '../../ConfirmDialog';
import { useNotifications } from '../../../contexts/NotificationContext';

export interface WithholdTarget { frameId: number; prepared: boolean }

/** Spec 2026-10-01 §4.5 / plan F2 — Don't publish (withheld=true) and Release (withheld=false).
 *  Confirms only when calibrated files are deleted. */
export function useWithhold(projectId: string, onChanged: () => void) {
  const { notify } = useNotifications();
  const [busy, setBusy] = useState(false);
  const [ask, setAsk] = useState<{ targets: WithholdTarget[]; prepared: number } | null>(null);
  const resolver = useRef<((ok: boolean) => void) | null>(null);
  const changed = useRef(onChanged);
  changed.current = onChanged;

  const write = useCallback(async (frameIds: number[], withheld: boolean): Promise<boolean> => {
    setBusy(true);
    try {
      await api.invoke<number>('set_collab_frames_withheld', { projectId, frameIds, withheld });
      changed.current();
      return true;
    } catch (err) {
      console.error('[projects] set_collab_frames_withheld failed:', err);
      notify({
        title: withheld ? 'Could not withhold the frames' : 'Could not release the frames',
        detail: err instanceof Error ? err.message : String(err),
        kind: 'project', tone: 'warning', hasErrors: true,
      });
      return false;
    } finally {
      setBusy(false);
    }
  }, [projectId, notify]);

  const dontPublish = useCallback((targets: WithholdTarget[]): Promise<boolean> => {
    const prepared = targets.filter((t) => t.prepared).length;
    if (prepared === 0) return write(targets.map((t) => t.frameId), true);
    return new Promise<boolean>((resolve) => {
      resolver.current?.(false);
      resolver.current = resolve;
      setAsk({ targets, prepared });
    });
  }, [write]);

  const release = useCallback((frameIds: number[]) => write(frameIds, false), [write]);

  const settle = (ok: boolean) => { const r = resolver.current; resolver.current = null; setAsk(null); r?.(ok); };

  const dialog: JSX.Element | null = ask && (
    <ConfirmDialog
      isOpen
      title={`Don't publish ${ask.targets.length} ${ask.targets.length === 1 ? 'frame' : 'frames'}?`}
      message={`${ask.prepared} calibrated ${ask.prepared === 1 ? 'file will be deleted' : 'files will be deleted'}. The frames move to Held back as "Withheld by you"; Release brings them back to Ready.`}
      confirmText="Don't publish"
      confirmDanger
      onConfirm={() => {
        const r = resolver.current;
        resolver.current = null;
        const ids = ask.targets.map((x) => x.frameId);
        setAsk(null);
        void write(ids, true).then((ok) => r?.(ok));
      }}
      onCancel={() => settle(false)}
    />
  );

  return { dontPublish, release, busy, dialog };
}
