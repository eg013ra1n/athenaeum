import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import UpdateRequired from './UpdateRequired';

const { openAvailableMock } = vi.hoisted(() => ({ openAvailableMock: vi.fn() }));

vi.mock('../../contexts/UpdatesContext', () => ({
  useUpdates: () => ({ openAvailable: openAvailableMock }),
}));

describe('UpdateRequired', () => {
  it('renders the update-required notice and calls openAvailable on click', () => {
    render(<UpdateRequired />);

    expect(
      screen.getByText('This project hub needs a newer Athenaeum. Update to keep collaborating.'),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Update' }));

    expect(openAvailableMock).toHaveBeenCalledTimes(1);
  });
});
