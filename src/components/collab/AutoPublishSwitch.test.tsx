import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import AutoPublishSwitch from './AutoPublishSwitch';
import { api } from '../../api';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(null)) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

describe('AutoPublishSwitch', () => {
  it('writes the local preference and re-reads it', async () => {
    const onToggled = vi.fn();
    render(<AutoPublishSwitch projectId="proj-1" enabled={false} onToggled={onToggled} />);
    fireEvent.click(screen.getByRole('checkbox', { name: /Auto-publish my frames/ }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_project_auto_publish', { projectId: 'proj-1', enabled: true }),
    );
    expect(onToggled).toHaveBeenCalledTimes(1);
  });
});
