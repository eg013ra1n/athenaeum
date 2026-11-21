// General tab, "Assistant access (MCP)" (plan 2026-11-19 T9, §8). Off by default. The toggle is a KV toggle whose
// read and write go through the MCP commands, never `set_setting` (the backend refuses `mcp.enabled`): turning it on
// starts the host, and the answer (`McpStatus`) carries what an assistant needs — on the desktop the bridge program
// and the config to paste, on the web the endpoint and the key.
import { useCallback, useRef, useState } from 'react';
import { AlertTriangle, Check, Copy } from 'lucide-react';
import { api } from '../../../api';
import { isTauri } from '../../../utils/platform';
import { useNotifications } from '../../../contexts/NotificationContext';
import { fieldMeta } from '../../../settings/registry';
import { Button } from '../../ui/Button';
import type { ClaudeDesktopAdded, McpStatus } from '../../../types/mcp';
import { SettingsSection } from '../SettingsSection';
import { SettingToggle } from '../SettingToggle';
import { SavedTick } from '../ResetButton';

const SECTION = 'general.assistantAccess';
const NOTIFY_KEY = 'mcp-settings';

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle');
  const copy = async () => {
    // Never claim success without actually copying (same rule as the transfer ticket button).
    let ok = false;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
        ok = true;
      }
    } catch (err) {
      console.error('[mcp-settings] copy failed:', err);
    }
    if (!ok) console.error('[mcp-settings] copy failed: clipboard unavailable');
    setState(ok ? 'copied' : 'failed');
    setTimeout(() => setState('idle'), 2000);
  };
  return (
    <Button size="sm" onClick={() => void copy()} aria-label={label}>
      {state === 'copied' ? <Check size={12} /> : <Copy size={12} />}
      {state === 'copied' ? 'Copied' : state === 'failed' ? 'Copy failed' : 'Copy'}
    </Button>
  );
}

function CopyBlock({ title, text, label }: { title: string; text: string; label: string }) {
  return (
    <div className="space-y-1">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs text-content-muted">{title}</span>
        <CopyButton text={text} label={label} />
      </div>
      <pre className="bg-surface-secondary rounded p-3 text-xs font-mono text-content whitespace-pre overflow-x-auto">{text}</pre>
    </div>
  );
}

