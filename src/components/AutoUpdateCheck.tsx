import { useAutoUpdateCheck } from '../hooks/useAutoUpdateCheck';

/**
 * Headless: runs the startup update check from inside NotificationProvider so
 * the hook can raise a toast + bell entry. Renders nothing.
 */
export function AutoUpdateCheck() {
  useAutoUpdateCheck();
  return null;
}
