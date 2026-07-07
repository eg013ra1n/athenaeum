import { useEffect, useRef, useState } from 'react';
import {
  Mail,
  KeyRound,
  LogIn,
  LogOut,
  ArrowLeft,
  ShieldCheck,
  Server,
  Monitor,
  Trash2,
  Loader2,
  AlertTriangle,
  RefreshCw,
} from 'lucide-react';
import { formatTimestamp } from '../../utils/dateFormatting';
import { useNotifications } from '../../contexts/NotificationContext';
import { useAccount, accountErrMsg, SIGNED_OUT_HEALED } from '../../hooks/useAccount';
import type { AccountDevice, DeviceRole } from '../../types/models';

/** Compact display for a hub-assigned device id (opaque, can be long). */
function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/** Small coloured pill for a device's role. */
function RoleBadge({ role }: { role: DeviceRole | null }) {
  if (role === 'primary') {
    return (
      <span className="inline-flex items-center rounded-full bg-accent/20 px-2 py-0.5 text-xs font-medium text-accent">
        Primary
      </span>
    );
  }
  if (role === 'capture') {
    return (
      <span className="inline-flex items-center rounded-full bg-purple/20 px-2 py-0.5 text-xs font-medium text-purple">
        Capture
      </span>
    );
  }
  return (
    <span className="inline-flex items-center rounded-full bg-surface-hover px-2 py-0.5 text-xs font-medium text-content-muted">
      Unassigned
    </span>
  );
}

/**
 * Settings → Account. Renders inner content only (the card/heading are supplied
 * by the host in `Settings.tsx`, matching the `LoggingSettings` pattern).
 *
 * Three states: loading, signed-out (email → code sign-in), signed-in (account
 * card + machine-role selector + device list). All account state is owned by
 * `useAccount`; see that hook's header for the A2 isolation guard.
 */
