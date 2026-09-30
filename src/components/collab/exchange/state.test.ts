import { describe, expect, it } from 'vitest';
import {
  EMPTY_EXCHANGE,
  applyProgress,
  applySnapshot,
  clearFlows,
  exchangeTotals,
  flowKey,
  nameKey,
  peerLabel,
  unknownDevices,
} from './state';
import type { FlowView, ProjectFlows } from '../../../types/models';

const flow = (o: Partial<FlowView>): FlowView => ({
  projectId: 'p1',
  device: 'devA',
  direction: 'recv',
  bytesSession: 0,
  rateBps: 1000,
  etaSecs: null,
  moving: true,
  completed: 0,
  inFlight: [],
  ...o,
});
const proj = (o: Partial<ProjectFlows>): ProjectFlows => ({
  projectId: 'p1',
  recv: [],
  send: [],
  toGo: 0,
  waitingForPublisher: null,
  ...o,
});

describe('collab exchange state', () => {
  it('a progress event keeps waitingForPublisher from the snapshot', () => {
    let s = applySnapshot(
      EMPTY_EXCHANGE,
      { projects: [proj({ recv: [flow({})], waitingForPublisher: 12 })], names: [] },
      null,
    );
    s = applyProgress(s, { projects: [proj({ recv: [flow({ rateBps: 2000 })], toGo: 5 })] });
    expect(s.projects.p1.waitingForPublisher).toBe(12);
    expect(s.projects.p1.toGo).toBe(5);
    expect(s.rates['p1|recv|devA']).toEqual([1000, 2000]);
  });

  it('the all-zero quiet event removes the project', () => {
    let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})] })], names: [] }, null);
    s = applyProgress(s, { projects: [proj({ recv: [flow({ moving: false, rateBps: 0 })] })] });
    expect(s.projects.p1).toBeUndefined();
  });

  it('rate history keeps the last 40 samples', () => {
    let s = EMPTY_EXCHANGE;
    for (let i = 0; i < 50; i++) {
      s = applyProgress(s, { projects: [proj({ recv: [flow({ rateBps: i + 1 })] })] });
    }
    expect(s.rates['p1|recv|devA']).toHaveLength(40);
    expect(s.rates['p1|recv|devA'][39]).toBe(50);
  });

  it('unknown devices are reported until a snapshot names them; the label falls back to the short id', () => {
    let s = applyProgress(EMPTY_EXCHANGE, {
      projects: [proj({ send: [flow({ device: 'abcdefghijkl', direction: 'send' })] })],
    });
    expect(unknownDevices(s)).toEqual([nameKey('p1', 'abcdefghijkl')]);
    expect(peerLabel(s, 'p1', 'abcdefghijkl')).toEqual({ member: null, device: 'abcdefgh' });
    s = applySnapshot(
      s,
      {
        projects: [],
        names: [{ projectId: 'p1', device: 'abcdefghijkl', memberName: 'Kostya', deviceName: 'kostya-obs' }],
      },
      'p9',
    );
    expect(unknownDevices(s)).toEqual([]);
    expect(peerLabel(s, 'p1', 'abcdefghijkl')).toEqual({ member: 'Kostya', device: 'kostya-obs' });
  });

  it('a scoped snapshot replaces only its project', () => {
    let s = applySnapshot(
      EMPTY_EXCHANGE,
      {
        projects: [proj({ recv: [flow({})] }), proj({ projectId: 'p2', recv: [flow({ projectId: 'p2' })] })],
        names: [],
      },
      null,
    );
    s = applySnapshot(s, { projects: [], names: [] }, 'p2');
    expect(Object.keys(s.projects)).toEqual(['p1']);
  });

  it('totals sum rates by direction', () => {
    const s = applySnapshot(
      EMPTY_EXCHANGE,
      {
        projects: [
          proj({ recv: [flow({ rateBps: 10 })], send: [flow({ direction: 'send', rateBps: 5 })] }),
        ],
        names: [],
      },
      null,
    );
    expect(exchangeTotals(s)).toEqual({ recvBps: 10, sendBps: 5, active: true });
  });

  it('flowKey and clearFlows', () => {
    expect(flowKey(flow({}))).toBe('p1|recv|devA');
    let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})] })], names: [] }, null);
    s = clearFlows(s);
    expect(s.projects).toEqual({});
    expect(s.rates).toEqual({});
  });

  // ── Fix round 1 (controller ruling): idle flows in a snapshot never
  // resurrect a project, `summary` carries toGo/waitingForPublisher across a
  // project going quiet, and rings/active are never reset or lit by a ghost.

  it('a global snapshot containing only an idle flow leaves projects empty and totals inactive', () => {
    const s = applySnapshot(
      EMPTY_EXCHANGE,
      { projects: [proj({ recv: [flow({ moving: false, rateBps: 0, inFlight: [] })] })], names: [] },
      null,
    );
    expect(s.projects).toEqual({});
    expect(exchangeTotals(s)).toEqual({ recvBps: 0, sendBps: 0, active: false });
  });

  it('a scoped snapshot of an idle-only project keeps its summary but drops it from projects', () => {
    const s = applySnapshot(
      EMPTY_EXCHANGE,
      {
        projects: [
          proj({
            recv: [flow({ moving: false, rateBps: 0, inFlight: [] })],
            toGo: 7,
            waitingForPublisher: 3,
          }),
        ],
        names: [],
      },
      'p1',
    );
    expect(s.summary.p1).toEqual({ toGo: 7, waitingForPublisher: 3 });
    expect(s.projects.p1).toBeUndefined();
  });

  it('a scoped snapshot naming a different project leaves an existing summary untouched', () => {
    let s = applySnapshot(
      EMPTY_EXCHANGE,
      { projects: [proj({ toGo: 7, waitingForPublisher: 3 })], names: [] },
      'p1',
    );
    s = applySnapshot(s, { projects: [], names: [] }, 'p9');
    expect(s.summary.p1).toEqual({ toGo: 7, waitingForPublisher: 3 });
  });

  it('a quiet progress event keeps summary; a live one updates toGo and keeps waitingForPublisher', () => {
    let s = applySnapshot(
      EMPTY_EXCHANGE,
      { projects: [proj({ waitingForPublisher: 3 })], names: [] },
      'p1',
    );
    s = applyProgress(s, { projects: [proj({ toGo: 0 })] });
    expect(s.summary.p1).toEqual({ toGo: 0, waitingForPublisher: 3 });
    expect(s.projects.p1).toBeUndefined();

    s = applyProgress(s, { projects: [proj({ toGo: 5 })] });
    expect(s.summary.p1).toEqual({ toGo: 5, waitingForPublisher: 3 });
  });

  it('a rate ring survives a later global snapshot of the same live flow (no reset)', () => {
    let s = applySnapshot(EMPTY_EXCHANGE, { projects: [proj({ recv: [flow({})] })], names: [] }, null);
    s = applyProgress(s, { projects: [proj({ recv: [flow({ rateBps: 2000 })] })] });
    expect(s.rates['p1|recv|devA']).toEqual([1000, 2000]);

    s = applySnapshot(
      s,
      { projects: [proj({ recv: [flow({ rateBps: 3000 })] })], names: [] },
      null,
    );
    expect(s.rates['p1|recv|devA']).toEqual([1000, 2000]);
  });

  it('clearFlows keeps summary', () => {
    let s = applySnapshot(
      EMPTY_EXCHANGE,
      { projects: [proj({ recv: [flow({})], toGo: 7, waitingForPublisher: 3 })], names: [] },
      null,
    );
    s = clearFlows(s);
    expect(s.summary.p1).toEqual({ toGo: 7, waitingForPublisher: 3 });
  });
});
