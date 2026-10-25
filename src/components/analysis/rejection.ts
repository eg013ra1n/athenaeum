import type { FrameAnalysis } from '../../types/models';
import type { RejectionThresholds } from '../calibration/RejectionThresholdBar';
import { isUnusable } from './unusable';

/**
 * Frame ids the threshold bar rejects. `plateScale` (arcsec / px) is set when the FWHM threshold was typed in arcsec;
 * it is converted back to pixels for the comparison. Frames without analysis are never rejected.
 */
export function computeRejectedFrameIds(
  frameIds: number[],
  analysisData: Map<number, FrameAnalysis>,
  thresholds: RejectionThresholds,
  plateScale: number | null,
): Set<number> {
  const rejected = new Set<number>();
  const rawFwhm = parseFloat(thresholds.fwhm);
  const fwhmThreshold = plateScale ? rawFwhm / plateScale : rawFwhm;
  const eccThreshold = parseFloat(thresholds.eccentricity);
  const snrThreshold = parseFloat(thresholds.frame_snr);
  const snrWtThreshold = parseFloat(thresholds.snr_weight);
  const rejectUnusable = thresholds.trail === 'true';

  for (const id of frameIds) {
    const a = analysisData.get(id);
    if (!a) continue;

    // FWHM > threshold, eccentricity > threshold = rejected (worse)
    if (!isNaN(fwhmThreshold) && a.median_fwhm > fwhmThreshold) { rejected.add(id); continue; }
    if (!isNaN(eccThreshold) && a.median_eccentricity > eccThreshold) { rejected.add(id); continue; }
    // Frame SNR, SNR Weight < threshold = rejected (worse)
    if (!isNaN(snrThreshold) && a.frame_snr < snrThreshold) { rejected.add(id); continue; }
    if (!isNaN(snrWtThreshold) && a.snr_weight < snrWtThreshold) { rejected.add(id); continue; }
    // Reject unusable frames: any verdict, or a legacy row by its old trailed flag (PC-15)
    if (rejectUnusable && isUnusable(a)) { rejected.add(id); continue; }
  }
  return rejected;
}
