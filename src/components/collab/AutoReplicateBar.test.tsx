import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import AutoReplicateBar from './AutoReplicateBar';
import { api } from '../../api';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function renderBar(onToggled = () => {}) {
  return render(
    <AutoReplicateBar
      projectId="proj-1"
      autoReplicate
      publishedBytes={2048}
      onToggled={onToggled}
    />,
  );
}

describe('AutoReplicateBar', () => {
  it('has no Sync now of its own — the one Sync now is the page header\'s', () => {
    renderBar();
    expect(screen.queryByRole('button', { name: /Sync now/ })).not.toBeInTheDocument();
    expect(screen.getByText('2.0 KB published')).toBeInTheDocument();
  });

  it('writes the local preference and re-reads it', async () => {
    const onToggled = vi.fn();
    renderBar(onToggled);
    fireEvent.click(screen.getByRole('checkbox', { name: /Auto-download contributions/ }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_project_auto_replicate', { projectId: 'proj-1', enabled: false }),
    );
    expect(onToggled).toHaveBeenCalledTimes(1);
  });

  it('no longer has an auto-publish switch — that moved to AutoPublishSwitch', () => {
    renderBar();
    expect(screen.queryByRole('checkbox', { name: /Auto-publish my frames/ })).not.toBeInTheDocument();
  });
});
