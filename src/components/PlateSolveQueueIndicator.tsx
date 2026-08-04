import { Crosshair } from 'lucide-react';
import { usePlateSolveProgressContext } from '../contexts/PlateSolveProgressContext';
import { useSmoothedPercent } from '../hooks/useSmoothedPercent';
import { QueueIndicator } from './QueueIndicator';

interface PlateSolveQueueIndicatorProps {
  collapsed: boolean;
}

export function PlateSolveQueueIndicator({ collapsed }: PlateSolveQueueIndicatorProps) {
  const { currentBatch, queueLength, hasActiveBatches, cancelAll } = usePlateSolveProgressContext();

  const progress = currentBatch?.progress;
  const realPercent = progress && progress.total > 0 ? (progress.current / progress.total) * 100 : 0;
  const total = progress?.total ?? 0;
  const percent = useSmoothedPercent(realPercent, total);
  const label = currentBatch?.label || 'Plate solve';

  return (
    <QueueIndicator
      collapsed={collapsed}
      icon={Crosshair}
      active={hasActiveBatches}
      label={label}
      percent={percent}
      current={progress?.current}
      total={progress?.total}
      queueLength={queueLength}
      cancelTitle="Cancel plate solve"
      onCancelAll={cancelAll}
      barTransition="linear"
    />
  );
}
