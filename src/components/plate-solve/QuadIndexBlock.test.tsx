// Quad Index block of the plate-solve settings: rows from `get_index_status`,
// Download -> `download_index_packs`, an incompatible pack with no Download
// button, and the progress line fed by `index-download-progress`.
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { advance } from '../../test/fakeClock';
import { QuadIndexBlock } from './QuadIndexBlock';
import { getIndexStatusText } from './useIndexPacks';
import { api } from '../../api';
import type { IndexStatusInfo, IndexDownloadProgress } from '../../types/helpers';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const base: IndexStatusInfo = {
  name: 'base',
  dir: '/idx/base',
  installed: false,
  compatible: true,
  sizeBytes: 147_000_000,
  minFovDeg: 1.3,
  error: null,
};
const narrow: IndexStatusInfo = {
  name: 'narrow',
  dir: '/idx/narrow',
  installed: false,
  compatible: true,
  sizeBytes: 236_000_000,
  minFovDeg: 0.65,
  error: null,
};

let status: IndexStatusInfo[];
let progressCb: ((p: IndexDownloadProgress) => void) | undefined;

beforeEach(() => {
  status = [base, narrow];
  progressCb = undefined;
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
    if (cmd === 'get_index_status') return Promise.resolve(status);
    return Promise.resolve('ok');
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((_evt: string, cb: (p: IndexDownloadProgress) => void) => {
    progressCb = cb;
    return Promise.resolve(() => {});
  }) as never);
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

async function renderBlock() {
  render(<QuadIndexBlock />);
  await advance(0);
  await advance(0);
}

function downloadCalls() {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'download_index_packs');
}

describe('QuadIndexBlock', () => {
  it('renders one row per pack with its field floor and size', async () => {
    await renderBlock();
    expect(screen.getByText('Base')).toBeInTheDocument();
    expect(screen.getByText('Narrow')).toBeInTheDocument();
    expect(screen.getByText(/fields ≥ 1\.30°/)).toBeInTheDocument();
    expect(screen.getByText(/fields ≥ 0\.65°/)).toBeInTheDocument();
    expect(screen.getByText('147 MB')).toBeInTheDocument();
    expect(screen.getByText('236 MB')).toBeInTheDocument();
  });

  it('Download on one row requests just that pack', async () => {
    await renderBlock();
    const buttons = screen.getAllByRole('button', { name: 'Download' });
    expect(buttons).toHaveLength(2);
    fireEvent.click(buttons[1]);
    await advance(0);
    expect(downloadCalls()).toEqual([['download_index_packs', { names: ['narrow'] }]]);
  });

  it('Download all requests every missing compatible pack', async () => {
    await renderBlock();
    fireEvent.click(screen.getByRole('button', { name: 'Download all' }));
    await advance(0);
    expect(downloadCalls()).toEqual([['download_index_packs', { names: ['base', 'narrow'] }]]);
  });

  it('shows Installed for an installed pack and hides Download all with one pack missing', async () => {
    status = [{ ...base, installed: true }, narrow];
    await renderBlock();
    expect(screen.getByText('Installed')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Download all' })).toBeNull();
    expect(screen.getAllByRole('button', { name: 'Download' })).toHaveLength(1);
  });

  it('an incompatible pack shows no Download button', async () => {
    status = [base, { ...narrow, compatible: false }];
    await renderBlock();
    expect(screen.getByText('Needs a newer Athenaeum')).toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: 'Download' })).toHaveLength(1);
    expect(screen.queryByRole('button', { name: 'Download all' })).toBeNull();
  });

  it('a pack with an error shows the text and keeps its Download button', async () => {
    status = [{ ...base, error: 'checksum mismatch' }];
    await renderBlock();
    expect(screen.getByText('checksum mismatch')).toHaveClass('text-error');
    expect(screen.getByRole('button', { name: 'Download' })).toBeInTheDocument();
  });

  it('says the index server was unreachable when the list is empty', async () => {
    status = [];
    await renderBlock();
    expect(screen.getByText(/index server could not be reached/)).toBeInTheDocument();
  });

  it('shows progress text from index-download-progress and reloads on complete', async () => {
    await renderBlock();
    expect(vi.mocked(api.listen)).toHaveBeenCalledWith('index-download-progress', expect.any(Function));
    const { act } = await import('@testing-library/react');
    act(() => {
      progressCb!({
        phase: 'downloading',
        current: 40 * 1048576,
        total: 147 * 1048576,
        percent: 27,
        packName: 'base',
        packIndex: 0,
        nPacks: 2,
      });
    });
    expect(screen.getByText('Pack 1/2 (base) — Downloading · 40 / 147 MB')).toBeInTheDocument();
    expect(screen.getByText('27%')).toBeInTheDocument();

    const before = vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_index_status').length;
    act(() => {
      progressCb!({ phase: 'complete', current: 0, total: 0, percent: 100, packName: 'narrow', packIndex: 1, nPacks: 2 });
    });
    await advance(0);
    const after = vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_index_status').length;
    expect(after).toBe(before + 1);
    expect(screen.queryByText(/Downloading/)).toBeNull();
  });

  it('shows the failure text from an error event', async () => {
    await renderBlock();
    const { act } = await import('@testing-library/react');
    act(() => {
      progressCb!({ phase: 'error', current: 0, total: 0, percent: 0, packName: 'base', packIndex: 0, nPacks: 1, message: 'disk full' });
    });
    expect(screen.getByText('disk full')).toHaveClass('text-error');
  });
});

describe('getIndexStatusText', () => {
  it('names the phases', () => {
    const p = { current: 0, total: 0, percent: 0, packName: 'base', packIndex: 0, nPacks: 1 };
    expect(getIndexStatusText(null)).toMatch(/connecting to the index server/);
    expect(getIndexStatusText({ ...p, phase: 'verifying' })).toBe('base — Verifying integrity…');
    expect(getIndexStatusText({ ...p, phase: 'extracting', nPacks: 2, packIndex: 1 })).toBe('Pack 2/2 (base) — Extracting…');
  });
});
