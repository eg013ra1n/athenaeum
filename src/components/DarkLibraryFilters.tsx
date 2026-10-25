import { useMemo, ReactNode } from "react";
import { X } from "lucide-react";
import { CalibrationSetDetail, ImageType } from "../types/models";
import { DateInputGroup, toISODate, isoToDateParts, generateYears } from './DateRangeFilter';

export type FilterMode = "darks" | "flats";

export interface FilterState {
  type: ImageType | null;
  tempBand: string | null;
  exposure: number | null;
  filter: string | null;
  expFrom: number | null;
  expTo: number | null;
  dateFrom: string | null;
  dateTo: string | null;
  gain: number | null;
  binning: string | null;
  /** `sizeKey` spelling, e.g. `9576×6388`. */
  size: string | null;
}

interface DarkLibraryFiltersProps {
  sets: CalibrationSetDetail[];
  filters: FilterState;
  onFilterChange: (filters: FilterState) => void;
  mode: FilterMode;
  actions?: ReactNode;
}

export const emptyFilters: FilterState = {
  type: null,
  tempBand: null,
  exposure: null,
  filter: null,
  expFrom: null,
  expTo: null,
  dateFrom: null,
  dateTo: null,
  gain: null,
  binning: null,
  size: null,
};

/** A set's frame size as `9576×6388`, or null when it records none. */
export function sizeKey(set: Pick<CalibrationSetDetail, 'naxis1' | 'naxis2'>): string | null {
  return set.naxis1 != null && set.naxis2 != null ? `${set.naxis1}×${set.naxis2}` : null;
}

/** The one predicate behind every calibration-library page: a filter left at
 *  null passes, so a mode that never shows a control never narrows by it. */
export function matchesLibraryFilters(set: CalibrationSetDetail, filters: FilterState): boolean {
  if (filters.type !== null && set.imagetyp !== filters.type) return false;
  if (filters.filter !== null && set.filter !== filters.filter) return false;
  if (filters.exposure !== null && set.exptime !== filters.exposure) return false;
  if (filters.expFrom !== null && set.exptime !== null && set.exptime < filters.expFrom) return false;
  if (filters.expTo !== null && set.exptime !== null && set.exptime > filters.expTo) return false;
  if (filters.gain !== null && set.gain !== filters.gain) return false;
  if (filters.binning !== null && set.binning !== filters.binning) return false;
  if (filters.size !== null && sizeKey(set) !== filters.size) return false;

  if (filters.tempBand !== null) {
    const match = filters.tempBand.match(/(-?\d+) to (-?\d+)/);
    if (match) {
      const min = parseInt(match[1]);
      const max = parseInt(match[2]);
      if (set.ccd_temp < min || set.ccd_temp >= max) return false;
    }
  }

  if (filters.dateFrom !== null && set.date_start.split('T')[0] < filters.dateFrom) return false;
  if (filters.dateTo !== null && set.date_end.split('T')[0] > filters.dateTo) return false;

  return true;
}

