import { afterEach, describe, expect, it, vi } from 'vitest';
import { eventSubjectKey, subjectKey, subjectLink, type StackSubject } from './subject';

const frameSet: StackSubject = { kind: 'frameSet', framesSetId: 7, lightFrames: [] };
const project: StackSubject = { kind: 'project', projectId: 'p1', title: 'T' };

describe('subject', () => {
  afterEach(() => vi.restoreAllMocks());

  it('keys a frame set as set:<id> and a project as project:<projectId>', () => {
    expect(subjectKey(frameSet)).toBe('set:7');
    expect(subjectKey(project)).toBe('project:p1');
  });

  it('keys an event by whichever of setId and projectId it carries', () => {
    expect(eventSubjectKey({ setId: 7, projectId: null })).toBe('set:7');
    expect(eventSubjectKey({ setId: null, projectId: 'p1' })).toBe('project:p1');
  });

  it('logs and returns null for an event carrying neither or both', () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(eventSubjectKey({ setId: null, projectId: null })).toBeNull();
    expect(eventSubjectKey({ setId: 7, projectId: 'p1' })).toBeNull();
    expect(err).toHaveBeenCalledTimes(2);
  });

  it('links a frame set and a project to their Stacking tab', () => {
    expect(subjectLink(frameSet)).toBe('/objects/7?tab=stacking');
    expect(subjectLink(project)).toBe('/projects/p1?tab=stacking');
  });
});