export default function AccountSection() {
  const { notify } = useNotifications();
  const {
    status,
    loading,
    statusError,
    devices,
    devicesLoading,
    devicesError,
    refreshStatus,
    sendCode,
    verifyCode,
    signOut,
    setRole,
    revokeDevice,
  } = useAccount();

  // ── sign-in form (signed-out) ──────────────────────────────────────────────
  const [step, setStep] = useState<'email' | 'code'>('email');
  const [email, setEmail] = useState('');
  const [code, setCode] = useState('');
  const [sending, setSending] = useState(false);
  const [verifying, setVerifying] = useState(false);
  const [signInError, setSignInError] = useState<string | null>(null);

  // ── signed-in controls ─────────────────────────────────────────────────────
  const [roleDraft, setRoleDraft] = useState<DeviceRole | null>(null);
  const [peerDraft, setPeerDraft] = useState('');
  const [roleError, setRoleError] = useState<string | null>(null);
  const [roleSaving, setRoleSaving] = useState(false);
  const [revokingId, setRevokingId] = useState<string | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const [retryingStatus, setRetryingStatus] = useState(false);

  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const signedIn = status?.signedIn ?? false;
  const deviceId = status?.deviceId ?? null;
  const currentRole = status?.role ?? null;
  const thisDevice = devices.find((d) => d.id === deviceId);
  const currentPeer = thisDevice?.peerDeviceId ?? '';
  const peerCandidates = devices.filter((d) => d.id !== deviceId);

  // Reset the sign-in form each time we (re-)enter the signed-out state so a
  // stale code/step never wedges a fresh sign-in.
  useEffect(() => {
    if (!signedIn) {
      setStep('email');
      setCode('');
      setSignInError(null);
    }
  }, [signedIn]);

  // Keep the role/peer drafts in step with the resolved status.
  useEffect(() => {
    setRoleDraft(currentRole);
    setRoleError(null);
  }, [currentRole, deviceId]);
  useEffect(() => {
    setPeerDraft(currentPeer);
  }, [currentPeer]);

  // ── handlers ────────────────────────────────────────────────────────────────

  const handleSendCode = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = email.trim();
    if (!trimmed || sending) return;
    setSending(true);
    setSignInError(null);
    try {
      await sendCode(trimmed);
      setStep('code');
    } catch (err) {
      setSignInError(accountErrMsg(err));
    } finally {
      setSending(false);
    }
  };

  const handleVerify = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmedCode = code.trim();
    if (!trimmedCode || verifying) return;
    setVerifying(true);
    setSignInError(null);
    try {
      await verifyCode(email.trim(), trimmedCode);
      // status flips to signed-in inside the hook → re-renders signed-in view.
      notify({
        title: 'Signed in',
        detail: `Signed in as ${email.trim()}.`,
        kind: 'generic',
        tone: 'success',
      });
    } catch (err) {
      setSignInError(accountErrMsg(err));
    } finally {
      setVerifying(false);
    }
  };

  const handleBackToEmail = () => {
    setStep('email');
    setCode('');
    setSignInError(null);
  };

  const applyRole = async (role: DeviceRole, peer: string | null) => {
    setRoleSaving(true);
    setRoleError(null);
    try {
      const res = await setRole(role, peer);
      if (res === SIGNED_OUT_HEALED) {
        notify({
          title: 'Signed out',
          detail: 'This device was signed out. Please sign in again.',
          kind: 'generic',
          tone: 'warning',
        });
        return;
      }
      notify({
        title: 'Machine role updated',
        detail: role === 'capture' ? 'This machine is now a Capture device.' : 'This machine is now the Primary device.',
        kind: 'generic',
        tone: 'success',
      });
    } catch (err) {
      // 409 (second primary) / 400 (peer validation) → actionable, inline.
      setRoleError(accountErrMsg(err));
      setRoleDraft(currentRole); // revert the radio to the true state
    } finally {
      setRoleSaving(false);
    }
  };

  const handleSelectRole = (next: DeviceRole) => {
    setRoleError(null);
    setRoleDraft(next);
    // Primary applies immediately (and clears any peer link). Capture waits for
    // a peer to be picked below, then applies via the Apply button.
    if (next === 'primary') {
      void applyRole('primary', null);
    }
  };

  const handleRevoke = async (device: AccountDevice) => {
    const isSelf = device.id === deviceId;
    const confirmMsg = isSelf
      ? 'Revoke THIS device? You will be signed out on this machine and must sign in again.'
      : `Revoke "${device.name}"? It will lose access to this account until it signs in again.`;
    if (!window.confirm(confirmMsg)) return;
    setRevokingId(device.id);
    try {
      const res = await revokeDevice(device.id);
      if (res === SIGNED_OUT_HEALED) {
        notify({
          title: 'Signed out',
          detail: 'This device was signed out. Please sign in again.',
          kind: 'generic',
          tone: 'warning',
        });
        return;
      }
      notify({
        title: isSelf ? 'Signed out on this device' : 'Device revoked',
        detail: isSelf
          ? 'This device was revoked and signed out.'
          : `"${device.name}" was revoked.`,
        kind: 'generic',
        tone: 'success',
      });
    } catch (err) {
      notify({
        title: 'Failed to revoke device',
        detail: accountErrMsg(err),
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setRevokingId(null);
    }
  };

  const handleSignOut = async () => {
    if (signingOut) return;
    setSigningOut(true);
    try {
      await signOut();
      notify({
        title: 'Signed out',
        detail: 'You have been signed out on this device.',
        kind: 'generic',
        tone: 'success',
      });
    } catch (err) {
      notify({
        title: 'Sign out failed',
        detail: accountErrMsg(err),
        kind: 'generic',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      setSigningOut(false);
    }
  };

  const handleRetryStatus = async () => {
    if (retryingStatus) return;
    setRetryingStatus(true);
    try {
      await refreshStatus();
    } finally {
      if (mounted.current) setRetryingStatus(false);
    }
  };

  // ── render ───────────────────────────────────────────────────────────────────

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-content-muted">
        <Loader2 size={16} className="animate-spin" />
        Loading account…
      </div>
    );
  }

  // The first status poll resolved but returned nothing — the hub is
  // unreachable (or the command failed). Never leave a dead "Loading…" spinner:
  // surface the error and a Retry, mirroring the devices-list error treatment.
  if (!status) {
    return (
      <div className="max-w-md space-y-3">
        <div className="flex items-start gap-2 rounded-lg border border-error/50 bg-error-muted p-3">
          <AlertTriangle size={16} className="text-error flex-shrink-0 mt-0.5" />
          <p className="text-sm text-error">
            Couldn&apos;t load your account{statusError ? `: ${statusError}` : '.'}
          </p>
        </div>
        <button
          onClick={handleRetryStatus}
          disabled={retryingStatus}
          className="inline-flex items-center gap-2 px-4 py-2 border border-border rounded-lg text-sm text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
        >
          {retryingStatus ? (
            <Loader2 size={16} className="animate-spin" />
          ) : (
            <RefreshCw size={16} />
          )}
          Retry
        </button>
      </div>
    );
  }

  // SIGNED OUT — sign-in flow.
  if (!status.signedIn) {
    return (
      <div className="space-y-4 max-w-md">
        <p className="text-sm text-content-muted">
          Sign in to link this device to your account for syncing between machines. The app is
          fully usable without an account.
        </p>

        {step === 'email' ? (
          <form onSubmit={handleSendCode} className="space-y-3">
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Email
              </label>
              <div className="relative">
                <Mail
                  size={16}
                  className="absolute left-3 top-1/2 -translate-y-1/2 text-content-muted pointer-events-none"
                />
                <input
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  placeholder="you@example.com"
                  autoComplete="email"
                  className="w-full bg-surface-hover border border-border rounded-lg pl-9 pr-3 py-2 text-content focus:outline-none focus:border-accent"
                />
              </div>
            </div>

            {signInError && (
              <p className="text-sm text-error">{signInError}</p>
            )}

            <button
              type="submit"
              disabled={sending || !email.trim()}
              className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
            >
              {sending ? <Loader2 size={18} className="animate-spin" /> : <Mail size={18} />}
              {sending ? 'Sending…' : 'Send code'}
            </button>
            <p className="text-xs text-content-muted">
              We&apos;ll email a 6-digit code. Signs in to{' '}
              <span className="text-content-secondary">{status.hubUrl}</span>.
            </p>
          </form>
        ) : (
          <form onSubmit={handleVerify} className="space-y-3">
            <p className="text-sm text-content-muted">
              Enter the 6-digit code sent to{' '}
              <span className="text-content-secondary">{email.trim()}</span>.
            </p>
            <div>
              <label className="block text-sm font-medium text-content-secondary mb-2">
                Verification code
              </label>
              <div className="relative">
                <KeyRound
                  size={16}
                  className="absolute left-3 top-1/2 -translate-y-1/2 text-content-muted pointer-events-none"
                />
                <input
                  type="text"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                  placeholder="123456"
                  autoFocus
                  className="w-full bg-surface-hover border border-border rounded-lg pl-9 pr-3 py-2 tracking-widest text-content focus:outline-none focus:border-accent"
                />
              </div>
            </div>

            {signInError && (
              <p className="text-sm text-error">{signInError}</p>
            )}

            <div className="flex items-center gap-3">
              <button
                type="submit"
                disabled={verifying || !code.trim()}
                className="flex items-center gap-2 px-6 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg transition-colors"
              >
                {verifying ? <Loader2 size={18} className="animate-spin" /> : <LogIn size={18} />}
                {verifying ? 'Signing in…' : 'Sign in'}
              </button>
              <button
                type="button"
                onClick={handleBackToEmail}
                disabled={verifying}
                className="flex items-center gap-1 text-sm text-content-muted hover:text-content transition-colors disabled:opacity-50"
              >
                <ArrowLeft size={14} />
                Use a different email
              </button>
            </div>
          </form>
        )}
      </div>
    );
  }

  // SIGNED IN — account card, role selector, device list.
  return (
    <div className="space-y-6">
      {/* Account card */}
      <div className="rounded-lg border border-border bg-surface p-4">
        <div className="flex items-start justify-between gap-4">
          <div className="space-y-2 min-w-0">
            <div className="flex items-center gap-2 text-content">
              <ShieldCheck size={16} className="text-success flex-shrink-0" />
              <span className="font-medium truncate">{status.email ?? 'Signed in'}</span>
            </div>
            <div className="flex items-center gap-2 text-sm text-content-muted">
              <Monitor size={14} className="flex-shrink-0" />
              This device:{' '}
              <span className="font-mono text-content-secondary">
                {status.deviceId ? shortId(status.deviceId) : '—'}
              </span>
              <RoleBadge role={currentRole} />
            </div>
            <div className="flex items-center gap-2 text-xs text-content-muted">
              <Server size={14} className="flex-shrink-0" />
              <span className="truncate">{status.hubUrl}</span>
            </div>
          </div>
          <button
            onClick={handleSignOut}
            disabled={signingOut}
            className="flex items-center gap-2 px-3 py-1.5 border border-border rounded-lg text-sm text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors flex-shrink-0"
          >
            {signingOut ? <Loader2 size={16} className="animate-spin" /> : <LogOut size={16} />}
            Sign out
          </button>
        </div>
      </div>

      {/* Machine role */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-1">Machine role</h4>
        <p className="text-xs text-content-muted mb-3">
          How this machine participates in sync. A <strong>Capture</strong> device sends its frames
          to a paired <strong>Primary</strong>.
        </p>

        <div className="space-y-2">
          <label className="flex items-center gap-2 text-sm text-content-muted cursor-not-allowed opacity-70">
            <input type="radio" name="machine-role" checked={roleDraft === null} disabled readOnly />
            None <span className="text-xs">(not assigned)</span>
          </label>
          <label className="flex items-center gap-2 text-sm text-content cursor-pointer">
            <input
              type="radio"
              name="machine-role"
              checked={roleDraft === 'primary'}
              disabled={roleSaving}
              onChange={() => handleSelectRole('primary')}
            />
            Primary <span className="text-xs text-content-muted">— receives frames</span>
          </label>
          <label className="flex items-center gap-2 text-sm text-content cursor-pointer">
            <input
              type="radio"
              name="machine-role"
              checked={roleDraft === 'capture'}
              disabled={roleSaving}
              onChange={() => handleSelectRole('capture')}
            />
            Capture <span className="text-xs text-content-muted">— sends frames to a Primary</span>
          </label>
        </div>

        {/* Peer picker for Capture role */}
        {roleDraft === 'capture' && (
          <div className="mt-3 pl-6 space-y-2">
            {peerCandidates.length === 0 ? (
              <p className="text-sm text-content-muted">
                No other devices on this account yet. Sign in on your Primary machine first, then
                come back to pair this one.
              </p>
            ) : (
              <>
                <label className="block text-xs text-content-muted">Paired Primary device</label>
                <div className="flex items-center gap-2 flex-wrap">
                  <select
                    value={peerDraft}
                    onChange={(e) => setPeerDraft(e.target.value)}
                    disabled={roleSaving}
                    className="bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                  >
                    <option value="">Select a device…</option>
                    {peerCandidates.map((d) => (
                      <option key={d.id} value={d.id}>
                        {d.name} ({shortId(d.id)})
                      </option>
                    ))}
                  </select>
                  <button
                    onClick={() => applyRole('capture', peerDraft)}
                    disabled={roleSaving || !peerDraft}
                    className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-surface rounded-lg text-sm transition-colors"
                  >
                    {roleSaving ? <Loader2 size={16} className="animate-spin" /> : null}
                    Apply
                  </button>
                </div>
              </>
            )}
          </div>
        )}

        {roleError && (
          <div className="mt-3 flex items-start gap-2 rounded-lg border border-error/50 bg-error-muted p-2.5">
            <AlertTriangle size={16} className="text-error flex-shrink-0 mt-0.5" />
            <p className="text-sm text-error">{roleError}</p>
          </div>
        )}
      </div>

      {/* Device list */}
      <div>
        <div className="flex items-center justify-between mb-2">
          <h4 className="text-sm font-medium text-content-secondary">Devices</h4>
          {devicesLoading && <Loader2 size={14} className="animate-spin text-content-muted" />}
        </div>

        {devicesError ? (
          <div className="flex items-start gap-2 rounded-lg border border-error/50 bg-error-muted p-3">
            <AlertTriangle size={16} className="text-error flex-shrink-0 mt-0.5" />
            <p className="text-sm text-error">Failed to load devices: {devicesError}</p>
          </div>
        ) : devices.length === 0 && !devicesLoading ? (
          <p className="text-sm text-content-muted">No devices registered.</p>
        ) : (
          <div className="overflow-x-auto rounded-lg border border-border">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-content-muted">
                  <th className="px-3 py-2 font-medium">Device</th>
                  <th className="px-3 py-2 font-medium">Role</th>
                  <th className="px-3 py-2 font-medium">Created</th>
                  <th className="px-3 py-2 font-medium">Last seen</th>
                  <th className="px-3 py-2 font-medium sr-only">Actions</th>
                </tr>
              </thead>
              <tbody>
                {devices.map((d) => {
                  const isSelf = d.id === deviceId;
                  return (
                    <tr key={d.id} className="border-b border-border/50 last:border-b-0">
                      <td className="px-3 py-2">
                        <div className="flex items-center gap-2">
                          <span className="text-content truncate">{d.name}</span>
                          {isSelf && (
                            <span className="inline-flex items-center rounded-full bg-accent/20 px-1.5 py-0.5 text-[10px] font-medium text-accent">
                              This device
                            </span>
                          )}
                        </div>
                        <span className="font-mono text-xs text-content-muted">{shortId(d.id)}</span>
                      </td>
                      <td className="px-3 py-2">
                        <RoleBadge role={d.role} />
                      </td>
                      <td className="px-3 py-2 text-content-muted whitespace-nowrap">
                        {formatTimestamp(d.createdAt)}
                      </td>
                      <td className="px-3 py-2 text-content-muted whitespace-nowrap">
                        {d.lastSeenAt ? formatTimestamp(d.lastSeenAt) : '—'}
                      </td>
                      <td className="px-3 py-2 text-right">
                        <button
                          onClick={() => handleRevoke(d)}
                          disabled={revokingId === d.id}
                          title={isSelf ? 'Revoke this device (signs out)' : 'Revoke device'}
                          className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-error hover:bg-error-muted disabled:opacity-50 transition-colors"
                        >
                          {revokingId === d.id ? (
                            <Loader2 size={14} className="animate-spin" />
                          ) : (
                            <Trash2 size={14} />
                          )}
                          Revoke
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
