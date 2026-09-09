import { ObjectFileLocations } from './ObjectFileLocations';
import { FailedSolvesReview } from './plate-solve/FailedSolvesReview';
import { useRef, useState } from 'react';
import { ScanSearch } from 'lucide-react';
import { api } from '../api';
import { useNotifications } from '../contexts/NotificationContext';
import { usePlateSolveProgressContext } from '../contexts/PlateSolveProgressContext';
import {
  PlateSolveBatchPanel,
  type PlateSolveBatchPanelHandle,
} from './plate-solve/PlateSolveBatchPanel';
import { ToolbarButton, ToolbarContainer } from './Toolbar';

interface Props {
  selectedIds: number[];
  visibleCount: number;
  disabled: boolean;
  onSelectAll: () => void;
  onClear: () => void;
  onSolveComplete: () => void;
}

export function ObjectPlateSolveToolbar({
  selectedIds,
  visibleCount,
  disabled,
  onSelectAll,
  onClear,
  onSolveComplete,
}: Props) {
  const [showFailures, setShowFailures] = useState(false);
  const [preparing, setPreparing] = useState(false);
  const preparingRef = useRef(false);
  const panelRef = useRef<PlateSolveBatchPanelHandle>(null);
  const { hasActiveBatches } = usePlateSolveProgressContext();
  const { notify } = useNotifications();

  const solve = async () => {
    if (disabled || preparingRef.current || hasActiveBatches || !selectedIds.length) return;
    preparingRef.current = true;
    setPreparing(true);
    // Capture the selection before awaiting so subsequent UI changes cannot
    // silently change which objects this click will solve.
    const ids = [...selectedIds];
    try {
      const frames = await api.invoke<number[]>('get_object_plate_solve_frame_ids', {
        framesSetIds: ids,
      });
      if (!frames.length) {
        notify({
          title: 'No eligible frames',
          detail: 'The selected objects have no available LIGHT frames to plate solve.',
          kind: 'platesolve',
          tone: 'warning',
        });
        return;
      }
      panelRef.current?.start([...new Set(frames)]);
    } catch (error) {
      console.error('Failed to resolve selected object frames:', error);
      notify({
        title: 'Could not prepare plate solving',
        detail: String(error),
        kind: 'platesolve',
        hasErrors: true,
        tone: 'warning',
      });
    } finally {
      preparingRef.current = false;
      setPreparing(false);
    }
  };

  return (
    <div className="mb-4 space-y-2">
      {showFailures && <FailedSolvesReview onClose={() => setShowFailures(false)} />}
      {selectedIds.length > 0 && <ObjectFileLocations ids={selectedIds} />}
      <ToolbarContainer>
        <ToolbarButton disabled={disabled || !visibleCount} onClick={onSelectAll}>
          Select All ({visibleCount})
        </ToolbarButton>
        <ToolbarButton disabled={!selectedIds.length} onClick={onClear}>
          Clear selection
        </ToolbarButton>
        <ToolbarButton
          icon={ScanSearch}
          disabled={disabled || preparing || hasActiveBatches || !selectedIds.length}
          onClick={solve}
          title="Solve all available LIGHT frames in the selected objects, including frames with existing coordinates."
        >
          {preparing ? 'Preparing frames…' : `Plate Solve Selected (${selectedIds.length})`}
        </ToolbarButton>
        <ToolbarButton onClick={() => setShowFailures(true)}>Failed solves</ToolbarButton>
        <span className="text-xs text-content-muted self-center">
          {selectedIds.length} selected · Select All applies to this tab and current filters
        </span>
      </ToolbarContainer>
      <p className="text-xs text-content-muted">
        Solves available LIGHT frames, including existing coordinates. Known-missing files are
        skipped.
      </p>
      <PlateSolveBatchPanel ref={panelRef} hideTriggerButtons onSolveComplete={onSolveComplete} />
    </div>
  );
}
