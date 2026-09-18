// Settings redesign (spec 2026-09-18 §7, plan Task E1 Step 4).
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { SearchResults } from './SearchResults';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../settings/SettingsDefaultsContext';
import { api } from '../../api';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS_FIXTURE = {
  kv: {
    session_gap_threshold_hours: '6',
    'blink.resolution': 'preview',
    'rustafits.quality.thumbnail': '70',
    'rustafits.quality.preview': '85',
    'rustafits.quality.full': '95',
    'blink.threads': '4',
    'blink.memory_cache_size': '200',
    'blink.memory_cache_max_mb': '512',
    'blink.memory_retention_minutes': '30',
  },
} as unknown as SettingsDefaults;

function mockInvoke(command: string): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      return Promise.resolve(DEFAULTS_FIXTURE);
    case 'get_setting':
      return Promise.resolve('');
    case 'get_blink_threads_max':
      return Promise.resolve(8);
    default:
      return Promise.resolve(null);
  }
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(mockInvoke as never);
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

function renderResults(query: string) {
  return render(
    <SettingsDefaultsProvider>
      <SearchResults query={query} />
    </SettingsDefaultsProvider>,
  );
}

describe('SearchResults', () => {
  it("'gap' renders exactly the Session detection section", async () => {
    renderResults('gap');

    expect(await screen.findByText('Session detection')).toBeInTheDocument();
    expect(screen.getByText('Session gap threshold (hours)')).toBeInTheDocument();

    // No other section's own title rendered alongside it.
    expect(screen.queryByText('Updates')).not.toBeInTheDocument();
    expect(screen.queryByText('Monitoring')).not.toBeInTheDocument();
  });

  it("'blink jpeg' renders the viewer section with the quality fields highlighted", async () => {
    renderResults('blink jpeg');

    expect(await screen.findByText('Blink viewer')).toBeInTheDocument();
    // Only ONE quality slider shows at a time (fix round Item 2) — the
    // default resolution is "preview".
    const qualityLabel = await screen.findByText('Preview JPEG Quality');
    expect(qualityLabel).toBeInTheDocument();

    // Its label row carries the search-highlight ring.
    const labelRow = qualityLabel.closest('div');
    expect(labelRow?.className).toContain('ring-1');
    expect(labelRow?.className).toContain('ring-accent/60');
  });

  it('shows "No settings match." for a query nothing matches', () => {
    renderResults('zzzznosuchsetting');
    expect(screen.getByText('No settings match.')).toBeInTheDocument();
  });
});
