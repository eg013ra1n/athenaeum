/**
 * Sky Atlas Interactive Region Selection Types
 */

export type DrawingMode = 'none' | 'circle' | 'rectangle' | 'polygon';

export interface SelectionBounds {
  raMin: number;
  raMax: number;
  decMin: number;
  decMax: number;
}

export interface SelectionCircle {
  ra: number;
  dec: number;
  radiusDegrees: number;
}

export interface SelectionPolygon {
  vertices: Array<[number, number]>; // [ra, dec]
}

export interface SelectionResult {
  frameIds: number[];
  count: number;
  totalExposureSeconds: number;
}

export interface SelectionData {
  type: 'circle' | 'rectangle' | 'polygon';
  center?: [number, number]; // For circle [ra, dec]
  radius?: number; // For circle, in degrees
  bounds?: SelectionBounds; // For rectangle
  vertices?: Array<[number, number]>; // For polygon
  frameIds: number[];
  frameCount: number;
  totalExposure: number;
}

export interface SelectionState {
  mode: DrawingMode;
  selectedFrames: Set<number>;
  persistentSelection: SelectionData | null;
}
