import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { SettingNumber } from './SettingNumber';
import { intCodec } from '../../settings/codecs';
import type { UseSettingFieldResult } from '../../hooks/useSettingField';

// `SettingNumber` is a thin renderer over `useSettingField` — these tests
// pin its wiring (draft/blur/Enter/Escape/error), not the hook's own
// behaviour (covered by `useSettingField.test.tsx`), so the hook is mocked
// directly. `hookBox` is a mutable box rather than a plain `let` so the
// hoisted `vi.mock` factory (which runs before this file's own `let`
// declarations would be initialized) can close over it safely.
const { hookBox, commitMock, escapeMock, setDraftMock, setValueMock, resetMock } = vi.hoisted(() => ({
  hookBox: { current: null as unknown },
  commitMock: vi.fn(),
  escapeMock: vi.fn(),
  setDraftMock: vi.fn(),
  setValueMock: vi.fn(),
  resetMock: vi.fn(),
}));

vi.mock('../../hooks/useSettingField', async () => {
  const actual = await vi.importActual<typeof import('../../hooks/useSettingField')>('../../hooks/useSettingField');
  return {
    ...actual,
    useSettingField: () => hookBox.current,
  };
});

function makeHookState(overrides: Partial<UseSettingFieldResult<number>> = {}): UseSettingFieldResult<number> {
  return {
    value: 5,
    draft: '5',
    setDraft: setDraftMock,
    commit: commitMock,
    setValue: setValueMock,
    escape: escapeMock,
    error: null,
    saving: false,
    savedAt: null,
    defaultValue: 5,
    isDefault: true,
    reset: resetMock,
    meta: { id: 'threads', label: 'Threads', help: 'How many worker threads to use.' },
    label: 'Threads',
    help: 'How many worker threads to use.',
    ...overrides,
  };
}

const FIELD_PROPS = { section: 'blink.viewer', field: 'threads', settingKey: 'blink.threads' } as const;

beforeEach(() => {
  commitMock.mockClear();
  escapeMock.mockClear();
  setDraftMock.mockClear();
  setValueMock.mockClear();
  resetMock.mockClear();
  hookBox.current = makeHookState();
});

describe('SettingNumber', () => {
  it('typing updates the draft without committing', () => {
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    const input = screen.getByRole('spinbutton');
    fireEvent.change(input, { target: { value: '12' } });
    expect(setDraftMock).toHaveBeenCalledWith('12');
    expect(commitMock).not.toHaveBeenCalled();
  });

  it('commits on blur', () => {
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    fireEvent.blur(screen.getByRole('spinbutton'));
    expect(commitMock).toHaveBeenCalledTimes(1);
  });

  it('commits on Enter', () => {
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    fireEvent.keyDown(screen.getByRole('spinbutton'), { key: 'Enter' });
    expect(commitMock).toHaveBeenCalledTimes(1);
  });

  it('restores the draft on Escape and never commits', () => {
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    fireEvent.keyDown(screen.getByRole('spinbutton'), { key: 'Escape' });
    expect(escapeMock).toHaveBeenCalledTimes(1);
    expect(commitMock).not.toHaveBeenCalled();
  });

  it('renders an inline error under the field', () => {
    hookBox.current = makeHookState({ error: 'Must be a whole number between 0 and 32' });
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    expect(screen.getByText('Must be a whole number between 0 and 32')).toBeInTheDocument();
  });

  it('shows the ResetButton only when the value differs from the default', () => {
    hookBox.current = makeHookState({ isDefault: true });
    const { rerender } = render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    expect(screen.queryByRole('button')).not.toBeInTheDocument();

    hookBox.current = makeHookState({ isDefault: false, value: 9, draft: '9', defaultValue: 5 });
    rerender(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} />);
    expect(screen.getByRole('button')).toBeInTheDocument();
  });

  it('renders a range input and commits through setValue on change in slider variant', () => {
    render(<SettingNumber {...FIELD_PROPS} codec={intCodec(0, 32)} variant="slider" min={0} max={32} />);
    const slider = screen.getByRole('slider');
    fireEvent.change(slider, { target: { value: '20' } });
    expect(setValueMock).toHaveBeenCalledWith(20);
    expect(commitMock).not.toHaveBeenCalled();
  });
});
