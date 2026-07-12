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
  Network,
  Pencil,
  Trash2,
  Loader2,
  AlertTriangle,
  RefreshCw,
} from 'lucide-react';
import { api } from '../../api';
import { formatTimestamp } from '../../utils/dateFormatting';
import { useNotifications } from '../../contexts/NotificationContext';
import { useAccount, accountErrMsg, SIGNED_OUT_HEALED } from '../../hooks/useAccount';
import type { AccountDevice, DeviceCapability } from '../../types/models';

/** The two first-class hub registries surfaced by the selector. */
const PROD_HUB_URL = 'https://projects.artfrom.space';
const TEST_HUB_URL = 'https://test-hub.artfrom.space';

/**
 * Default hub when `account.hub_url` is unset — mirrors the backend's
 * build-profile default (`settings::defaults::ACCOUNT_HUB_URL`): dev builds
 * point at the test hub, release builds (prod + betas) at production.
 */
const DEFAULT_HUB_URL = import.meta.env.DEV ? TEST_HUB_URL : PROD_HUB_URL;

/** Compact display for a hub-assigned device id (opaque, can be long). */
function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/** Human label for a device capability. The app is always a full peer. */
function capabilityLabel(capability: DeviceCapability): string {
  return capability === 'perseus' ? 'Send-only' : 'Full peer';
}

