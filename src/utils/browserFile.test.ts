import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { downloadTextFile, readFileText } from './browserFile';

describe('downloadTextFile', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    document.body.innerHTML = '';
  });

  it('clicks a download link for a Blob URL and revokes the URL afterwards', async () => {
    const create = vi.fn(() => 'blob:test-url');
    const revoke = vi.fn();
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke });
    let clicked: HTMLAnchorElement | null = null;
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
      clicked = this;
    });

    downloadTextFile('rules.json', '{"a":1}');

    expect(create).toHaveBeenCalledTimes(1);
    const blob = (create.mock.calls[0] as unknown as [Blob])[0];
    expect(blob.size).toBe(7);
    expect(await blob.text()).toBe('{"a":1}');
    expect(clicked).not.toBeNull();
    expect(clicked!.getAttribute('download')).toBe('rules.json');
    expect(clicked!.getAttribute('href')).toBe('blob:test-url');
    // The link is gone from the page and the URL is released only after the browser has taken it.
    expect(document.querySelector('a')).toBeNull();
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(revoke).toHaveBeenCalledWith('blob:test-url');
  });
});

describe('readFileText', () => {
  it('reads a text file', async () => {
    expect(await readFileText(new File(['hello'], 'a.json'))).toBe('hello');
  });

  it('resolves for bytes that are not UTF-8, so the server can refuse the text', async () => {
    const text = await readFileText(new File([new Uint8Array([0x41, 0xff, 0x42])], 'a.json'));
    expect(typeof text).toBe('string');
    expect(text.startsWith('A')).toBe(true);
    expect(text.endsWith('B')).toBe(true);
  });
});
