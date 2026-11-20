import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, render, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { api } from '../../api';
import { routeInvoke } from '../../test/apiMock';
import { MissingMetadataView } from './MissingMetadataView';

vi.mock('../../api');
// The panels, the viewer and the side editor are not under test; each would need its own providers.
vi.mock('../plate-solve/PlateSolveBatchPanel', () => ({ PlateSolveBatchPanel: () => null }));
vi.mock('../plate-solve/FillObjectsPanel', () => ({ FillObjectsPanel: () => null }));
vi.mock('../BlinkViewer', () => ({ default: () => null }));
vi.mock('../dualpane/MetadataPane', () => ({ default: () => null }));

type Handler = (p: unknown) => void;
let handlers: Map<string, Set<Handler>>;

const loads = () => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_frames_with_missing_metadata').length;

beforeEach(() => {
  handlers = new Map();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((name: string, cb: Handler) => {
    if (!handlers.has(name)) handlers.set(name, new Set());
    handlers.get(name)!.add(cb);
    return Promise.resolve(() => handlers.get(name)!.delete(cb));
  }) as never);
  routeInvoke({ get_frames_with_missing_metadata: [] });
});
afterEach(() => cleanup());

describe('MissingMetadataView', () => {
  it('missing_metadata_reloads_on_event', async () => {
    const view = render(
      <MemoryRouter>
        <MissingMetadataView />
      </MemoryRouter>,
    );
    await waitFor(() => expect(loads()).toBe(1));
    await waitFor(() => expect(handlers.get('frames-metadata-changed')?.size).toBe(1));

    // The assistant's metadata plan was applied: the view asks the catalog again.
    await act(async () => {
      handlers.get('frames-metadata-changed')!.forEach((h) => h({ count: 4 }));
    });
    await waitFor(() => expect(loads()).toBe(2));

    // Unmounted: the listener is gone and a late event loads nothing.
    view.unmount();
    expect(handlers.get('frames-metadata-changed')!.size).toBe(0);
  });

  it('missing_metadata_reloads_after_an_assistant_solve', async () => {
    render(
      <MemoryRouter>
        <MissingMetadataView />
      </MemoryRouter>,
    );
    await waitFor(() => expect(loads()).toBe(1));
    await waitFor(() => expect(handlers.get('plate-solve-complete')?.size).toBe(1));
    await waitFor(() => expect(handlers.get('plate-solve-started')?.size).toBe(1));

    // A solve started in Athenaeum is refreshed by its own panel: no second reload here.
    await act(async () => {
      handlers.get('plate-solve-started')!.forEach((h) => h({ origin: 'ui', runId: 'solve-u', total: 2 }));
      handlers.get('plate-solve-complete')!.forEach((h) => h({ solved: 2, failed: 0, total: 2, total_time_ms: 1 }));
    });
    expect(loads()).toBe(1);

    // An assistant's solve wrote coordinates: the list asks the catalog again.
    await act(async () => {
      handlers.get('plate-solve-started')!.forEach((h) => h({ origin: 'assistant', runId: 'solve-a', total: 2 }));
    });
    await act(async () => {
      handlers.get('plate-solve-complete')!.forEach((h) => h({ solved: 2, failed: 0, total: 2, total_time_ms: 1 }));
    });
    await waitFor(() => expect(loads()).toBe(2));
  });
});