export function AssistantAccessSection() {
  const { notify } = useNotifications();
  const [status, setStatus] = useState<McpStatus | null>(null);
  // A failed write leaves the toggle showing a value that was not stored: remounting it re-reads the real one.
  const [toggleKey, setToggleKey] = useState(0);
  const notifyRef = useRef(notify);
  notifyRef.current = notify;

  const fail = useCallback((what: string, err: unknown) => {
    console.error(`[mcp-settings] ${what} failed:`, err);
    notifyRef.current({
      kind: 'generic',
      tone: 'warning',
      title: 'Assistant access not changed',
      detail: errMsg(err),
      dedupeKey: NOTIFY_KEY,
    });
  }, []);

  const read = useCallback(async (): Promise<string> => {
    const s = await api.invoke<McpStatus>('get_mcp_status');
    setStatus(s);
    return String(s.enabled);
  }, []);

  const write = useCallback(
    async (enabled: boolean): Promise<void> => {
      try {
        setStatus(await api.invoke<McpStatus>('set_mcp_enabled', { enabled }));
      } catch (err) {
        // Handled here (one notification, our own dedupe key): the toggle must not raise a second one.
        fail('set_mcp_enabled', err);
        setToggleKey((k) => k + 1);
      }
    },
    [fail],
  );

  const regenerate = async () => {
    try {
      setStatus(await api.invoke<McpStatus>('regenerate_mcp_token'));
    } catch (err) {
      fail('regenerate_mcp_token', err);
    }
  };

  // The write runs once per click: the ref closes the gap before the disabled state renders.
  const [adding, setAdding] = useState(false);
  const addingRef = useRef(false);
  const [added, setAdded] = useState<{ result: ClaudeDesktopAdded; at: number } | null>(null);
  const addToClaudeDesktop = async () => {
    if (addingRef.current) return;
    addingRef.current = true;
    setAdding(true);
    try {
      const result = await api.invoke<ClaudeDesktopAdded>('add_mcp_to_claude_desktop');
      setAdded({ result, at: Date.now() });
    } catch (err) {
      setAdded(null);
      fail('add_mcp_to_claude_desktop', err);
    } finally {
      addingRef.current = false;
      setAdding(false);
    }
  };

  const blocked = status?.blockedReason ?? null;
  const on = status?.enabled === true;
  const web = !isTauri;
  const translocated = (status?.bridgePath ?? '').includes('/AppTranslocation/');
  const webUrl = `${window.location.origin}/mcp`;
  const webConfig =
    web && status?.token
      ? JSON.stringify(
          {
            mcpServers: {
              [status.serverName]: { type: 'http', url: webUrl, headers: { Authorization: `Bearer ${status.token}` } },
            },
          },
          null,
          2,
        )
      : null;

  return (
    <SettingsSection id={SECTION}>
      <div className="space-y-4">
        <SettingToggle
          key={toggleKey}
          section={SECTION}
          field="enabled"
          settingKey="mcp.enabled"
          disabled={blocked !== null}
          read={read}
          write={write}
        />

        {blocked && (
          <div className="flex items-start gap-2 text-sm text-warning">
            <AlertTriangle size={16} className="mt-0.5 shrink-0" />
            <p>{blocked}</p>
          </div>
        )}

        {on && status && !blocked && (
          <div className="space-y-4">
            {status.state === 'starting' && <p className="text-sm text-content-muted">Starting assistant access…</p>}
            {status.state === 'elsewhere' && (
              <p className="text-sm text-warning">
                Another Athenaeum of this build is already serving the assistant on this computer; this one is not.
              </p>
            )}
            {status.state === 'failed' && (
              <p className="text-sm text-error">{`Assistant access could not start: ${status.lastError ?? 'unknown error'}`}</p>
            )}

            {web ? (
              <>
                <div className="bg-surface-secondary rounded p-3 text-xs font-mono space-y-2">
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted min-w-[70px]">Endpoint:</span>
                    <span className="text-content break-all flex-1">{webUrl}</span>
                    <CopyButton text={webUrl} label="Copy endpoint" />
                  </div>
                  {status.token && (
                    <div className="flex items-center gap-2">
                      <span className="text-content-muted min-w-[70px]">Key:</span>
                      <span className="text-content break-all flex-1">{status.token}</span>
                      <CopyButton text={status.token} label="Copy key" />
                    </div>
                  )}
                </div>
                {webConfig && <CopyBlock title="Client configuration" text={webConfig} label="Copy client configuration" />}
              </>
            ) : (
              <>
                <div className="bg-surface-secondary rounded p-3 text-xs font-mono space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="text-content-muted min-w-[70px]">Bridge:</span>
                    {status.bridgePath && status.bridgeFound ? (
                      <span className="text-content break-all flex-1">{status.bridgePath}</span>
                    ) : (
                      <span className="text-content-muted flex-1">
                        {status.serverName.endsWith('-dev') ? (
                          <>
                            not found — build it with <code>cargo build -p athenaeum-mcp</code>
                          </>
                        ) : (
                          'not found — reinstall Athenaeum to restore it'
                        )}
                      </span>
                    )}
                  </div>
                </div>
                {translocated && (
                  <div className="flex items-start gap-2 text-sm text-warning">
                    <AlertTriangle size={16} className="mt-0.5 shrink-0" />
                    <p>
                      Athenaeum is running from a temporary location (macOS App Translocation), so this path changes at
                      every launch. Move Athenaeum to the Applications folder, restart it, then copy the configuration
                      again.
                    </p>
                  </div>
                )}
                {status.bridgeFound && !translocated && status.claudeDesktopConfig && (
                  <div className="space-y-1">
                    <div className="flex items-center gap-2">
                      <Button onClick={() => void addToClaudeDesktop()} disabled={adding}>
                        {fieldMeta(SECTION, 'addToClaudeDesktop').label}
                      </Button>
                      <SavedTick savedAt={added?.at ?? null} />
                    </div>
                    <p className="text-xs text-content-muted">{fieldMeta(SECTION, 'addToClaudeDesktop').help}</p>
                    {added && (
                      <p className="text-xs text-content-muted break-all">
                        {`Added to ${added.result.path}. Restart Claude desktop to load it.${added.result.replaced ? ' Replaced the earlier entry.' : ''}`}
                      </p>
                    )}
                  </div>
                )}
                {status.bridgeFound && status.clientConfig && (
                  <CopyBlock title="Client configuration" text={status.clientConfig} label="Copy client configuration" />
                )}
                {status.bridgeFound && status.claudeCodeCommand && (
                  <CopyBlock title="Claude Code command" text={status.claudeCodeCommand} label="Copy Claude Code command" />
                )}
              </>
            )}

            <div className="space-y-1">
              <Button onClick={() => void regenerate()}>{fieldMeta(SECTION, 'regenerate').label}</Button>
              <p className="text-xs text-content-muted">{fieldMeta(SECTION, 'regenerate').help}</p>
            </div>
          </div>
        )}
      </div>
    </SettingsSection>
  );
}
