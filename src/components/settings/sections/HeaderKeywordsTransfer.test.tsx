import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { HeaderKeywordsTransfer } from './HeaderKeywordsTransfer';
import { api } from '../../../api';
import { pickFile, pickSaveFile } from '../../../api/desktop';
import { downloadTextFile } from '../../../utils/browserFile';
import { routeInvoke } from '../../../test/apiMock';
import { DEFAULT_HEADER_CHAINS, HEADER_FIELDS } from '../../../settings/headerFields';
import type { HeaderFallbackConfig, HeaderFallbackImportPlan } from '../../../types/models';

vi.mock('../../../api');
vi.mock('../../../api/desktop', () => ({ pickFile: vi.fn(), pickSaveFile: vi.fn() }));

const platform = vi.hoisted(() => ({ tauri: false }));
vi.mock('../../../utils/platform', () => ({
  get isTauri() {
    return platform.tauri;
  },
}));

// `readFileText` stays real (jsdom has FileReader); only the download is replaced.
vi.mock('../../../utils/browserFile', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../../utils/browserFile')>()),
  downloadTextFile: vi.fn(),
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const FILE_TEXT = '{"format":"athenaeum.header-fallbacks","version":1,"fields":{"ccdTemp":["DET-TEMP","CCD-TEMP"]}}';

function config(): HeaderFallbackConfig {
  const c: HeaderFallbackConfig = {
    fields: Object.fromEntries(
      HEADER_FIELDS.map((f) => [f, { order: [...DEFAULT_HEADER_CHAINS[f]], retired: [] }]),
    ),
  };
  c.fields.ccdTemp = { order: ['DET-TEMP', 'CCD-TEMP'], retired: [] };
  return c;
}

const CHANGED: HeaderFallbackImportPlan = {
  config: config(),
  changes: [{ field: 'ccdTemp', before: ['CCD-TEMP', 'DET-TEMP'], after: ['DET-TEMP', 'CCD-TEMP'] }],
};
const UNCHANGED: HeaderFallbackImportPlan = { config: config(), changes: [] };

const onImport = vi.fn();

function callsOf(command: string) {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === command);
}

function chooseFile(text: string) {
  const input = screen.getByTestId('header-keywords-file') as HTMLInputElement;
  const file = new File([text], 'rules.json', { type: 'application/json' });
  fireEvent.change(input, { target: { files: [file] } });
}

beforeEach(() => {
  platform.tauri = false;
  notifyMock.mockClear();
  onImport.mockClear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(downloadTextFile).mockClear();
  vi.mocked(pickFile).mockReset();
  vi.mocked(pickSaveFile).mockReset();
});

