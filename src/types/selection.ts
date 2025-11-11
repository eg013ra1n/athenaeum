/**
 * Sky Atlas Interactive Region Selection Types
 */

export type DrawingMode = 'none' | 'rectangle';

export interface SelectionBounds {
  raMin: number;
  raMax: number;
  decMin: number;
  decMax: number;
}

export interface SelectionResult {
  frameIds: number[];
  count: number;
  totalExposureSeconds: number;
}

export interface SelectionData {
  type: 'rectangle';
  bounds: SelectionBounds;
  frameIds: number[];
  frameCount: number;
  totalExposure: number;
}

export interface SelectionState {
  mode: DrawingMode;
  selectedFrames: Set<number>;
  persistentSelection: SelectionData | null;
}
