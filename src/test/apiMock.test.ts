import { beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../api';
import { routeInvoke, unexpectedCommand } from './apiMock';

vi.mock('../api');

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
});

describe('routeInvoke', () => {
  it('resolves a fixed value for its command', async () => {
    routeInvoke({ get_a: [1, 2], get_b: 'b' });
    await expect(api.invoke('get_a')).resolves.toEqual([1, 2]);
    await expect(api.invoke('get_b')).resolves.toBe('b');
  });

  it('gives a function route the args and passes its promise through', async () => {
    routeInvoke({
      echo: (args: { n: number }) => args.n + 1,
      refuse: () => Promise.reject('refused'),
      boom: () => { throw new Error('thrown'); },
    });
    await expect(api.invoke('echo', { n: 4 })).resolves.toBe(5);
    await expect(api.invoke('refuse')).rejects.toBe('refused');
    await expect(api.invoke('boom')).rejects.toThrow('thrown');
  });

  it('resolves null for a command it has no route for', async () => {
    routeInvoke({ get_a: 1 });
    await expect(api.invoke('get_other')).resolves.toBeNull();
    await expect(api.invoke('constructor')).resolves.toBeNull();
  });

  it('hands an unknown command to the fallback', async () => {
    routeInvoke({ get_a: 1 }, (cmd, args) => [cmd, args]);
    await expect(api.invoke('get_other', { x: 1 })).resolves.toEqual(['get_other', { x: 1 }]);
    await expect(api.invoke('get_a')).resolves.toBe(1);
  });

  it('can reject unknown commands with unexpectedCommand', async () => {
    routeInvoke({}, unexpectedCommand);
    await expect(api.invoke('get_x')).rejects.toThrow('unexpected get_x');
  });

  it('keeps recording calls and replaces its earlier routes', async () => {
    routeInvoke({ get_a: 1 });
    routeInvoke({ get_a: 2 });
    await expect(api.invoke('get_a', { k: 1 })).resolves.toBe(2);
    expect(api.invoke).toHaveBeenCalledWith('get_a', { k: 1 });
  });
});
