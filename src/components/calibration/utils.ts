import type {
  CalibrationHierarchyView,
  CalibrationFilterGroup,
} from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';
import type { CameraFilterNode } from './CameraFilterTree';
import type { AggregatedWarning } from './WarningPanel';

export interface CameraFilterData {
  nodes: CameraFilterNode[];
  framesByKey: Map<string, EnrichedLightFrame[]>;
  allFrames: EnrichedLightFrame[];
}

/**
 * Build a unique filter key that includes exptime to differentiate same filter with different exposures.
 */
export function buildFilterKey(filterGroup: CalibrationFilterGroup): string {
  const baseFilter = filterGroup.filter ?? '__no_filter__';
  return filterGroup.exptime !== null
    ? `${baseFilter}:${filterGroup.exptime}`
    : baseFilter;
}

/**
 * Collect warnings from a filter group.
 */
export function collectFilterGroupWarnings(filterGroup: CalibrationFilterGroup): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];

  const hasFlats = filterGroup.flat_sets.length > 0;
  const hasDarks = filterGroup.dark_sets.length > 0;
  const pluralSuffix = filterGroup.frame_count !== 1 ? 's' : '';

  if (!hasFlats && !hasDarks) {
    warnings.push({
      message: `No calibration linked (${filterGroup.frame_count} frame${pluralSuffix})`,
      type: 'missing_calibration',
      filter: filterGroup.filter ?? undefined,
    });
  } else {
    if (!hasFlats) {
      warnings.push({
        message: `Missing Flat calibration (${filterGroup.frame_count} frame${pluralSuffix})`,
        type: 'missing_calibration',
        filter: filterGroup.filter ?? undefined,
      });
    }
    if (!hasDarks) {
      warnings.push({
        message: `Missing Dark calibration (${filterGroup.frame_count} frame${pluralSuffix})`,
        type: 'missing_calibration',
        filter: filterGroup.filter ?? undefined,
      });
    }
  }

  const addSetWarnings = (sets: typeof filterGroup.flat_sets, setType: string) => {
    for (const setWithCount of sets) {
      for (const warning of setWithCount.warnings) {
        warnings.push({
          message: `${setType}: ${warning.message}`,
          type: warning.warning_type as 'date' | 'temperature',
          filter: filterGroup.filter ?? undefined,
        });
      }
    }
  };

  addSetWarnings(filterGroup.flat_sets, 'Flat');
  addSetWarnings(filterGroup.dark_sets, 'Dark');

  return warnings;
}

/**
 * Build a 2-level camera→filter tree from the calibration hierarchy data.
 * Merges across date groups so each camera+filter combination appears once.
 */
export function buildCameraFilterTree(hierarchy: CalibrationHierarchyView): CameraFilterData {
  const cameraMap = new Map<string, Map<string, { label: string; frames: EnrichedLightFrame[] }>>();
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

        if (!filterMap.has(key)) {
          filterMap.set(key, { label, frames: [] });
        }

        const enriched: EnrichedLightFrame[] = filterGroup.light_frames.map(f => ({
          ...f,
          camera,
          filter: filterGroup.filter,
        }));

        filterMap.get(key)!.frames.push(...enriched);
        allFrames.push(...enriched);
      }
    }
  }

  const nodes: CameraFilterNode[] = [];
  const framesByKey = new Map<string, EnrichedLightFrame[]>();

  for (const [camera, filterMap] of cameraMap) {
    const filters: CameraFilterNode['filters'] = [];

    for (const [key, data] of filterMap) {
      filters.push({
        key,
        label: data.label,
        frameCount: data.frames.length,
      });
      framesByKey.set(key, data.frames);
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
