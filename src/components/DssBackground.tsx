import { useCallback, useEffect, useRef, useState } from 'react';
import type { HipsView } from '../hips/view';

interface DssBackgroundProps {
  enabled: boolean;
  mapReady: boolean;
  onReady: (ready: boolean) => void;
}

export function DssBackground({ enabled, mapReady, onReady }: DssBackgroundProps) {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [attempt, setAttempt] = useState(0);
  const [status, setStatus] = useState<'loading' | 'ready' | 'error'>('loading');
  const [error, setError] = useState('');
  const syncRef = useRef<(() => void) | null>(null);
  const registered = useRef(false);

  const sync = useCallback(() => {
    const frame = frameRef.current;
    const canvas = document.querySelector('#celestial-map > canvas') as HTMLCanvasElement | null;
    const projection = window.Celestial?.mapProjection;
    if (!frame || !canvas || !projection) return;
    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const width = canvas.width / dpr;
    const height = canvas.height / dpr;
    if (width <= 0 || height <= 0) return;
    const [tx, ty] = projection.translate();
    const [ra, dec] = projection.invert([tx, ty]);
    // Use the same backing-store → CSS scaling as the existing field overlays.
    // The projection center can differ by a fraction of a pixel from the canvas
    // center because Celestial rounds the canvas size; preserve that offset.
    const sx = rect.width / width,
      sy = rect.height / height;
    // Aladin rounds its viewport to integer CSS pixels. Make that rounding
    // explicit, including at fractional device pixel ratios, and compensate
    // the resulting half-pixel center shift rather than stretching the sky.
    const rendererWidth = Math.ceil(width),
      rendererHeight = Math.ceil(height);
    frame.style.width = `${rendererWidth}px`;
    frame.style.height = `${rendererHeight}px`;
    frame.style.left = `${(tx - rendererWidth / 2) * sx}px`;
    frame.style.top = `${(ty - rendererHeight / 2) * sy}px`;
    frame.style.transform = `scale(${sx}, ${sy})`;
    const view: HipsView = { ra, dec, rotation: projection.rotate()[2], scale: projection.scale() };
    frame.contentWindow?.postMessage({ type: 'athenaeum-hips-view', view }, '*');
  }, []);

  useEffect(() => {
    syncRef.current = enabled ? sync : null;
    if (mapReady && !registered.current) {
      window.Celestial.add({
        type: 'raw',
        callback: () => syncRef.current?.(),
        redraw: () => syncRef.current?.(),
      });
      registered.current = true;
    }
    if (enabled) sync();
    return () => {
      syncRef.current = null;
    };
  }, [enabled, mapReady, sync]);

  useEffect(() => {
    if (!enabled) {
      onReady(false);
      return;
    }
    setStatus('loading');
    onReady(false);
    const timeout = window.setTimeout(() => {
      console.error('DSS background loading timed out');
      setStatus('error');
      setError('DSS color is taking too long to load. Check your connection and retry.');
      onReady(false);
    }, 30000);
    const receive = (event: MessageEvent) => {
      if (
        event.source !== frameRef.current?.contentWindow ||
        event.data?.type !== 'athenaeum-hips-status'
      )
        return;
      if (event.data.status !== 'ready' && event.data.status !== 'error') return;
      window.clearTimeout(timeout);
      setStatus(event.data.status);
      setError(
        typeof event.data.message === 'string'
          ? event.data.message
          : 'DSS color could not be loaded.',
      );
      sync();
      onReady(event.data.status === 'ready');
    };
    window.addEventListener('message', receive);
    return () => {
      window.clearTimeout(timeout);
      window.removeEventListener('message', receive);
      onReady(false);
    };
  }, [enabled, attempt, onReady, sync]);

  if (!enabled) return null;

  return (
    <>
      <iframe
        key={attempt}
        ref={frameRef}
        src={`${import.meta.env.BASE_URL}hips.html`}
        title="DSS color HiPS imagery"
        aria-hidden="true"
        tabIndex={-1}
        onLoad={sync}
        className="absolute border-0 pointer-events-none origin-top-left"
        style={{ visibility: status === 'ready' ? 'visible' : 'hidden' }}
      />
      {status !== 'ready' && (
        <div
          role="status"
          className="absolute bottom-3 left-3 z-10 max-w-sm rounded border border-border bg-surface-elevated p-3 text-sm text-content"
        >
          {status === 'loading'
            ? 'Loading DSS color HiPS… Original chart shown while loading.'
            : error}
          {status === 'error' && (
            <button
              type="button"
              onClick={() => setAttempt(value => value + 1)}
              className="ml-2 text-accent underline focus:outline-none focus:ring-2 focus:ring-accent"
            >
              Retry DSS
            </button>
          )}
        </div>
      )}
    </>
  );
}
