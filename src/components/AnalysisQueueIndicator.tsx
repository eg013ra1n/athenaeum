import { BarChart3 } from 'lucide-react';
import { useAnalysisProgressContext } from '../contexts/AnalysisProgressContext';
import { QueueIndicator } from './QueueIndicator';

interface AnalysisQueueIndicatorProps {
  collapsed: boolean;
}

export function AnalysisQueueIndicator({ collapsed }: AnalysisQueueIndicatorProps) {
  const { currentAnalysis, queueLength, hasActiveAnalyses, cancelAll } = useAnalysisProgressContext();

  const progress = currentAnalysis?.progress;
  const percent = progress?.percent ?? 0;
  const label = currentAnalysis?.frameSetName || 'Analysis';

  return (
    <QueueIndicator
      collapsed={collapsed}
      icon={BarChart3}
      active={hasActiveAnalyses}
      label={label}
      percent={percent}
      current={progress?.current}
      total={progress?.total}
      queueLength={queueLength}
      cancelTitle="Cancel analysis"
      onCancelAll={cancelAll}
    />
  );
}