describe('HeaderKeywordsTransfer', () => {
  it('exports through a browser download on the web', async () => {
    routeInvoke({ export_header_fallbacks: () => FILE_TEXT });
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    fireEvent.click(screen.getByRole('button', { name: 'Export…' }));
    await waitFor(() => expect(downloadTextFile).toHaveBeenCalled());
    expect(callsOf('export_header_fallbacks')[0]![1]).toEqual({});
    expect(downloadTextFile).toHaveBeenCalledWith('athenaeum-fits-keywords.json', FILE_TEXT);
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it('imports a file and confirms the field changes', async () => {
    routeInvoke({ plan_header_fallbacks_import: () => CHANGED });
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    chooseFile(FILE_TEXT);
    await screen.findByText(/Sensor temperature: CCD-TEMP, DET-TEMP → DET-TEMP, CCD-TEMP/);
    expect(callsOf('plan_header_fallbacks_import')[0]![1]).toEqual({ text: FILE_TEXT });
    expect(screen.getByText(/Catalog values change only when you apply\./)).toBeTruthy();
    expect(onImport).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Import' }));
    expect(onImport).toHaveBeenCalledTimes(1);
    expect(onImport).toHaveBeenCalledWith(CHANGED.config);
    expect(screen.queryByText('Import FITS keywords?')).toBeNull();
  });

  it('cancelling the confirm applies nothing', async () => {
    routeInvoke({ plan_header_fallbacks_import: () => CHANGED });
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    chooseFile(FILE_TEXT);
    await screen.findByText('Import FITS keywords?');
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onImport).not.toHaveBeenCalled();
    expect(screen.queryByText('Import FITS keywords?')).toBeNull();
  });

  it('says when a file changes nothing', async () => {
    routeInvoke({ plan_header_fallbacks_import: () => UNCHANGED });
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    chooseFile(FILE_TEXT);
    await screen.findByText('This file matches your current keywords.');
    expect(screen.queryByText(/Catalog values change only when you apply\./)).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Import' }));
    expect(onImport).not.toHaveBeenCalled();
    expect(screen.queryByText('Import FITS keywords?')).toBeNull();
  });

  it('notifies a refused file and applies nothing', async () => {
    routeInvoke({ plan_header_fallbacks_import: () => Promise.reject('unknown field "bogus"') });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    chooseFile('{"fields":{"bogus":["X"]}}');
    await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
    expect(notifyMock).toHaveBeenCalledWith({
      kind: 'generic',
      tone: 'warning',
      hasErrors: true,
      title: 'FITS keywords not imported',
      detail: 'unknown field "bogus"',
    });
    expect(screen.queryByText('Import FITS keywords?')).toBeNull();
    expect(onImport).not.toHaveBeenCalled();
  });

  it('notifies a failed export', async () => {
    routeInvoke({ export_header_fallbacks: () => Promise.reject(new Error('disk full')) });
    vi.spyOn(console, 'error').mockImplementation(() => {});
    render(<HeaderKeywordsTransfer onImport={onImport} />);
    fireEvent.click(screen.getByRole('button', { name: 'Export…' }));
    await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
    expect(notifyMock.mock.calls[0]![0]).toMatchObject({ title: 'FITS keywords not exported', detail: 'disk full' });
    expect(downloadTextFile).not.toHaveBeenCalled();
  });

  describe('desktop', () => {
    beforeEach(() => {
      platform.tauri = true;
    });

    it('exports to the chosen path and does nothing on cancel', async () => {
      routeInvoke({ export_header_fallbacks: () => 'ignored' });
      render(<HeaderKeywordsTransfer onImport={onImport} />);
      vi.mocked(pickSaveFile).mockResolvedValueOnce(null);
      fireEvent.click(screen.getByRole('button', { name: 'Export…' }));
      await waitFor(() => expect(pickSaveFile).toHaveBeenCalledTimes(1));
      expect(pickSaveFile).toHaveBeenCalledWith({
        defaultPath: 'athenaeum-fits-keywords.json',
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      await waitFor(() => expect((screen.getByRole('button', { name: 'Export…' }) as HTMLButtonElement).disabled).toBe(false));
      expect(callsOf('export_header_fallbacks')).toHaveLength(0);

      vi.mocked(pickSaveFile).mockResolvedValueOnce('/tmp/rules.json');
      fireEvent.click(screen.getByRole('button', { name: 'Export…' }));
      await waitFor(() => expect(callsOf('export_header_fallbacks')).toHaveLength(1));
      expect(callsOf('export_header_fallbacks')[0]![1]).toEqual({ path: '/tmp/rules.json' });
      expect(downloadTextFile).not.toHaveBeenCalled();
    });

    it('imports by path from the open dialog', async () => {
      routeInvoke({ plan_header_fallbacks_import: () => CHANGED });
      vi.mocked(pickFile).mockResolvedValueOnce('/tmp/rules.json');
      render(<HeaderKeywordsTransfer onImport={onImport} />);
      expect(screen.queryByTestId('header-keywords-file')).toBeNull();
      fireEvent.click(screen.getByRole('button', { name: 'Import…' }));
      await screen.findByText('Import FITS keywords?');
      expect(pickFile).toHaveBeenCalledWith({
        title: 'Import FITS keywords',
        filters: [{ name: 'JSON', extensions: ['json'] }],
      });
      expect(callsOf('plan_header_fallbacks_import')[0]![1]).toEqual({ path: '/tmp/rules.json' });
    });

    it('does nothing when the open dialog is cancelled', async () => {
      routeInvoke({ plan_header_fallbacks_import: () => CHANGED });
      vi.mocked(pickFile).mockResolvedValueOnce(null);
      render(<HeaderKeywordsTransfer onImport={onImport} />);
      fireEvent.click(screen.getByRole('button', { name: 'Import…' }));
      await waitFor(() => expect(pickFile).toHaveBeenCalledTimes(1));
      expect(callsOf('plan_header_fallbacks_import')).toHaveLength(0);
      expect(notifyMock).not.toHaveBeenCalled();
    });
  });
});
