import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { AssistantAccessSection } from './AssistantAccessSection';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import { api } from '../../../api';
import { routeInvoke } from '../../../test/apiMock';
import type { McpStatus } from '../../../types/mcp';

vi.mock('../../../api');

const platform = vi.hoisted(() => ({ tauri: true }));
vi.mock('../../../utils/platform', () => ({
  get isTauri() {
    return platform.tauri;
  },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS_FIXTURE = { kv: { 'mcp.enabled': 'false' } } as unknown as SettingsDefaults;

const OFF: McpStatus = {
  enabled: false,
  state: 'off',
  serverName: 'athenaeum-dev',
  endpoint: null,
  token: null,
  bridgePath: null,
  bridgeFound: false,
  clientConfig: null,
  claudeCodeCommand: null,
  lastError: null,
  blockedReason: null,
};
const BRIDGE = '/Applications/Athenaeum.app/Contents/MacOS/athenaeum-mcp';
const DESKTOP_ON: McpStatus = {
  ...OFF,
  enabled: true,
  state: 'serving',
  endpoint: 'unix:/tmp/athenaeum.sock',
  bridgePath: BRIDGE,
  bridgeFound: true,
  clientConfig: `{"mcpServers":{"athenaeum-dev":{"command":"${BRIDGE}","args":["--build","dev","--app-data-dir","/data"]}}}`,
  claudeCodeCommand: `claude mcp add athenaeum-dev -- "${BRIDGE}" --build dev --app-data-dir "/data"`,
};

let current: McpStatus = OFF;

function callsOf(command: string) {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === command);
}

function route(extra: Record<string, unknown> = {}) {
  routeInvoke({
    get_settings_defaults: DEFAULTS_FIXTURE,
    get_mcp_status: () => current,
    set_mcp_enabled: (a: { enabled: boolean }) => {
      current = a.enabled ? DESKTOP_ON : OFF;
      return current;
    },
    regenerate_mcp_token: () => current,
    ...extra,
  });
}

beforeEach(() => {
  platform.tauri = true;
  current = OFF;
  notifyMock.mockReset();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockResolvedValue(() => {});
  route();
});

async function renderSection(status: McpStatus = OFF) {
  current = status;
  const utils = render(
    <SettingsDefaultsProvider>
      <AssistantAccessSection />
    </SettingsDefaultsProvider>,
  );
  await screen.findByText('Allow assistant access');
  await waitFor(() => expect(callsOf('get_mcp_status').length).toBeGreaterThan(0));
  // Let the status read settle into the state.
  await new Promise((r) => setTimeout(r, 0));
  return utils;
}

describe('AssistantAccessSection', () => {
  it('off_shows_only_the_toggle', async () => {
    await renderSection(OFF);
    const toggle = screen.getByRole('checkbox') as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    expect(screen.queryByText(/Regenerate access key/)).toBeNull();
    expect(screen.queryByText(/athenaeum-mcp/)).toBeNull();
    expect(screen.queryByText(/Copy/)).toBeNull();
  });

  it('web_without_api_key_says_why_and_disables_the_toggle', async () => {
    platform.tauri = false;
    const why = 'Assistant access on the web needs an API key; none is configured on this server.';
    await renderSection({ ...OFF, blockedReason: why });
    expect(await screen.findByText(new RegExp(why.slice(0, 30)))).toBeTruthy();
    expect(screen.getByText(/ATHENAEUM_API_KEY/)).toBeTruthy();
    expect((screen.getByRole('checkbox') as HTMLInputElement).disabled).toBe(true);
    expect(screen.queryByText(/mcpServers/)).toBeNull();
  });

  it('translocated_path_warns', async () => {
    const path = '/private/var/folders/x/AppTranslocation/ABC/d/Athenaeum.app/Contents/MacOS/athenaeum-mcp';
    await renderSection({ ...DESKTOP_ON, bridgePath: path });
    expect(await screen.findByText(/running from a temporary location \(macOS App Translocation\)/)).toBeTruthy();
    expect(screen.getByText(/Move Athenaeum to the Applications folder, restart it, then copy the configuration again\./)).toBeTruthy();
  });

  it('a normal path has no translocation warning', async () => {
    await renderSection(DESKTOP_ON);
    expect(await screen.findByText(BRIDGE)).toBeTruthy();
    expect(screen.queryByText(/App Translocation/)).toBeNull();
  });

  it('a missing bridge says how to build it', async () => {
    await renderSection({ ...DESKTOP_ON, bridgeFound: false });
    expect(await screen.findByText(/not found/)).toBeTruthy();
    expect(screen.getByText(/cargo build -p athenaeum-mcp/)).toBeTruthy();
  });

  it('elsewhere_and_failed_have_their_copy', async () => {
    const { unmount } = await renderSection({ ...DESKTOP_ON, state: 'elsewhere' });
    expect(
      await screen.findByText('Another Athenaeum of this build is already serving the assistant on this computer; this one is not.'),
    ).toBeTruthy();
    unmount();
    await renderSection({ ...DESKTOP_ON, state: 'failed', lastError: 'address in use' });
    expect(await screen.findByText('Assistant access could not start: address in use')).toBeTruthy();
  });

  it('turning_on_calls_set_mcp_enabled_and_shows_the_config', async () => {
    await renderSection(OFF);
    fireEvent.click(screen.getByRole('checkbox'));
    await waitFor(() => expect(callsOf('set_mcp_enabled')).toHaveLength(1));
    expect(callsOf('set_mcp_enabled')[0]![1]).toEqual({ enabled: true });
    expect(callsOf('set_setting')).toHaveLength(0);
    const blocks = await screen.findAllByText(/athenaeum-dev/, { selector: 'pre' });
    expect(blocks).toHaveLength(2); // the client configuration and the Claude Code command
    for (const block of blocks) expect(block.textContent).toContain('--build');
  });

  it('a_failure_notifies_once', async () => {
    route({
      set_mcp_enabled: () => Promise.reject(new Error('the channel could not start')),
    });
    await renderSection(OFF);
    fireEvent.click(screen.getByRole('checkbox'));
    await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 50));
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0]![0]).toMatchObject({ tone: 'warning', dedupeKey: 'mcp-settings' });
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('web_on_shows_the_endpoint_the_key_and_the_config', async () => {
    platform.tauri = false;
    await renderSection({ ...OFF, enabled: true, state: 'serving', serverName: 'athenaeum', token: 'KEY123' });
    const url = `${window.location.origin}/mcp`;
    expect((await screen.findAllByText(new RegExp(url.replace(/[/.]/g, '\\$&')))).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/KEY123/).length).toBeGreaterThan(0);
    const config = await screen.findByText(/mcpServers/, { selector: 'pre' });
    expect(JSON.parse(config.textContent ?? '')).toEqual({
      mcpServers: { athenaeum: { type: 'http', url, headers: { Authorization: 'Bearer KEY123' } } },
    });
  });

  it('regenerate_calls_its_command', async () => {
    await renderSection(DESKTOP_ON);
    fireEvent.click(await screen.findByRole('button', { name: 'Regenerate access key' }));
    await waitFor(() => expect(callsOf('regenerate_mcp_token')).toHaveLength(1));
  });
});
