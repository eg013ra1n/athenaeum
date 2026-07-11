// Account state + actions for Settings → Account (task B4b).
//
// A2 GUARD — "the app is fully functional without an account". ALL account
// state and logic live in exactly two files: this hook and its sole consumer
// `src/components/settings/AccountSection.tsx`. No page or component outside the
// Settings screen imports either symbol, so a signed-out (or never-signed-in)
// user hits zero account code on any normal path. The invariant is enforced by
// import-graph isolation and checked with:
//
//   grep -riE "useAccount|AccountSection" src/pages src/components \
//     --include="*.tsx" | grep -vi settings
//
// which MUST return empty. The two legitimate reference sites are filtered by
// `grep -vi settings`: `src/components/settings/AccountSection.tsx` (its path
// contains "settings") and the single registration point `src/pages/Settings.tsx`
// (the host page is literally named "Settings.tsx"). A surviving match means
// account state has leaked out of Settings — fix the leak, don't relax the grep.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../api';
import type { AccountDevice, AccountStatus } from '../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
export function accountErrMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Marker resolved value: an authed call hit a 401, the backend cleared the
 *  local session, and we re-polled to signed-out. Callers should render the
 *  signed-out UI and surface an info notice, not an error. */
export const SIGNED_OUT_HEALED = Symbol('signed-out-healed');
type Healed = typeof SIGNED_OUT_HEALED;

export interface UseAccount {
  /** `null` until the first status poll resolves. */
  status: AccountStatus | null;
  loading: boolean;
  /**
   * The last status-poll error, or `null`. Set when a poll rejects (e.g. the
   * hub is unreachable). Paired with a `null` `status` this means the initial
   * load failed — render an error + Retry, never a dead spinner.
   */
  statusError: string | null;
  devices: AccountDevice[];
  devicesLoading: boolean;
  devicesError: string | null;

  /** Re-poll status; returns the fresh value (or `null` if the poll failed). */
  refreshStatus: () => Promise<AccountStatus | null>;
  refreshDevices: () => Promise<void>;

  /** Request an email OTP. Throws the actionable message on failure. */
  sendCode: (email: string) => Promise<void>;
  /** Verify the OTP and land signed-in. Throws the actionable message. */
  verifyCode: (email: string, code: string) => Promise<void>;
  /** Local-only sign-out. */
  signOut: () => Promise<void>;
  /**
   * Rename a device by id (this device or a peer). Resolves `SIGNED_OUT_HEALED`
   * if the call 401'd and self-healed to signed-out; throws the hub's actionable
   * message otherwise (a duplicate name surfaces as `name already in use`).
   */
  renameDevice: (deviceId: string, name: string) => Promise<Healed | void>;
  /** Revoke a device. Revoking THIS device signs out locally (status re-polls). */
  revokeDevice: (deviceId: string) => Promise<Healed | void>;
}

export function useAccount(): UseAccount {
  const [status, setStatus] = useState<AccountStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [devices, setDevices] = useState<AccountDevice[]>([]);
  const [devicesLoading, setDevicesLoading] = useState(false);
  const [devicesError, setDevicesError] = useState<string | null>(null);

  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refreshStatus = useCallback(async (): Promise<AccountStatus | null> => {
    try {
      const s = await api.invoke<AccountStatus>('account_status');
      if (mounted.current) {
        setStatus(s);
        setStatusError(null);
      }
      return s;
    } catch (err) {
      // Status is offline-resolvable in the backend, so a failure here is
      // unexpected — log, never swallow silently. Record it so a failed first
      // poll surfaces an error + Retry instead of an eternal spinner.
      console.error('[account] status poll failed:', err);
      if (mounted.current) setStatusError(accountErrMsg(err));
      return null;
    }
  }, []);

  const refreshDevices = useCallback(async (): Promise<void> => {
    if (mounted.current) {
      setDevicesLoading(true);
      setDevicesError(null);
    }
    try {
      const list = await api.invoke<AccountDevice[]>('list_account_devices');
      if (mounted.current) setDevices(list);
    } catch (err) {
      console.error('[account] list devices failed:', err);
      // A 401 self-heals: the backend already dropped the session. Re-poll —
      // if we're now signed out, clear the table quietly; else show the error.
      const fresh = await refreshStatus();
      if (mounted.current) {
        if (fresh && !fresh.signedIn) {
          setDevices([]);
        } else {
          setDevicesError(accountErrMsg(err));
        }
      }
    } finally {
      if (mounted.current) setDevicesLoading(false);
    }
  }, [refreshStatus]);

  // Initial status load.
  useEffect(() => {
    (async () => {
      await refreshStatus();
      if (mounted.current) setLoading(false);
    })();
  }, [refreshStatus]);

  // Load the device list whenever we transition into a signed-in state.
  const signedIn = status?.signedIn ?? false;
  useEffect(() => {
    if (signedIn) {
      void refreshDevices();
    } else {
      setDevices([]);
      setDevicesError(null);
    }
  }, [signedIn, refreshDevices]);

  const sendCode = useCallback(async (email: string): Promise<void> => {
    try {
      await api.invoke('account_sign_in_start', { email });
    } catch (err) {
      console.error('[account] send code failed:', err);
      throw new Error(accountErrMsg(err));
    }
  }, []);

  const verifyCode = useCallback(
    async (email: string, code: string): Promise<void> => {
      try {
        const s = await api.invoke<AccountStatus>('account_sign_in_verify', { email, code });
        if (mounted.current) setStatus(s);
      } catch (err) {
        console.error('[account] verify code failed:', err);
        throw new Error(accountErrMsg(err));
      }
    },
    [],
  );

  const signOut = useCallback(async (): Promise<void> => {
    try {
      await api.invoke('account_sign_out');
    } catch (err) {
      console.error('[account] sign out failed:', err);
      throw new Error(accountErrMsg(err));
    } finally {
      await refreshStatus();
    }
  }, [refreshStatus]);

  const renameDevice = useCallback(
    async (deviceId: string, name: string): Promise<Healed | void> => {
      try {
        await api.invoke('rename_device', { deviceId, name });
      } catch (err) {
        console.error('[account] rename device failed:', err);
        const fresh = await refreshStatus();
        if (fresh && !fresh.signedIn) return SIGNED_OUT_HEALED;
        // A duplicate name maps to `name already in use` — surfaced verbatim.
        throw new Error(accountErrMsg(err));
      }
      // Name changed — refresh both the device list and status (status.deviceId
      // carries no name, but a self-rename should still re-poll for consistency).
      await refreshDevices();
      await refreshStatus();
    },
    [refreshStatus, refreshDevices],
  );

  const revokeDevice = useCallback(
    async (deviceId: string): Promise<Healed | void> => {
      const isSelf = status?.deviceId === deviceId;
      try {
        await api.invoke('revoke_account_device', { deviceId });
      } catch (err) {
        console.error('[account] revoke device failed:', err);
        const fresh = await refreshStatus();
        if (fresh && !fresh.signedIn) return SIGNED_OUT_HEALED;
        throw new Error(accountErrMsg(err));
      }
      // Revoking THIS device clears the local session server-side — re-poll to
      // reflect signed-out. Otherwise just refresh the list.
      if (isSelf) {
        await refreshStatus();
      } else {
        await refreshDevices();
      }
    },
    [status?.deviceId, refreshStatus, refreshDevices],
  );

  return {
    status,
    loading,
    statusError,
    devices,
    devicesLoading,
    devicesError,
    refreshStatus,
    refreshDevices,
    sendCode,
    verifyCode,
    signOut,
    renameDevice,
    revokeDevice,
  };
}