/** Small coloured pill for a device's capability (full peer vs send-only agent). */
function CapabilityPill({ capability }: { capability: DeviceCapability }) {
  const isPeer = capability !== 'perseus';
  return (
    <span
      className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${
        isPeer ? 'bg-accent/20 text-accent' : 'bg-purple/20 text-purple'
      }`}
    >
      {capabilityLabel(capability)}
    </span>
  );
}

/**
 * Inline editor for this device's display name → `rename_device`. Mirrors
 * `HubUrlDevEditor`'s load/edit/save shape: local `value`/`saving`/`error`
 * state, an explicit Save button (never per-keystroke), a duplicate-name error
 * surfaced inline (the hub maps a clash to `name already in use`). Re-seeds from
 * `initialName` when the resolved device name changes (e.g. after a rename
 * refresh) but never clobbers text the user has started editing.
 */
function DeviceNameEditor({
  deviceId,
  initialName,
  onRename,
  onRenamed,
  onSignedOut,
}: {
  deviceId: string;
  initialName: string;
  onRename: (deviceId: string, name: string) => Promise<typeof SIGNED_OUT_HEALED | void>;
  onRenamed: (name: string) => void;
  onSignedOut: () => void;
}) {
  const [value, setValue] = useState(initialName);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);
  const prevInitial = useRef(initialName);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  // Re-seed on an external name change, but only if the user hasn't diverged the
  // field from the last-known value (never stomp an in-progress edit).
  useEffect(() => {
    setValue((cur) => (cur === prevInitial.current ? initialName : cur));
    prevInitial.current = initialName;
  }, [initialName]);

  const trimmed = value.trim();
  const dirty = trimmed !== initialName.trim();

  const handleSave = async () => {
    if (saving) return;
    if (trimmed === '') {
      setError('Device name cannot be empty.');
      return;
    }
    if (!dirty) return; // no-op — nothing changed
    setSaving(true);
    setError(null);
    try {
      const res = await onRename(deviceId, trimmed);
      if (res === SIGNED_OUT_HEALED) {
        onSignedOut();
        return;
      }
      onRenamed(trimmed);
    } catch (err) {
      // Duplicate name → `name already in use`; other hub errors surface too.
      if (mounted.current) setError(accountErrMsg(err));
    } finally {
      if (mounted.current) setSaving(false);
    }
  };

  return (
    <div className="space-y-1.5">
      <label className="flex items-center gap-1.5 text-sm font-medium text-content-secondary">
        <Pencil size={13} className="flex-shrink-0" />
        Device name
      </label>
      <div className="flex items-center gap-2 max-w-md">
        <input
          type="text"
          value={value}
          onChange={(e) => {
            setValue(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              void handleSave();
            }
          }}
          placeholder="This machine's name"
          spellCheck={false}
          autoComplete="off"
          className="flex-1 min-w-0 bg-surface-hover border border-border rounded-lg px-3 py-1.5 text-sm text-content focus:outline-none focus:border-accent"
        />
        <button
          type="button"
          onClick={handleSave}
          disabled={saving || !dirty || trimmed === ''}
          className="flex-shrink-0 inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
        >
          {saving ? <Loader2 size={13} className="animate-spin" /> : null}
          Save
        </button>
      </div>
      {error && <p className="text-xs text-error">{error}</p>}
      <p className="text-xs text-content-muted">
        Shown to your other devices in sync history and transfers. Must be unique across your
        account.
      </p>
    </div>
  );
}

/**
 * Hub selector — Production vs Test registry, shown in ALL builds (this is the
 * beta's prod/dev toggle). Writes `account.hub_url` and re-polls status.
 * Rendered only while signed out: the hub choice decides which account/device
 * registry sign-in talks to, and device tokens are stored per hub host, so
 * flipping back and forth never clobbers the other hub's sign-in.
 */
function HubSelector({ onSaved }: { onSaved: () => Promise<unknown> }) {
  const [current, setCurrent] = useState<string>(DEFAULT_HUB_URL);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const raw = await api.invoke<string | null>('get_setting', { key: 'account.hub_url' });
        if (mounted.current && raw != null && raw !== '') setCurrent(raw);
      } catch (err) {
        console.error('[account] load hub url failed:', err);
      }
    })();
    return () => {
      mounted.current = false;
    };
  }, []);

  const choose = async (url: string) => {
    if (busy || url === current) return;
    setBusy(true);
    setError(null);
    try {
      await api.invoke('set_setting', { key: 'account.hub_url', value: url });
      if (mounted.current) setCurrent(url);
      await onSaved(); // re-poll account_status so the sign-in form shows the new hub
    } catch (err) {
      console.error('[account] switch hub failed:', err);
      if (mounted.current) setError(accountErrMsg(err));
    } finally {
      if (mounted.current) setBusy(false);
    }
  };

  const isCustom = current !== PROD_HUB_URL && current !== TEST_HUB_URL;
  const pill = (url: string, label: string) => (
    <button
      key={url}
      type="button"
      onClick={() => choose(url)}
      disabled={busy}
      className={`px-3 py-1.5 rounded-md text-xs border transition-colors disabled:opacity-50 ${
        current === url
          ? 'border-accent bg-accent-muted/20 text-content'
          : 'border-border text-content-secondary hover:bg-surface-hover'
      }`}
    >
      {label}
    </button>
  );

  return (
    <div className="space-y-1.5 border-t border-border/50 pt-4">
      <label className="block text-xs text-content-muted">Hub</label>
      <div className="flex items-center gap-2">
        {pill(PROD_HUB_URL, 'Production')}
        {pill(TEST_HUB_URL, 'Test')}
        {busy && <Loader2 size={13} className="animate-spin text-content-muted" />}
      </div>
      {isCustom && (
        <p className="text-xs text-content-muted">
          Custom hub: <span className="font-mono text-content-secondary">{current}</span>
        </p>
      )}
      {error && <p className="text-xs text-error">{error}</p>}
      <p className="text-xs text-content-muted">
        Production is where your real devices live; Test is a separate registry for trying
        things out. Each hub keeps its own sign-in, so switching is safe.
      </p>
    </div>
  );
}

/**
 * Dev-only editor for `account.hub_url` — lets a developer point sign-in at a
 * different hub before signing in. Gated on `import.meta.env.DEV` by the caller,
 * so it is statically tree-shaken out of production builds. Reads the current
 * value on mount; saves on an explicit button (never per-keystroke). Empty input
 * + Save resets to the default hub. After a save it re-polls status via
 * `onSaved` so the signed-in card's read-only hub URL reflects the change.
 */
function HubUrlDevEditor({ onSaved }: { onSaved: () => Promise<unknown> }) {
  const [value, setValue] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    (async () => {
      try {
        const raw = await api.invoke<string | null>('get_setting', { key: 'account.hub_url' });
        // null/empty → leave the field blank so the placeholder shows the default.
        if (mounted.current && raw != null && raw !== '') setValue(raw);
      } catch (err) {
        console.error('[account] load hub url failed:', err);
      }
    })();
    return () => {
      mounted.current = false;
    };
  }, []);

  const handleSave = async () => {
    if (saving) return;
    const trimmed = value.trim();
    // Empty + Save resets to the default hub URL.
    const next = trimmed === '' ? DEFAULT_HUB_URL : trimmed;
    if (!next.startsWith('http://') && !next.startsWith('https://')) {
      setError('Hub URL must start with http:// or https://');
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await api.invoke('set_setting', { key: 'account.hub_url', value: next });
      if (mounted.current) setValue(next);
      await onSaved(); // re-poll account_status so the displayed hubUrl refreshes
    } catch (err) {
      console.error('[account] save hub url failed:', err);
      if (mounted.current) setError(accountErrMsg(err));
    } finally {
      if (mounted.current) setSaving(false);
    }
  };

  return (
    <div className="space-y-1.5 border-t border-border/50 pt-4">
      <label className="block text-xs text-content-muted">Hub URL (dev)</label>
      <div className="flex items-center gap-2">
        <input
          type="text"
          value={value}
          onChange={(e) => {
            setValue(e.target.value);
            setError(null);
          }}
          placeholder={DEFAULT_HUB_URL}
          spellCheck={false}
          autoComplete="off"
          className="flex-1 min-w-0 bg-surface-hover border border-border rounded-lg px-3 py-1.5 text-xs font-mono text-content focus:outline-none focus:border-accent"
        />
        <button
          type="button"
          onClick={handleSave}
          disabled={saving}
          className="flex-shrink-0 inline-flex items-center gap-1 rounded-md border border-border px-3 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
        >
          {saving ? <Loader2 size={13} className="animate-spin" /> : null}
          Save
        </button>
      </div>
      {error && <p className="text-xs text-error">{error}</p>}
      <p className="text-xs text-content-muted">
        Dev only. Points sign-in at a different hub. Empty + Save resets to the default.
      </p>
    </div>
  );
}

/**
 * Settings → Account. Renders inner content only (the card/heading are supplied
 * by the host in `Settings.tsx`, matching the `LoggingSettings` pattern).
 *
 * Three states: loading, signed-out (email → code sign-in), signed-in (account
 * card + device-name editor + device list). All account state is owned by
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
    renameDevice,
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
  const capability = status?.capability ?? 'athenaeum';
  const thisDevice = devices.find((d) => d.id === deviceId);

  // Reset the sign-in form each time we (re-)enter the signed-out state so a
  // stale code/step never wedges a fresh sign-in.
  useEffect(() => {
    if (!signedIn) {
      setStep('email');
      setCode('');
      setSignInError(null);
    }
  }, [signedIn]);

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

  // Success path for the inline device-name editor — the editor owns its own
  // input/saving/error state; the parent just raises the outcome notification.
  const handleRenamed = (name: string) => {
    notify({
      title: 'Device renamed',
      detail: `This device is now named "${name}".`,
      kind: 'generic',
      tone: 'success',
    });
  };

  const handleRenameSignedOut = () => {
    notify({
      title: 'Signed out',
      detail: 'This device was signed out. Please sign in again.',
      kind: 'generic',
      tone: 'warning',
    });
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

        <HubSelector onSaved={refreshStatus} />
        {import.meta.env.DEV && <HubUrlDevEditor onSaved={refreshStatus} />}
      </div>
    );
  }

  // SIGNED IN — account card, device-name editor, device list.
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
            </div>
            <div className="flex items-center gap-2 text-sm text-content-muted">
              <Network size={14} className="flex-shrink-0" />
              Capability:
              <CapabilityPill capability={capability} />
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

      {/* Device name */}
      {deviceId && (
        <DeviceNameEditor
          key={deviceId}
          deviceId={deviceId}
          initialName={thisDevice?.name ?? ''}
          onRename={renameDevice}
          onRenamed={handleRenamed}
          onSignedOut={handleRenameSignedOut}
        />
      )}

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
                  <th className="px-3 py-2 font-medium">Capability</th>
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
                        <CapabilityPill capability={d.capability} />
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
