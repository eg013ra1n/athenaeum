import { useState } from 'react';
import { api } from '../../api';
import { Checkbox } from '../settings/Checkbox';

/**
 * Collab v3 wave 2 Task 10 (R16, P13), moved into its own component in the
 * contributor-path cycle (spec §7.2, §9): a LOCAL preference that coalesces
 * and auto-publishes this device's own passing frames on scan/analysis/
 * solve/link/threshold changes (`set_project_auto_publish`; the hub never
 * learns of it). Rendered for EVERY member in the Contribute tab header —
 * unlike `AutoReplicateBar`'s auto-download switch, it is not gated on
 * `canReceive` (a send-only member still publishes).
 *
 * Saved then re-read — the parent's `onToggled` reloads the detail from the
 * catalog, so the rendered state is the stored one (S6), never optimistic.
 */
export default function AutoPublishSwitch({
  projectId,
  enabled,
  onToggled,
}: {
  projectId: string;
  enabled: boolean;
  onToggled: () => void;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const setEnabled = async (next: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await api.invoke('set_project_auto_publish', { projectId, enabled: next });
      onToggled();
    } catch (err) {
      // S6 — a failed preference write surfaces inline, never silently caught.
      const msg = err instanceof Error ? err.message : String(err);
      console.error('[projects] set_project_auto_publish failed:', err);
      setError(msg);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="max-w-xl space-y-1">
      <Checkbox
        checked={enabled}
        onChange={(checked) => void setEnabled(checked)}
        disabled={saving}
        label="Auto-publish my frames"
        description="Passing frames publish automatically as scans, analysis and links change."
      />
      {error && <p className="text-sm text-error">{error}</p>}
    </div>
  );
}
