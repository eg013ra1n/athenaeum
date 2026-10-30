import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router-dom';
import { FrameSetDuplicatesChip, duplicatesLabel } from './FrameSetDuplicatesChip';

function FilesProbe() {
  const location = useLocation();
  return <div data-testid="files-state">{JSON.stringify(location.state)}</div>;
}

describe('FrameSetDuplicatesChip', () => {
  it('says how many copies are not used', () => {
    expect(duplicatesLabel(1)).toBe('1 duplicate');
    expect(duplicatesLabel(12)).toBe('12 duplicates');
  });

  it('renders nothing for a set without duplicates', () => {
    const { container } = render(
      <MemoryRouter>
        <FrameSetDuplicatesChip count={0} />
      </MemoryRouter>,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('opens the Duplicates page', () => {
    render(
      <MemoryRouter initialEntries={['/objects']}>
        <Routes>
          <Route path="/objects" element={<FrameSetDuplicatesChip count={12} />} />
          <Route path="/files" element={<FilesProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    fireEvent.click(screen.getByText('12 duplicates'));
    expect(JSON.parse(screen.getByTestId('files-state').textContent ?? 'null')).toEqual({
      openDuplicates: true,
    });
  });
});
