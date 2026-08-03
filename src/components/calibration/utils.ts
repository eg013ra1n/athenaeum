import type {
  CalibrationHierarchyView,
} from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';

/** Warning with additional context for display */
export interface AggregatedWarning {
  message: string;
  type: 'missing_calibration' | 'date' | 'temperature';
  filter?: string;
  camera?: string;
}

export interface DateCameraFilterNode {
  dateKey: string;
  dateDisplay: string;
  totalFrameCount: number;
  cameras: {
    camera: string;
    totalFrameCount: number;
    filters: { key: string; label: string; frameCount: number }[];
  }[];
}

export interface CameraFilterData {
  nodes: DateCameraFilterNode[];
  framesByKey: Map<string, EnrichedLightFrame[]>;
  allFrames: EnrichedLightFrame[];
}

export interface MergedCameraFilterNode {
  camera: string;
  totalFrameCount: number;
  filters: { key: string; label: string; frameCount: number }[];
}

export interface MergedCameraFilterData {
  nodes: MergedCameraFilterNode[];
  framesByKey: Map<string, EnrichedLightFrame[]>;
  allFrames: EnrichedLightFrame[];
}

/**
 * Build a 3-level date→camera→filter tree from the calibration hierarchy data.
 * Each date group becomes a top-level node containing cameras and their filters.
 */
export function buildCameraFilterTree(hierarchy: CalibrationHierarchyView): CameraFilterData {
  const nodes: DateCameraFilterNode[] = [];
  const framesByKey = new Map<string, EnrichedLightFrame[]>();
  const allFrames: EnrichedLightFrame[] = [];

  for (const dateGroup of hierarchy.date_groups) {
    const cameras: DateCameraFilterNode['cameras'] = [];

    for (const cameraGroup of dateGroup.camera_groups) {
      const camera = cameraGroup.instrume;
      const filters: DateCameraFilterNode['cameras'][0]['filters'] = [];

      for (const filterGroup of cameraGroup.filter_groups) {
        const filterName = filterGroup.filter ?? 'No Filter';
        const exptime = filterGroup.exptime;
        const key = `${dateGroup.date}::${camera}::${filterName}::${exptime ?? 'any'}`;
        const label = exptime != null ? `${filterName} (${exptime}s)` : filterName;

        const enriched: EnrichedLightFrame[] = filterGroup.light_frames.map(f => ({
          ...f,
          camera,
          filter: filterGroup.filter,
        }));

        filters.push({
          key,
          label,
          frameCount: enriched.length,
        });

        framesByKey.set(key, enriched);
        allFrames.push(...enriched);
      }

      filters.sort((a, b) => a.label.localeCompare(b.label));

      cameras.push({
        camera,
        totalFrameCount: filters.reduce((sum, f) => sum + f.frameCount, 0),
        filters,
      });
    }

    cameras.sort((a, b) => a.camera.localeCompare(b.camera));

    nodes.push({
      dateKey: dateGroup.date,
      dateDisplay: dateGroup.date_display,
      totalFrameCount: cameras.reduce((sum, c) => sum + c.totalFrameCount, 0),
      cameras,
    });
  }

  // Sort dates chronologically (newest first)
  nodes.sort((a, b) => b.dateKey.localeCompare(a.dateKey));

  return { nodes, framesByKey, allFrames };
}

/**
 * Build a 2-level camera→filter tree merged across all dates.
 * Used by the Analysis tab where date grouping is not needed.
 */
export function buildMergedCameraFilterTree(hierarchy: CalibrationHierarchyView): MergedCameraFilterData {
  const cameraMap = new Map<string, Map<string, { label: string; frames: EnrichedLightFrame[] }>>();
  const framesByKey = new Map<string, EnrichedLightFrame[]>();
  const allFrames: EnrichedLightFrame[] = [];

  for (const dateGroup of hierarchy.date_groups) {
    for (const cameraGroup of dateGroup.camera_groups) {
      const camera = cameraGroup.instrume;

      if (!cameraMap.has(camera)) {
        cameraMap.set(camera, new Map());
      }
      const filterMap = cameraMap.get(camera)!;

      for (const filterGroup of cameraGroup.filter_groups) {
        const filterName = filterGroup.filter ?? 'No Filter';
        const exptime = filterGroup.exptime;
        const key = `${camera}::${filterName}::${exptime ?? 'any'}`;
        const label = exptime != null ? `${filterName} (${exptime}s)` : filterName;

        const enriched: EnrichedLightFrame[] = filterGroup.light_frames.map(f => ({
          ...f,
          camera,
          filter: filterGroup.filter,
        }));

        if (!filterMap.has(key)) {
          filterMap.set(key, { label, frames: [] });
        }
        filterMap.get(key)!.frames.push(...enriched);
        allFrames.push(...enriched);
      }
    }
  }

  const nodes: MergedCameraFilterNode[] = [];

  for (const [camera, filterMap] of cameraMap) {
    const filters: MergedCameraFilterNode['filters'] = [];

    for (const [key, { label, frames }] of filterMap) {
      filters.push({ key, label, frameCount: frames.length });
      framesByKey.set(key, frames);
    }

    filters.sort((a, b) => a.label.localeCompare(b.label));

    nodes.push({
      camera,
      totalFrameCount: filters.reduce((sum, f) => sum + f.frameCount, 0),
      filters,
    });
  }

  nodes.sort((a, b) => a.camera.localeCompare(b.camera));

  return { nodes, framesByKey, allFrames };
}
