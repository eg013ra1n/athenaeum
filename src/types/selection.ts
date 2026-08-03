/**
 * Sky Chart Interactive Region Selection Types
 */

export type DrawingMode = 'none' | 'rectangle';

/** A candidate frame returned by the backend with its sky coordinates */
export interface SelectionCandidate {
  id: number;
  ra: number;
  dec: number;
  exposure: number;
}

/** Raw backend response with candidate frames and their coordinates */
export interface SelectionCandidates {
  candidates: SelectionCandidate[];
  count: number;
  totalExposureSeconds: number;
}

/** Final selection result after pixel-space verification */
export interface SelectionResult {
  frameIds: number[];
  count: number;
  totalExposureSeconds: number;
}
