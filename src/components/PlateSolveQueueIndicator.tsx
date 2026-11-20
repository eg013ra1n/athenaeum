import { Crosshair } from 'lucide-react';
import { usePlateSolveProgressContext } from '../contexts/PlateSolveProgressContext';
import { QueueIndicator } from './QueueIndicator';

interface PlateSolveQueueIndicatorProps {
  collapsed: boolean;
}

export function PlateSolveQueueIndicator({ collapsed }: PlateSolveQueueIndicatorProps) {
  const { currentBatch, queueLength, hasActiveBatches, cancelAll, foreignRun } = usePlateSolveProgressContext();

  // A run this page did not start holds the one slot, so it is the only run to show while it goes.
  const progress = foreignRun ? { current: foreignRun.done, total: foreignRun.total } : currentBatch?.progress;
  const percent = progress && progress.total > 0 ? (progress.current / progress.total) * 100 : 0;
  const label = foreignRun ? (foreignRun.origin === 'assistant' ? 'Plate solve (assistant)' : 'Plate solve') : currentBatch?.label || 'Plate solve';

  return (
    <QueueIndicator
      collapsed={collapsed}
      icon={Crosshair}
      active={hasActiveBatches || foreignRun != null}
      label={label}
      percent={percent}
      current={progress?.current}
      total={progress?.total}
      queueLength={queueLength}
      cancelTitle="Cancel plate solve"
      onCancelAll={cancelAll}
    />
  );
}
