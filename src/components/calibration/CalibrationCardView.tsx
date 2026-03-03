import { useMemo, useState, useCallback } from 'react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
} from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';
import { CalibrationGroupCard, type FlatGroupData, type DarkOnlyGroupData } from './CalibrationGroupCard';
import { CalibrationLightsTable } from './CalibrationLightsTable';

interface CalibrationCardViewProps {
  data: CalibrationHierarchyViewData;
  allFrames: EnrichedLightFrame[];
  /** Frame IDs currently visible from CameraFilterTree filtering */
  visibleFrameIds?: Set<number>;
  onOpenGroup: (type: 'flat' | 'dark', groupData: FlatGroupData | DarkOnlyGroupData) => void;
  onManualCalibration?: (frameIds: number[]) => void;
}

interface DerivedGroups {
  flatGroups: FlatGroupData[];
  darkOnlyGroups: DarkOnlyGroupData[];
  uncoveredLights: EnrichedLightFrame[];
}

/**
 * Derive card groupings from hierarchy data.
 * Groups lights by their linked flat set, dark-only set, or uncovered.
 */
function deriveGroups(data: CalibrationHierarchyViewData, allFrames: EnrichedLightFrame[]): DerivedGroups {
  // Build frame lookup by frame_id
  const frameById = new Map<number, EnrichedLightFrame>();
  for (const f of allFrames) {
    frameById.set(f.frame_id, f);
  }

  // Track which frames are assigned to flat groups or dark-only groups
  const assignedFrameIds = new Set<number>();

  // 1. Flat groups: collect unique flat sets, map lights to them
  const flatSetMap = new Map<number, { set: typeof data.date_groups[0]['camera_groups'][0]['filter_groups'][0]['flat_sets'][0]; lights: EnrichedLightFrame[] }>();

  for (const dateGroup of data.date_groups) {
    for (const cameraGroup of dateGroup.camera_groups) {
      for (const filterGroup of cameraGroup.filter_groups) {
        for (const flatSet of filterGroup.flat_sets) {
          if (flatSet.set.id === null) continue;
          if (!flatSetMap.has(flatSet.set.id)) {
            flatSetMap.set(flatSet.set.id, { set: flatSet, lights: [] });
          }
          // Add the lights that use this flat set
          for (const frameId of flatSet.frame_ids) {
            const frame = frameById.get(frameId);
            if (frame && !assignedFrameIds.has(frameId)) {
              flatSetMap.get(flatSet.set.id)!.lights.push(frame);
              assignedFrameIds.add(frameId);
            }
          }
        }
      }
    }
  }

  const flatGroups: FlatGroupData[] = [];
  for (const [, entry] of flatSetMap) {
    if (entry.lights.length > 0) {
      flatGroups.push({ flatSet: entry.set, lights: entry.lights });
    }
  }

  // 2. Dark-only groups: lights where flat_set_id is null but dark_set_id is set
  const darkSetMap = new Map<number, { set: typeof data.date_groups[0]['camera_groups'][0]['filter_groups'][0]['dark_sets'][0]; lights: EnrichedLightFrame[] }>();

  for (const frame of allFrames) {
    if (assignedFrameIds.has(frame.frame_id)) continue;
    const darkSetId = frame.calibration_status.dark_set_id;
    if (darkSetId === null) continue;

    if (!darkSetMap.has(darkSetId)) {
      // Find the dark set detail from hierarchy
      let darkSetData: typeof data.date_groups[0]['camera_groups'][0]['filter_groups'][0]['dark_sets'][0] | null = null;
      outer: for (const dg of data.date_groups) {
        for (const cg of dg.camera_groups) {
          for (const fg of cg.filter_groups) {
            for (const ds of fg.dark_sets) {
              if (ds.set.id === darkSetId) {
                darkSetData = ds;
                break outer;
              }
            }
          }
        }
      }
      if (darkSetData) {
        darkSetMap.set(darkSetId, { set: darkSetData, lights: [] });
      }
    }

    const entry = darkSetMap.get(darkSetId);
    if (entry) {
      entry.lights.push(frame);
      assignedFrameIds.add(frame.frame_id);
    }
  }

  const darkOnlyGroups: DarkOnlyGroupData[] = [];
  for (const [, entry] of darkSetMap) {
    if (entry.lights.length > 0) {
      darkOnlyGroups.push({ darkSet: entry.set, lights: entry.lights });
    }
  }

  // 3. Uncovered: remaining unassigned lights
  const uncoveredLights = allFrames.filter(f => !assignedFrameIds.has(f.frame_id));

  return { flatGroups, darkOnlyGroups, uncoveredLights };
}

