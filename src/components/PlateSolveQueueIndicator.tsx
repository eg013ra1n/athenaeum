import { Crosshair } from 'lucide-react';
import { usePlateSolveProgressContext } from '../contexts/PlateSolveProgressContext';
import { QueueIndicator } from './QueueIndicator';

interface PlateSolveQueueIndicatorProps {
  collapsed: boolean;
}

export function PlateSolveQueueIndicator({ collapsed }: PlateSolveQueueIndicatorProps) {
  const { currentBatch, queueLength, hasActiveBatches, cancelAll, assistantRun } = usePlateSolveProgressContext();

  // The assistant's run holds the one slot, so it is the only run to show while it goes.
  const progress = assistantRun ? { current: assistantRun.done, total: assistantRun.total } : currentBatch?.progress;
  const percent = progress && progress.total > 0 ? (progress.current / progress.total) * 100 : 0;
  const label = assistantRun ? 'Plate solve (assistant)' : currentBatch?.label || 'Plate solve';

  return (
    <QueueIndicator
      collapsed={collapsed}
      icon={Crosshair}
      active={hasActiveBatches || assistantRun != null}
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