export default function DarkLibraryFilters({ sets, filters, onFilterChange, mode, actions }: DarkLibraryFiltersProps) {
  // Extract unique values from sets
  const uniqueValues = useMemo(() => {
    const types = Array.from(new Set(sets.map(s => s.imagetyp))).sort();
    const exposures = Array.from(new Set(sets.map(s => s.exptime).filter((e): e is number => e !== null))).sort((a, b) => a - b);
    const opticalFilters = Array.from(new Set(sets.map(s => s.filter).filter((f): f is string => f !== null && f !== ""))).sort();
    const gains = Array.from(new Set(sets.map(s => s.gain).filter((g): g is number => g !== null))).sort((a, b) => a - b);
    const binnings = Array.from(new Set(sets.map(s => s.binning).filter((b): b is string => b !== null && b !== ""))).sort();
    // Largest frame first: the full sensor, then the sub-frames.
    const area = (k: string) => k.split('×').reduce((p, n) => p * Number(n), 1);
    const sizes = Array.from(new Set(sets.map(sizeKey).filter((k): k is string => k !== null))).sort((a, b) => area(b) - area(a));

    // Generate temperature bands (5°C steps)
    const temps = sets.map(s => s.ccd_temp).filter(t => t !== null && t !== undefined);
    const minTemp = Math.floor(Math.min(...temps) / 5) * 5;
    const maxTemp = Math.ceil(Math.max(...temps) / 5) * 5;

    const tempBands: string[] = [];
    for (let temp = minTemp; temp < maxTemp; temp += 5) {
      tempBands.push(`${temp} to ${temp + 5}°C`);
    }

    // Get date range from sets
    const allDates = sets.flatMap(set => [set.date_start, set.date_end]).filter(d => d);
    const minDate = allDates.length > 0 ? allDates.sort()[0].split('T')[0] : '';
    const maxDate = allDates.length > 0 ? allDates.sort().reverse()[0].split('T')[0] : '';

    // Get exposure range for flats
    const minExp = exposures.length > 0 ? Math.min(...exposures) : 0;
    const maxExp = exposures.length > 0 ? Math.max(...exposures) : 0;

    return { types, exposures, tempBands, opticalFilters, gains, binnings, sizes, minDate, maxDate, minExp, maxExp };
  }, [sets]);

  const handleSelectChange = (field: keyof FilterState, value: string) => {
    let newValue: any = value === "" ? null : value;

    // Convert to number for numeric fields
    if ((field === "exposure" || field === "expFrom" || field === "expTo" || field === "gain") && value !== "") {
      newValue = parseFloat(value);
    }

    onFilterChange({ ...filters, [field]: newValue });
  };

  const handleClearAll = () => {
    onFilterChange(emptyFilters);
  };

  const hasActiveFilters = Object.values(filters).some(v => v !== null);

  const selectClass = "px-2 py-1.5 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-1 focus:ring-accent cursor-pointer";
  const inputClass = "px-2 py-1.5 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-1 focus:ring-accent w-20";
  const labelClass = "text-xs text-content-muted mr-1";

  return (
    <div className="flex flex-wrap items-center gap-3 mb-4 p-3 bg-surface-elevated/50 rounded-lg border border-border">
      {/* DARKS MODE: Type, Temp, Exp (dropdown), Date range */}
      {mode === "darks" && (
        <>
          {/* Type Filter - only show if multiple types */}
          {uniqueValues.types.length > 1 && (
            <div className="flex items-center">
              <span className={labelClass}>Type:</span>
              <select
                value={filters.type || ""}
                onChange={(e) => handleSelectChange("type", e.target.value)}
                className={selectClass}
              >
                <option value="">All</option>
                {uniqueValues.types.map(type => (
                  <option key={type} value={type}>{type}</option>
                ))}
              </select>
            </div>
          )}

          {/* Temperature Band Filter */}
          {uniqueValues.tempBands.length > 0 && (
            <div className="flex items-center">
              <span className={labelClass}>Temp:</span>
              <select
                value={filters.tempBand || ""}
                onChange={(e) => handleSelectChange("tempBand", e.target.value)}
                className={selectClass}
              >
                <option value="">All</option>
                {uniqueValues.tempBands.map(band => (
                  <option key={band} value={band}>{band}</option>
                ))}
              </select>
            </div>
          )}

          {/* Exposure Filter (dropdown) */}
          {uniqueValues.exposures.length > 0 && (
            <div className="flex items-center">
              <span className={labelClass}>Exp:</span>
              <select
                value={filters.exposure?.toString() || ""}
                onChange={(e) => handleSelectChange("exposure", e.target.value)}
                className={selectClass}
              >
                <option value="">All</option>
                {uniqueValues.exposures.map(exp => (
                  <option key={exp} value={exp}>{exp}s</option>
                ))}
              </select>
            </div>
          )}
        </>
      )}

      {/* FLATS MODE: Temp, Filter, Exp from-to (inputs), Date range */}
      {mode === "flats" && (
        <>
          {/* Temperature Band Filter */}
          {uniqueValues.tempBands.length > 0 && (
            <div className="flex items-center">
              <span className={labelClass}>Temp:</span>
              <select
                value={filters.tempBand || ""}
                onChange={(e) => handleSelectChange("tempBand", e.target.value)}
                className={selectClass}
              >
                <option value="">All</option>
                {uniqueValues.tempBands.map(band => (
                  <option key={band} value={band}>{band}</option>
                ))}
              </select>
            </div>
          )}

          {/* Optical Filter */}
          {uniqueValues.opticalFilters.length > 0 && (
            <div className="flex items-center">
              <span className={labelClass}>Filter:</span>
              <select
                value={filters.filter || ""}
                onChange={(e) => handleSelectChange("filter", e.target.value)}
                className={selectClass}
              >
                <option value="">All</option>
                {uniqueValues.opticalFilters.map(f => (
                  <option key={f} value={f}>{f}</option>
                ))}
              </select>
            </div>
          )}

          {/* Exposure Range (manual input) */}
          <div className="flex items-center gap-2">
            <span className={labelClass}>Exp from:</span>
            <input
              type="number"
              step="0.001"
              min="0"
              value={filters.expFrom ?? ""}
              placeholder={uniqueValues.minExp.toString()}
              onChange={(e) => handleSelectChange("expFrom", e.target.value)}
              className={inputClass}
            />
            <span className={labelClass}>to:</span>
            <input
              type="number"
              step="0.001"
              min="0"
              value={filters.expTo ?? ""}
              placeholder={uniqueValues.maxExp.toString()}
              onChange={(e) => handleSelectChange("expTo", e.target.value)}
              className={inputClass}
            />
            <span className="text-xs text-content-muted">s</span>
          </div>
        </>
      )}

      {/* Gain, Binning, Size - both modes */}
      {uniqueValues.gains.length > 0 && (
        <div className="flex items-center">
          <span className={labelClass}>Gain:</span>
          <select
            value={filters.gain?.toString() || ""}
            onChange={(e) => handleSelectChange("gain", e.target.value)}
            className={selectClass}
          >
            <option value="">All</option>
            {uniqueValues.gains.map(g => (
              <option key={g} value={g}>{g}</option>
            ))}
          </select>
        </div>
      )}

      {uniqueValues.binnings.length > 0 && (
        <div className="flex items-center">
          <span className={labelClass}>Bin:</span>
          <select
            value={filters.binning || ""}
            onChange={(e) => handleSelectChange("binning", e.target.value)}
            className={selectClass}
          >
            <option value="">All</option>
            {uniqueValues.binnings.map(b => (
              <option key={b} value={b}>{b}</option>
            ))}
          </select>
        </div>
      )}

      {uniqueValues.sizes.length > 0 && (
        <div className="flex items-center">
          <span className={labelClass}>Size:</span>
          <select
            value={filters.size || ""}
            onChange={(e) => handleSelectChange("size", e.target.value)}
            className={selectClass}
          >
            <option value="">All</option>
            {uniqueValues.sizes.map(k => (
              <option key={k} value={k}>{k}</option>
            ))}
          </select>
        </div>
      )}

      {/* Date Range - both modes */}
      <div className="flex items-center gap-2">
        <span className={labelClass}>Date:</span>
        <DateInputGroup
          value={isoToDateParts(filters.dateFrom)}
          onChange={(parts) => handleSelectChange('dateFrom', toISODate(parts) || '')}
          years={generateYears()}
        />
        <span className="text-content-muted text-sm">to</span>
        <DateInputGroup
          value={isoToDateParts(filters.dateTo)}
          onChange={(parts) => handleSelectChange('dateTo', toISODate(parts) || '')}
          years={generateYears()}
        />
      </div>

      {/* Clear Button */}
      {hasActiveFilters && (
        <button
          onClick={handleClearAll}
          className="flex items-center gap-1 px-2 py-1.5 bg-error-muted hover:bg-error/30 text-error rounded text-sm transition-colors"
          title="Clear all filters"
        >
          <X size={14} />
          Clear
        </button>
      )}

      {/* Actions (pushed to the right) */}
      {actions && (
        <div className="ml-auto flex items-center gap-2">
          {actions}
        </div>
      )}
    </div>
  );
}
