// The "Originals" line of a built master: originals inside a zip Athenaeum did not write are not offered
// "Archive originals to zip" (they are not ours to move).
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { api } from '../api';
import { MasterProvenanceBlock } from './CalibrationSetTable';
import type { MasterProvenanceInfo } from '../types/models';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));
vi.mock('../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: vi.fn() }) }));

const base: MasterProvenanceInfo = {
  masterSetId: 9,
  sourceSetId: 3,
  recipeJson: '{}',
  memberCount: 20,
  memberHash: 'h',
  createdAt: '2026-10-01T00:00:00Z',
  sourceFramesOnDisk: true,
  originalsArchived: false,
  archiveOperationId: null,
  archiveZipPath: null,
  archiveZipMissing: false,
  sourceFramesInForeignZip: false,
};

function provide(p: MasterProvenanceInfo) {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((async (cmd: string) => (cmd === 'get_master_provenance' ? p : null)) as never);
}

describe('MasterProvenanceBlock — originals line', () => {
  beforeEach(() => { vi.mocked(api.invoke).mockReset(); });

  it('offers Archive originals to zip for originals on disk', async () => {
    provide(base);
    render(<MasterProvenanceBlock setId={9} />);
    expect(await screen.findByRole('button', { name: 'Archive originals to zip' })).toBeInTheDocument();
  });

  it('does not offer it for originals that sit in a foreign zip', async () => {
    provide({ ...base, sourceFramesInForeignZip: true });
    render(<MasterProvenanceBlock setId={9} />);
    expect(await screen.findByText('in a zip file')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Archive originals to zip' })).toBeNull();
    // The rebuild button still follows `sourceFramesOnDisk`.
    expect(screen.getByRole('button', { name: 'Rebuild master' })).toBeEnabled();
  });
});
