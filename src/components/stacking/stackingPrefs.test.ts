import { afterEach, describe, expect, it } from 'vitest';
import { readSelectedRunId, writeSelectedRunId } from './stackingPrefs';

const PREFIX = 'athenaeum.stacking.selectedRun.';

describe('selected run id by subject key', () => {
  afterEach(() => localStorage.clear());

  it('stores a frame set under the prefix and the bare set id', () => {
    writeSelectedRunId('set:12', 5);
    expect(localStorage.getItem(`${PREFIX}12`)).toBe('5');
    expect(localStorage.getItem(`${PREFIX}set:12`)).toBeNull();
  });

  it('reads a value stored under the pre-subject spelling', () => {
    localStorage.setItem(`${PREFIX}12`, '9');
    expect(readSelectedRunId('set:12')).toBe(9);
  });

  it('stores a project under the prefix and project:<projectId>', () => {
    writeSelectedRunId('project:p1', 3);
    expect(localStorage.getItem(`${PREFIX}project:p1`)).toBe('3');
    expect(readSelectedRunId('project:p1')).toBe(3);
  });

  it('keeps a frame set and a project with the same digits apart', () => {
    writeSelectedRunId('set:1', 10);
    writeSelectedRunId('project:1', 20);
    expect(readSelectedRunId('set:1')).toBe(10);
    expect(readSelectedRunId('project:1')).toBe(20);
  });

  it('reads null when nothing or an empty string is stored', () => {
    expect(readSelectedRunId('set:99')).toBeNull();
    localStorage.setItem(`${PREFIX}99`, '');
    expect(readSelectedRunId('set:99')).toBeNull();
  });
});
