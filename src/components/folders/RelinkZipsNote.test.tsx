import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import { RelinkZipsNote } from './RelinkZipsNote';
import type { RelinkResult } from '../../types/models';

const result = (zips_missing: number): RelinkResult =>
  ({ files_matched: 4, files_new: 0, files_orphaned: 0, orphaned_file_ids: [], zips_missing });

describe('RelinkZipsNote', () => {
  it('says how many zips were not found and that Remove folder is the way out', () => {
    render(<RelinkZipsNote result={result(2)} />);
    const note = screen.getByRole('status');
    expect(note).toHaveTextContent('2 zips not found at the new location');
    expect(note).toHaveTextContent('Remove folder');
  });
  it('uses the singular for one zip', () => {
    render(<RelinkZipsNote result={result(1)} />);
    expect(screen.getByRole('status')).toHaveTextContent('1 zip not found');
  });
  it('renders nothing when every zip was found', () => {
    const { container } = render(<RelinkZipsNote result={result(0)} />);
    expect(container).toBeEmptyDOMElement();
  });
});
