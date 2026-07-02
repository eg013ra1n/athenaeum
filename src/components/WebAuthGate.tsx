import { useEffect, useState, type FormEvent, type ReactNode } from 'react';
import { probeAuth, setApiKey } from '../api/http';

type GateStatus = 'checking' | 'ok' | 'unauthorized' | 'unreachable';

/**
 * Web-only login gate for the opt-in `ATHENAEUM_API_KEY` backend auth
 * (see crates/athenaeum-web/src/routes/auth.rs). Probes the server on
 * mount; children (Layout, pages, hooks that call api.listen) are not
 * mounted until the probe succeeds. This is what prevents every SSE
 * listener from opening its own EventSource against a 401 endpoint and
 * auto-reconnect-spamming it.
 *
 * If the server has no key configured, probeAuth() resolves 'ok'
 * immediately and this renders exactly like it isn't here.
 */
export default function WebAuthGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<GateStatus>('checking');
  const [keyInput, setKeyInput] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const runProbe = () => {
    setStatus('checking');
    probeAuth()
      .then((result) => setStatus(result))
      .catch((err) => {
        console.error('[WebAuthGate] auth probe failed:', err);
        setStatus('unreachable');
      });
  };

  useEffect(() => {
    runProbe();
  }, []);

  const handleSubmit = async (e: FormEvent) => {
    e.preventDefault();
    if (!keyInput.trim() || submitting) return;
    setSubmitting(true);
    setError(null);
    setApiKey(keyInput);
    try {
      const result = await probeAuth();
      if (result === 'ok') {
        setStatus('ok');
      } else {
        setApiKey(null);
        setError('Invalid API key');
      }
    } catch (err) {
      console.error('[WebAuthGate] auth probe failed:', err);
      setApiKey(null);
      setError('Server unreachable — try again');
    } finally {
      setSubmitting(false);
    }
  };

  if (status === 'ok') {
    return <>{children}</>;
  }

  if (status === 'checking') {
    return <div className="min-h-screen bg-surface" />;
  }

  if (status === 'unreachable') {
    return (
      <div className="min-h-screen bg-surface flex items-center justify-center">
        <div className="w-full max-w-sm mx-4 text-center">
          <h1 className="text-lg font-medium text-content mb-2">Server unreachable</h1>
          <p className="text-content-muted text-sm mb-4">
            Could not reach the Athenaeum server. Check your connection and try again.
          </p>
          <button
            onClick={runProbe}
            className="px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded transition-colors"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-surface flex items-center justify-center">
      <div className="w-full max-w-sm mx-4">
        <div className="bg-surface-elevated border border-border rounded-lg p-6 shadow-xl">
          <h1 className="text-lg font-medium text-content mb-1 text-center">Athenaeum</h1>
          <p className="text-content-muted text-sm mb-4 text-center">
            This server requires an API key.
          </p>
          <form onSubmit={handleSubmit}>
            <input
              type="password"
              autoFocus
              value={keyInput}
              onChange={(e) => setKeyInput(e.target.value)}
              placeholder="API key"
              className="w-full px-3 py-2 bg-surface border border-border rounded font-mono text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            {error && <p className="text-error text-sm mt-2">{error}</p>}
            <button
              type="submit"
              disabled={!keyInput.trim() || submitting}
              className="w-full mt-4 px-4 py-2 bg-accent hover:bg-accent-hover disabled:opacity-50 disabled:cursor-not-allowed text-white rounded transition-colors"
            >
              {submitting ? 'Checking…' : 'Continue'}
            </button>
          </form>
        </div>
      </div>
    </div>
  );
}
