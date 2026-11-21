import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../../../api';
import { routeInvoke } from '../../../test/apiMock';
import { SetFrameTypeModal } from './SetFrameTypeModal';

vi.mock('../../../api');

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  routeInvoke({ bulk_update_frame_metadata: 2 });
});
afterEach(() => cleanup());

async function applyChoice(label: string) {
  const onApplied = vi.fn();
  render(
    <SetFrameTypeModal
      isOpen
      eligibleCount={2}
      totalSelected={2}
      eligibleFrameIds={[11, 12]}
      onApplied={onApplied}
      onCancel={() => {}}
    />,
  );
  fireEvent.click(screen.getByLabelText(label));
  fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
  await waitFor(() => expect(onApplied).toHaveBeenCalled());
  const call = vi.mocked(api.invoke).mock.calls.find(([c]) => c === 'bulk_update_frame_metadata');
  expect(call).toBeDefined();
  return call![1] as { frameIds: number[]; edits: { imagetyp: string; isMaster: boolean } };
}

describe('SetFrameTypeModal', () => {
  it('sends_the_master_type_text_for_a_master', async () => {
    const dark = await applyChoice('MASTERDARK');
    expect(dark.frameIds).toEqual([11, 12]);
    expect(dark.edits.imagetyp).toBe('MasterDark');
    expect(dark.edits.isMaster).toBe(true);
    cleanup();
    vi.mocked(api.invoke).mockClear();
    const flat = await applyChoice('MASTERFLAT');
    expect(flat.edits.imagetyp).toBe('MasterFlat');
    expect(flat.edits.isMaster).toBe(true);
  });

  it('sends_the_plain_type_text_for_a_raw_frame', async () => {
    const dark = await applyChoice('DARK');
    expect(dark.edits).toEqual({ imagetyp: 'DARK', isMaster: false });
    cleanup();
    vi.mocked(api.invoke).mockClear();
    const flat = await applyChoice('FLAT');
    expect(flat.edits).toEqual({ imagetyp: 'FLAT', isMaster: false });
  });
});
