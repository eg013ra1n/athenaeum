import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { CalibrationWarnings } from './CalibrationWarnings';

describe('CalibrationWarnings', () => {
  it('renders nothing without warnings', () => {
    const { container } = render(<CalibrationWarnings warnings={[]} />);
    expect(container).toBeEmptyDOMElement();
  });
  it('lists each sentence', () => {
    render(<CalibrationWarnings warnings={['3 of 30 lights have a flat but no dark or bias (OFFSET 1, GAIN 100).']} />);
    expect(screen.getByRole('list', { name: 'Calibration warnings' })).toHaveTextContent('3 of 30 lights');
  });
});
