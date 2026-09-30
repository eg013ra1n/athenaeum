import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ExcludeDialog from './ExcludeDialog';
import { api } from '../../../api';
import type { FrameVM } from './frames';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

afterEach(cleanup);

function frame(overrides: Partial<FrameVM> = {}): FrameVM {
  return {
    key: 'uuid-1',
    frameId: null,
    frameUuid: 'uuid-1',
    setId: null,
    setName: null,
    fileName: 'light_001.fits',
    night: null,
    filter: 'L',
    filterMapped: true,
    camera: 'ASI2600MM',
    publisher: null,
    publisherAccountId: null,
    exptimeSec: 300,
    byteSize: 1000,
    fwhm: null,
    ecc: null,
    stars: null,
    snr: null,
    failures: [],
    contentVersion: 1,
    pubState: 'published',
    excluded: false,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    disk: null,
    publishedAt: null,
    device: null,
    missingWhy: null,
    progress: null,
    submittedAt: null,
    states: [],
    own: null,
    lib: null,
    mod: null,
    ...overrides,
  };
}

describe('ExcludeDialog', () => {
  it('titles the dialog with the frame count and disables Exclude for a blank or over-limit reason', () => {
    render(
      <ExcludeDialog
        projectId="p"
        frames={[frame(), frame({ key: 'uuid-2', frameUuid: 'uuid-2' })]}
        onClose={vi.fn()}
        onDone={vi.fn()}
      />,
    );
    expect(screen.getByText('Exclude 2 frames from the project')).toBeInTheDocument();
    expect(
      screen.getByText(
        "Excluded frames stop counting toward the project and are no longer exchanged. You can restore them from the frame's panel.",
      ),
    ).toBeInTheDocument();

    const textarea = screen.getByRole('textbox');
    const btn = screen.getByRole('button', { name: 'Exclude' });
    expect(btn).toBeDisabled();

    fireEvent.change(textarea, { target: { value: 'x'.repeat(501) } });
    expect(btn).toBeDisabled();

    fireEvent.change(textarea, { target: { value: 'a valid reason' } });
    expect(btn).not.toBeDisabled();

    fireEvent.change(textarea, { target: { value: '   ' } });
    expect(btn).toBeDisabled();
  });

  it('excludes each frame with the trimmed reason, in turn; a failure stops and reports "Excluded K of N"', async () => {
    let calls = 0;
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'exclude_collab_frame') {
        calls += 1;
        if (calls === 2) return Promise.reject(new Error('hub unreachable'));
        return Promise.resolve(undefined);
      }
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);
    const onDone = vi.fn();
    const onClose = vi.fn();
    const errSpy = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(
      <ExcludeDialog
        projectId="p"
        frames={[frame({ key: 'uuid-1', frameUuid: 'uuid-1' }), frame({ key: 'uuid-2', frameUuid: 'uuid-2' })]}
        onClose={onClose}
        onDone={onDone}
      />,
    );
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '  bad frame  ' } });
    fireEvent.click(screen.getByRole('button', { name: 'Exclude' }));

    await waitFor(() => expect(screen.getByText('Excluded 1 of 2 — hub unreachable')).toBeInTheDocument());

    expect(api.invoke).toHaveBeenNthCalledWith(1, 'exclude_collab_frame', {
      projectId: 'p',
      frameUuid: 'uuid-1',
      reason: 'bad frame',
    });
    expect(api.invoke).toHaveBeenNthCalledWith(2, 'exclude_collab_frame', {
      projectId: 'p',
      frameUuid: 'uuid-2',
      reason: 'bad frame',
    });
    expect(onDone).toHaveBeenCalledWith(1);
    expect(onClose).not.toHaveBeenCalled();
    expect(errSpy).toHaveBeenCalledWith('[exclude] failed:', expect.any(Error));
    errSpy.mockRestore();
  });

  it('calls onDone(N) then onClose when every frame excludes successfully', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'exclude_collab_frame') return Promise.resolve(undefined);
      return Promise.reject(new Error(`unexpected ${command}`));
    }) as typeof api.invoke);
    const onDone = vi.fn();
    const onClose = vi.fn();

    render(<ExcludeDialog projectId="p" frames={[frame()]} onClose={onClose} onDone={onDone} />);
    fireEvent.change(screen.getByRole('textbox'), { target: { value: 'trailed frame' } });
    fireEvent.click(screen.getByRole('button', { name: 'Exclude' }));

    await waitFor(() => expect(onDone).toHaveBeenCalledWith(1));
    expect(onClose).toHaveBeenCalled();
    expect(api.invoke).toHaveBeenCalledWith('exclude_collab_frame', {
      projectId: 'p',
      frameUuid: 'uuid-1',
      reason: 'trailed frame',
    });
  });
});