export function CalibrationCardView({
  data,
  allFrames,
  visibleFrameIds,
  onOpenGroup,
  onManualCalibration,
}: CalibrationCardViewProps) {
  const [uncoveredSelectedIds, setUncoveredSelectedIds] = useState<Set<number>>(new Set());

  const { flatGroups, darkOnlyGroups, uncoveredLights } = useMemo(
    () => deriveGroups(data, allFrames),
    [data, allFrames]
  );

  // Filter groups by visible frame IDs from CameraFilterTree
  const filteredFlatGroups = useMemo(() => {
    if (!visibleFrameIds) return flatGroups;
    return flatGroups
      .map(g => ({ ...g, lights: g.lights.filter(f => visibleFrameIds.has(f.frame_id)) }))
      .filter(g => g.lights.length > 0);
  }, [flatGroups, visibleFrameIds]);

  const filteredDarkOnlyGroups = useMemo(() => {
    if (!visibleFrameIds) return darkOnlyGroups;
    return darkOnlyGroups
      .map(g => ({ ...g, lights: g.lights.filter(f => visibleFrameIds.has(f.frame_id)) }))
      .filter(g => g.lights.length > 0);
  }, [darkOnlyGroups, visibleFrameIds]);

  const filteredUncovered = useMemo(() => {
    if (!visibleFrameIds) return uncoveredLights;
    return uncoveredLights.filter(f => visibleFrameIds.has(f.frame_id));
  }, [uncoveredLights, visibleFrameIds]);

  const handleManualCalibrationUncovered = useCallback(() => {
    if (onManualCalibration && uncoveredSelectedIds.size > 0) {
      onManualCalibration([...uncoveredSelectedIds]);
    }
  }, [onManualCalibration, uncoveredSelectedIds]);

  return (
    <div className="flex-1 min-w-0 overflow-y-auto space-y-6 pr-1">
      {/* Flat Groups */}
      {filteredFlatGroups.length > 0 && (
        <section>
          <h3 className="text-sm font-semibold text-content mb-2">
            Flat Groups
            <span className="ml-2 text-xs font-normal text-content-muted">
              {filteredFlatGroups.length} set{filteredFlatGroups.length !== 1 ? 's' : ''}
            </span>
          </h3>
          <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
            {filteredFlatGroups.map(group => (
              <CalibrationGroupCard
                key={`flat-${group.flatSet.set.id}`}
                type="flat"
                data={group}
                allLightFrames={allFrames}
                onClick={() => onOpenGroup('flat', group)}
              />
            ))}
          </div>
        </section>
      )}

      {/* Dark Only Groups */}
      {filteredDarkOnlyGroups.length > 0 && (
        <section>
          <h3 className="text-sm font-semibold text-content mb-2">
            Dark Only
            <span className="ml-2 text-xs font-normal text-warning">
              {filteredDarkOnlyGroups.length} group{filteredDarkOnlyGroups.length !== 1 ? 's' : ''} without flats
            </span>
          </h3>
          <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
            {filteredDarkOnlyGroups.map(group => (
              <CalibrationGroupCard
                key={`dark-${group.darkSet.set.id}`}
                type="dark"
                data={group}
                allLightFrames={allFrames}
                onClick={() => onOpenGroup('dark', group)}
              />
            ))}
          </div>
        </section>
      )}

      {/* Uncovered Lights */}
      {filteredUncovered.length > 0 && (
        <section>
          <div className="flex items-center gap-2 mb-2">
            <h3 className="text-sm font-semibold text-content">
              Uncovered Lights
            </h3>
            <span className="text-xs text-error">
              {filteredUncovered.length} frame{filteredUncovered.length !== 1 ? 's' : ''} without calibration
            </span>
            {onManualCalibration && uncoveredSelectedIds.size > 0 && (
              <button
                onClick={handleManualCalibrationUncovered}
                className="ml-auto text-xs px-2 py-1 bg-accent hover:bg-accent-hover text-white rounded transition-colors"
              >
                Assign Calibration ({uncoveredSelectedIds.size})
              </button>
            )}
          </div>
          <div className="border border-border rounded-xl overflow-hidden">
            <CalibrationLightsTable
              frames={filteredUncovered}
              selectedFrameIds={uncoveredSelectedIds}
              onSelectionChange={setUncoveredSelectedIds}
              compact
            />
          </div>
        </section>
      )}

      {/* Empty state */}
      {filteredFlatGroups.length === 0 && filteredDarkOnlyGroups.length === 0 && filteredUncovered.length === 0 && (
        <div className="flex items-center justify-center py-12">
          <p className="text-content-muted">No calibration groups match the current filter</p>
        </div>
      )}
    </div>
  );
}
