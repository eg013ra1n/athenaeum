// How Blink lays an image into its canvas at zoom 1 (fit to view). An aligned frame of a project run lies on the
// project's grid, which can stand taller than wide; Blink turns such a frame a quarter clockwise so its long side is
// horizontal (owner, 2026-11-07). The turn is the same for every frame of one grid, so the stars stay still.

export interface ImageFit {
  /** The image is drawn turned a quarter clockwise. */
  turned: boolean;
  /** The image's size on the canvas at zoom 1, after the turn. */
  width: number;
  height: number;
}

/** The fit of a `imgW × imgH` image into a `canvasW × canvasH` canvas; `turnTall` turns an image taller than wide.
 *  `null` when a size is not a finite, positive number. */
export function imageFit(canvasW: number, canvasH: number, imgW: number, imgH: number, turnTall: boolean): ImageFit | null {
  if (![canvasW, canvasH, imgW, imgH].every((v) => Number.isFinite(v) && v > 0)) return null;
  const turned = turnTall && imgH > imgW;
  const [w, h] = turned ? [imgH, imgW] : [imgW, imgH];
  const canvasAspect = canvasW / canvasH;
  const imageAspect = w / h;
  return imageAspect > canvasAspect
    ? { turned, width: canvasW, height: canvasW / imageAspect }
    : { turned, width: canvasH * imageAspect, height: canvasH };
}

/** Draw `img` at its fit, centred on `(centerX, centerY)` and scaled by `zoom`. */
export function drawFitted(
  ctx: CanvasRenderingContext2D,
  img: CanvasImageSource,
  fit: ImageFit,
  centerX: number,
  centerY: number,
  zoom: number,
): void {
  const w = fit.width * zoom;
  const h = fit.height * zoom;
  if (!fit.turned) {
    ctx.drawImage(img, centerX - w / 2, centerY - h / 2, w, h);
    return;
  }
  // Turned: the image's own width spans the canvas height, and its height the canvas width.
  ctx.save();
  ctx.translate(centerX, centerY);
  ctx.rotate(Math.PI / 2);
  ctx.drawImage(img, -h / 2, -w / 2, h, w);
  ctx.restore();
}
