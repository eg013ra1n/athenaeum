import { Layers } from 'lucide-react';
import { useRegistrationProgressContext } from '../contexts/RegistrationProgressContext';
import { QueueIndicator } from './QueueIndicator';

interface RegistrationQueueIndicatorProps {
  collapsed: boolean;
}

export function RegistrationQueueIndicator({ collapsed }: RegistrationQueueIndicatorProps) {
  const { currentRegistration, queueLength, hasActiveRegistrations, cancelAll } =
    useRegistrationProgressContext();

  const progress = currentRegistration?.progress;
  const percent =
    progress && progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;
  const label = currentRegistration?.frameSetName || 'Registration';

  return (
    <QueueIndicator
      collapsed={collapsed}
      icon={Layers}
      active={hasActiveRegistrations}
      label={label}
      percent={percent}
      current={progress?.current}
      total={progress?.total}
      queueLength={queueLength}
      cancelTitle="Cancel registration"
      onCancelAll={cancelAll}
    />
  );
}
