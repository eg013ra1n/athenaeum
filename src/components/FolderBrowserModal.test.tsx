// The folder browser: folder mode is unchanged; file mode lists files, selects one and sends the filter.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { FolderBrowserModal } from './FolderBrowserModal';
import { api } from '../api';
import { routeInvoke } from '../test/apiMock';

vi.mock('../api');
afterEach(cleanup);

const FILTERS = [
  { name: 'FITS/XISF Files', extensions: ['fits', 'fit', 'xisf'] },
  { name: 'All Files', extensions: ['*'] },
];

const listing = {
  current: '/data/m31',
  parent: '/data',
  directories: [{ name: 'sub', path: '/data/m31/sub' }],
  files: [{ name: 'M31.fits', path: '/data/m31/M31.fits', size: 1536 }],
};

function browseCalls(): Record<string, unknown>[] {
  return vi
    .mocked(api.invoke)
    .mock.calls.filter(([cmd]) => cmd === 'browse_directories')
    .map(([, args]) => args as Record<string, unknown>);
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
});

describe('FolderBrowserModal', () => {
  it('folder mode sends no file keys and Select returns the current folder', async () => {
    routeInvoke({ browse_directories: { current: '/data/m31', parent: '/data', directories: [] } });
    const onSelect = vi.fn();
    render(<FolderBrowserModal isOpen scope="scan" onSelect={onSelect} onClose={vi.fn()} />);
    await screen.findByText('No subdirectories');
    const request = browseCalls()[0]!;
    expect(request).toEqual({ path: undefined, scope: 'scan' });
    expect(request).not.toHaveProperty('includeFiles');
    expect(request).not.toHaveProperty('extensions');
    expect(screen.getByText('Select Directory')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Select' }));
    expect(onSelect).toHaveBeenCalledWith('/data/m31');
  });

  it('file mode asks for files with the extension filter and shows name and size', async () => {
    routeInvoke({ browse_directories: listing });
    render(
      <FolderBrowserModal isOpen mode="file" scope="scan" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />,
    );
    await screen.findByText('M31.fits');
    expect(browseCalls()[0]).toMatchObject({ scope: 'scan', includeFiles: true, extensions: ['fits', 'fit', 'xisf'] });
    expect(screen.getByText('1.5 KB')).toBeTruthy();
    expect(screen.getByText('Select File')).toBeTruthy();
  });

  it('Select is disabled until a file is clicked, then returns its path', async () => {
    routeInvoke({ browse_directories: listing });
    const onSelect = vi.fn();
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={onSelect} onClose={vi.fn()} />);
    const row = await screen.findByText('M31.fits');
    const select = screen.getByRole('button', { name: 'Select' });
    expect(select).toBeDisabled();
    fireEvent.click(row);
    expect(select).not.toBeDisabled();
    expect(screen.getAllByText('/data/m31/M31.fits').length).toBeGreaterThan(0);
    fireEvent.click(select);
    expect(onSelect).toHaveBeenCalledWith('/data/m31/M31.fits');
  });

  it('navigating into a folder clears the selection', async () => {
    routeInvoke({ browse_directories: listing });
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
    fireEvent.click(await screen.findByText('M31.fits'));
    expect(screen.getByRole('button', { name: 'Select' })).not.toBeDisabled();
    fireEvent.click(screen.getByText('sub'));
    await waitFor(() => expect(browseCalls()).toHaveLength(2));
    expect(browseCalls()[1]).toMatchObject({ path: '/data/m31/sub' });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Select' })).toBeDisabled());
    expect(screen.getByText('Select a file')).toBeTruthy();
  });

  it('double-click on a file confirms it', async () => {
    routeInvoke({ browse_directories: listing });
    const onSelect = vi.fn();
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={onSelect} onClose={vi.fn()} />);
    fireEvent.doubleClick(await screen.findByText('M31.fits'));
    expect(onSelect).toHaveBeenCalledWith('/data/m31/M31.fits');
  });

  it('All Files re-requests the same folder without extensions', async () => {
    routeInvoke({ browse_directories: listing });
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
    await screen.findByText('M31.fits');
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '1' } });
    await waitFor(() => expect(browseCalls()).toHaveLength(2));
    expect(browseCalls()[1]).toMatchObject({ path: '/data/m31', includeFiles: true });
    expect(browseCalls()[1]).not.toHaveProperty('extensions');
  });

  it('shows "No matching files" when file mode finds nothing', async () => {
    routeInvoke({ browse_directories: { current: '/data/m31', parent: '/data', directories: [], files: [] } });
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
    expect(await screen.findByText('No matching files')).toBeTruthy();
  });

  it('a rejected listing shows the server text', async () => {
    routeInvoke({ browse_directories: () => Promise.reject('Path is outside allowed directories') });
    render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
    expect(await screen.findByText('Path is outside allowed directories')).toBeTruthy();
  });
});
