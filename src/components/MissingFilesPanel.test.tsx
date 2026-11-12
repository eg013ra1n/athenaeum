// The missing-files panel: Locate picks a path through the native dialog on the desktop and through the
// folder browser's file mode on the web, then relocates the file.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MissingFilesPanel } from './MissingFilesPanel';
import { api } from '../api';
import { pickFile } from '../api/desktop';
import { routeInvoke } from '../test/apiMock';
import type { MissingFileRecord } from '../types/helpers';

vi.mock('../api');
vi.mock('../api/desktop', () => ({ pickFile: vi.fn() }));
const platform = vi.hoisted(() => ({ tauri: true }));
vi.mock('../utils/platform', () => ({ get isTauri() { return platform.tauri; } }));
const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: notifyMock }) }));
vi.mock('./FolderBrowserModal', () => ({
  FolderBrowserModal: ({
    isOpen,
    mode,
    onSelect,
  }: {
    isOpen: boolean;
    mode?: string;
    onSelect: (path: string) => void;
  }) =>
    isOpen ? (
      <button data-testid="folder-browser" data-mode={mode} onClick={() => onSelect('/data/new/M31.fits')}>
        pick
      </button>
    ) : null,
}));
afterEach(cleanup);

const file: MissingFileRecord = {
  id: 1,
  file_id: 42,
  scan_root_id: 7,
  detected_at: '2026-11-01T00:00:00Z',
  last_checked_at: '2026-11-01T00:00:00Z',
  status: 'missing',
  path: '/data/old/M31.fits',
  filename: 'M31.fits',
  size: 2048,
  modified_at: '2026-10-01T00:00:00Z',
  has_frame: true,
  object: 'M31',
  date_obs: null,
};

beforeEach(() => {
  platform.tauri = true;
  notifyMock.mockReset();
  vi.mocked(pickFile).mockReset();
  vi.mocked(api.invoke).mockReset();
  routeInvoke({ relocate_missing_file: null });
});

function renderPanel(onRefresh = vi.fn()) {
  render(<MissingFilesPanel rootId={7} missingFiles={[file]} onRefresh={onRefresh} />);
  return onRefresh;
}

describe('MissingFilesPanel Locate', () => {
  it('web: Locate opens the folder browser in file mode and relocates the picked path', async () => {
    platform.tauri = false;
    const onRefresh = renderPanel();
    expect(screen.queryByTestId('folder-browser')).toBeNull();
    fireEvent.click(screen.getByTitle('Locate file'));
    const modal = screen.getByTestId('folder-browser');
    expect(modal.getAttribute('data-mode')).toBe('file');
    expect(pickFile).not.toHaveBeenCalled();
    fireEvent.click(modal);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('relocate_missing_file', { fileId: 42, newPath: '/data/new/M31.fits' }),
    );
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(screen.queryByTestId('folder-browser')).toBeNull();
  });

  it('web: a rejected relocate notifies', async () => {
    platform.tauri = false;
    routeInvoke({ relocate_missing_file: () => Promise.reject('File is outside the root') });
    const onRefresh = renderPanel();
    vi.spyOn(console, 'error').mockImplementation(() => {});
    fireEvent.click(screen.getByTitle('Locate file'));
    fireEvent.click(screen.getByTestId('folder-browser'));
    await waitFor(() =>
      expect(notifyMock).toHaveBeenCalledWith(
        expect.objectContaining({ title: 'Relocate failed', detail: 'File is outside the root' }),
      ),
    );
    expect(onRefresh).not.toHaveBeenCalled();
  });

  it('desktop: the native dialog gets the locate filters and the modal stays closed', async () => {
    vi.mocked(pickFile).mockResolvedValue('/data/new/M31.fits');
    const onRefresh = renderPanel();
    fireEvent.click(screen.getByTitle('Locate file'));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('relocate_missing_file', { fileId: 42, newPath: '/data/new/M31.fits' }),
    );
    expect(pickFile).toHaveBeenCalledWith({
      title: 'Locate M31.fits',
      filters: [
        { name: 'FITS/XISF Files', extensions: ['fits', 'fit', 'xisf', 'FITS', 'FIT', 'XISF'] },
        { name: 'All Files', extensions: ['*'] },
      ],
    });
    await waitFor(() => expect(onRefresh).toHaveBeenCalled());
    expect(screen.queryByTestId('folder-browser')).toBeNull();
  });

  it('desktop: a cancelled dialog invokes nothing', async () => {
    vi.mocked(pickFile).mockResolvedValue(null);
    renderPanel();
    fireEvent.click(screen.getByTitle('Locate file'));
    await waitFor(() => expect(pickFile).toHaveBeenCalled());
    expect(api.invoke).not.toHaveBeenCalled();
  });
});
