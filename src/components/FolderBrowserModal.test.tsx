// The folder browser: folder mode is unchanged; file mode lists files, selects one and sends the filter.
import { act } from 'react';
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

  describe('ordering of answers', () => {
    const root = {
      current: '/data',
      parent: null,
      directories: [
        { name: 'dirA', path: '/data/dirA' },
        { name: 'dirB', path: '/data/dirB' },
      ],
      files: [],
    };
    const listingFor = (folder: string, file: string) => ({
      current: `/data/${folder}`,
      parent: '/data',
      directories: [],
      files: [{ name: file, path: `/data/${folder}/${file}`, size: 10 }],
    });

    /** Resolves/rejects a held listing later. */
    function hold<T>() {
      let resolve!: (value: T) => void;
      let reject!: (reason: unknown) => void;
      const promise = new Promise<T>((res, rej) => {
        resolve = res;
        reject = rej;
      });
      return { promise, resolve, reject };
    }

    it('a late answer of an older request changes nothing and keeps the selection', async () => {
      const slowA = hold<unknown>();
      routeInvoke({
        browse_directories: (args: { path?: string }) => {
          if (args.path === '/data/dirA') return slowA.promise;
          if (args.path === '/data/dirB') return Promise.resolve(listingFor('dirB', 'b1.fits'));
          return Promise.resolve(root);
        },
      });
      const onSelect = vi.fn();
      render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={onSelect} onClose={vi.fn()} />);
      fireEvent.click(await screen.findByText('dirA'));
      await waitFor(() => expect(browseCalls()).toHaveLength(2));
      // A is still loading; the user cannot click B in the spinner view, so go through the path box.
      const input = screen.getByPlaceholderText('Enter path and press Enter');
      fireEvent.change(input, { target: { value: '/data/dirB' } });
      fireEvent.keyDown(input, { key: 'Enter' });
      fireEvent.click(await screen.findByText('b1.fits'));
      expect(screen.getByRole('button', { name: 'Select' })).not.toBeDisabled();

      await act(async () => {
        slowA.resolve(listingFor('dirA', 'a1.fits'));
        await slowA.promise;
      });

      expect(screen.getByText('b1.fits')).toBeTruthy();
      expect(screen.queryByText('a1.fits')).toBeNull();
      expect((input as HTMLInputElement).value).toBe('/data/dirB');
      fireEvent.click(screen.getByRole('button', { name: 'Select' }));
      expect(onSelect).toHaveBeenCalledWith('/data/dirB/b1.fits');
    });

    it('a late rejection of a superseded request shows no error', async () => {
      const slowA = hold<unknown>();
      routeInvoke({
        browse_directories: (args: { path?: string }) => {
          if (args.path === '/data/dirA') return slowA.promise;
          if (args.path === '/data/dirB') return Promise.resolve(listingFor('dirB', 'b1.fits'));
          return Promise.resolve(root);
        },
      });
      const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
      render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
      fireEvent.click(await screen.findByText('dirA'));
      await waitFor(() => expect(browseCalls()).toHaveLength(2));
      const input = screen.getByPlaceholderText('Enter path and press Enter');
      fireEvent.change(input, { target: { value: '/data/dirB' } });
      fireEvent.keyDown(input, { key: 'Enter' });
      await screen.findByText('b1.fits');

      await act(async () => {
        slowA.reject('Path is outside allowed directories');
        await slowA.promise.catch(() => {});
      });

      expect(screen.queryByText('Path is outside allowed directories')).toBeNull();
      expect(screen.getByText('b1.fits')).toBeTruthy();
      expect(screen.queryByText('Loading...')).toBeNull();
      warn.mockRestore();
    });

    it('a filter change while a navigation is in flight keeps the newer listing', async () => {
      const slowNav = hold<unknown>();
      routeInvoke({
        browse_directories: (args: { path?: string; extensions?: string[] }) => {
          if (args.path === '/data/dirA') return slowNav.promise;
          if (args.path === '/data' && args.extensions === undefined && browseCalls().length > 2) {
            return Promise.resolve(listingFor('all', 'any.txt'));
          }
          return Promise.resolve(root);
        },
      });
      render(<FolderBrowserModal isOpen mode="file" fileFilters={FILTERS} onSelect={vi.fn()} onClose={vi.fn()} />);
      fireEvent.click(await screen.findByText('dirA'));
      await waitFor(() => expect(browseCalls()).toHaveLength(2));
      fireEvent.change(screen.getByRole('combobox'), { target: { value: '1' } });
      await screen.findByText('any.txt');

      await act(async () => {
        slowNav.resolve(listingFor('dirA', 'a1.fits'));
        await slowNav.promise;
      });

      expect(screen.getByText('any.txt')).toBeTruthy();
      expect(screen.queryByText('a1.fits')).toBeNull();
    });

    it('an answer that lands after the modal was closed and reopened is ignored', async () => {
      const slowFirst = hold<unknown>();
      let calls = 0;
      routeInvoke({
        browse_directories: () => {
          calls += 1;
          return calls === 1 ? slowFirst.promise : Promise.resolve(listingFor('dirB', 'b1.fits'));
        },
      });
      const props = { mode: 'file' as const, fileFilters: FILTERS, onSelect: vi.fn(), onClose: vi.fn() };
      const { rerender } = render(<FolderBrowserModal isOpen {...props} />);
      await waitFor(() => expect(calls).toBe(1));
      rerender(<FolderBrowserModal isOpen={false} {...props} />);
      rerender(<FolderBrowserModal isOpen {...props} />);
      await screen.findByText('b1.fits');

      await act(async () => {
        slowFirst.resolve(listingFor('dirA', 'a1.fits'));
        await slowFirst.promise;
      });

      expect(screen.getByText('b1.fits')).toBeTruthy();
      expect(screen.queryByText('a1.fits')).toBeNull();
    });

    it('an answer that lands while the modal is closed is dropped', async () => {
      const slowFirst = hold<unknown>();
      routeInvoke({ browse_directories: () => slowFirst.promise });
      const props = { mode: 'file' as const, fileFilters: FILTERS, onSelect: vi.fn(), onClose: vi.fn() };
      const { rerender } = render(<FolderBrowserModal isOpen {...props} />);
      await waitFor(() => expect(browseCalls()).toHaveLength(1));
      rerender(<FolderBrowserModal isOpen={false} {...props} />);
      await act(async () => {
        slowFirst.resolve(listingFor('dirA', 'a1.fits'));
        await slowFirst.promise;
      });
      // Reopening shows a fresh request's loading state, not the stale listing.
      routeInvoke({ browse_directories: () => new Promise(() => {}) });
      rerender(<FolderBrowserModal isOpen {...props} />);
      expect(screen.queryByText('a1.fits')).toBeNull();
      expect(await screen.findByText('Loading...')).toBeTruthy();
    });
  });
});
