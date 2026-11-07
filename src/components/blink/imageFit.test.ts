import { describe, expect, it, vi } from 'vitest';
import { drawFitted, imageFit } from './imageFit';

describe('imageFit', () => {
  it('fits a wide image to the canvas width and never turns it', () => {
    expect(imageFit(1000, 800, 6000, 4000, true)).toEqual({ turned: false, width: 1000, height: 1000 / 1.5 });
  });

  it('turns an image taller than wide when asked, so its long side lies horizontal', () => {
    // The project grid of the owner's LDN1272 test: 4480 × 5760 lies as 5760 × 4480.
    const fit = imageFit(1000, 800, 4480, 5760, true)!;
    expect(fit.turned).toBe(true);
    expect(fit.width).toBe(1000);
    expect(fit.height).toBeCloseTo((1000 * 4480) / 5760, 9);
  });

  it('keeps a tall image upright when not asked to turn it', () => {
    const fit = imageFit(1000, 800, 4480, 5760, false)!;
    expect(fit.turned).toBe(false);
    expect(fit.width).toBeCloseTo((800 * 4480) / 5760, 9);
    expect(fit.height).toBe(800);
  });

  it('leaves a square image upright', () => {
    expect(imageFit(1000, 800, 500, 500, true)?.turned).toBe(false);
  });

  it('is null for a size that is zero or not a number', () => {
    expect(imageFit(0, 800, 10, 10, true)).toBeNull();
    expect(imageFit(1000, 800, Number.NaN, 10, true)).toBeNull();
  });
});

describe('drawFitted', () => {
  const ctx = () => ({ drawImage: vi.fn(), save: vi.fn(), restore: vi.fn(), translate: vi.fn(), rotate: vi.fn() });
  const img = {} as CanvasImageSource;

  it('draws an upright image centred, at the zoom', () => {
    const c = ctx();
    drawFitted(c as unknown as CanvasRenderingContext2D, img, { turned: false, width: 400, height: 300 }, 500, 400, 2);
    expect(c.drawImage).toHaveBeenCalledWith(img, 100, 100, 800, 600);
    expect(c.rotate).not.toHaveBeenCalled();
  });

  it('draws a turned image a quarter clockwise about the centre, its own sides swapped', () => {
    const c = ctx();
    drawFitted(c as unknown as CanvasRenderingContext2D, img, { turned: true, width: 400, height: 300 }, 500, 400, 1);
    expect(c.translate).toHaveBeenCalledWith(500, 400);
    expect(c.rotate).toHaveBeenCalledWith(Math.PI / 2);
    expect(c.drawImage).toHaveBeenCalledWith(img, -150, -200, 300, 400);
    expect(c.restore).toHaveBeenCalled();
  });
});
