import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { api } from '../../api';
import { ZipExtractionLine } from './ZipExtractionLine';

vi.mock('../../api', () => ({ api: { invoke: vi.fn() } }));

const estimate = (files: number, bytes: number) => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((async () => ({ files, bytes })) as never);
};

// Braces: a function returned from beforeEach is run as cleanup, and `mockReset()` returns the mock.
beforeEach(() => { vi.mocked(api.invoke).mockReset(); });

describe('ZipExtractionLine', () => {
  it('asks the backend for the named batch and shows files and size', async () => {
    estimate(120, 6.2e9);
    render(<ZipExtractionLine scope={{ frameIds: [1, 2, 3] }} />);
    expect(await screen.findByTestId('zip-extraction-line')).toHaveTextContent('From zip: 120 files, 6.2 GB');
    expect(api.invoke).toHaveBeenCalledWith('estimate_zip_extraction', { fileIds: [], frameIds: [1, 2, 3], frameSetId: null });
  });

  it('names a whole frame set by its id', async () => {
    estimate(2, 2e8);
    render(<ZipExtractionLine scope={{ frameSetId: 5 }} />);
    await screen.findByTestId('zip-extraction-line');
    expect(api.invoke).toHaveBeenCalledWith('estimate_zip_extraction', { fileIds: [], frameIds: [], frameSetId: 5 });
  });

  it('shows nothing when no frame needs extracting, and does not ask for an empty batch', async () => {
    estimate(0, 0);
    const { rerender } = render(<ZipExtractionLine scope={{ frameIds: [1] }} />);
    await waitFor(() => expect(api.invoke).toHaveBeenCalled());
    expect(screen.queryByTestId('zip-extraction-line')).toBeNull();
    vi.mocked(api.invoke).mockClear();
    rerender(<ZipExtractionLine scope={{ frameIds: [] }} />);
    await new Promise((r) => setTimeout(r, 350));
    expect(api.invoke).not.toHaveBeenCalled();
  });

  it('logs and shows nothing when the estimate fails', async () => {
    const errors: unknown[][] = [];
    const original = console.error;
    console.error = (...args: unknown[]) => { errors.push(args); };
    try {
      vi.mocked(api.invoke).mockReset();
      vi.mocked(api.invoke).mockImplementation((() => Promise.reject(new Error('boom'))) as never);
      render(<ZipExtractionLine scope={{ fileIds: [9] }} />);
      await waitFor(() => expect(errors.some((e) => String(e[0]).includes('estimate_zip_extraction failed'))).toBe(true));
      expect(screen.queryByTestId('zip-extraction-line')).toBeNull();
    } finally {
      console.error = original;
    }
  });
});
