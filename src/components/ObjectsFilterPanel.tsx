import { Search, Calendar, MapPin, X } from 'lucide-react';
import { DateInputGroup, toISODate, isoToDateParts, generateYears } from './DateRangeFilter';

export interface ObjectsFilterState {
  dateFrom: string | null;
  dateTo: string | null;
  nameSearch: string;
  coordRa: string;
  coordDec: string;
  coordRadius: string;
  coordRadiusUnit: 'arcmin' | 'deg';
  coordEnabled: boolean;
}

export const emptyFilterState: ObjectsFilterState = {
  dateFrom: null,
  dateTo: null,
  nameSearch: '',
  coordRa: '',
  coordDec: '',
  coordRadius: '60',
  coordRadiusUnit: 'arcmin',
  coordEnabled: false,
};

interface ObjectsFilterPanelProps {
  filters: ObjectsFilterState;
  onChange: (filters: ObjectsFilterState) => void;
  isOpen: boolean;
}

const inputClass = "px-2 py-1.5 bg-surface-hover border border-border rounded text-sm text-content focus:outline-none focus:ring-1 focus:ring-accent";
const labelClass = "text-xs text-content-muted mb-1";

export function ObjectsFilterPanel({ filters, onChange, isOpen }: ObjectsFilterPanelProps) {
  if (!isOpen) return null;

  const updateFilter = <K extends keyof ObjectsFilterState>(key: K, value: ObjectsFilterState[K]) => {
    onChange({ ...filters, [key]: value });
  };

  const hasActiveFilters =
    filters.nameSearch.trim() !== '' ||
    filters.dateFrom !== null ||
    filters.dateTo !== null ||
    filters.coordEnabled;

  const clearAllFilters = () => {
    onChange(emptyFilterState);
  };

  return (
    <div className="mb-4 p-4 bg-surface-elevated border border-border rounded-lg">
      <div className="flex flex-wrap gap-6">
        {/* Name Search */}
        <div className="flex flex-col">
          <label className={labelClass}>
            <Search size={12} className="inline mr-1" />
            Name
          </label>
          <input
            type="text"
            value={filters.nameSearch}
            onChange={(e) => updateFilter('nameSearch', e.target.value)}
            placeholder="Search..."
            className={`${inputClass} w-40`}
          />
        </div>

        {/* Date Range */}
        <div className="flex flex-col">
          <label className={labelClass}>
            <Calendar size={12} className="inline mr-1" />
            Date Range
          </label>
          <div className="flex items-center gap-2">
            <DateInputGroup
              value={isoToDateParts(filters.dateFrom)}
              onChange={(parts) => updateFilter('dateFrom', toISODate(parts))}
              years={generateYears()}
            />
            <span className="text-content-muted text-sm">to</span>
            <DateInputGroup
              value={isoToDateParts(filters.dateTo)}
              onChange={(parts) => updateFilter('dateTo', toISODate(parts))}
              years={generateYears()}
            />
          </div>
        </div>

        {/* Coordinates */}
        <div className="flex flex-col">
          <label className={labelClass}>
            <MapPin size={12} className="inline mr-1" />
            Coordinates
          </label>
          <div className="flex items-center gap-2">
            <button
              onClick={() => updateFilter('coordEnabled', !filters.coordEnabled)}
              className={`px-2 py-1.5 rounded text-sm transition-colors ${
                filters.coordEnabled
                  ? 'bg-accent text-white'
                  : 'bg-surface-hover text-content-secondary hover:bg-surface-hover'
              }`}
              title="Enable coordinate filtering"
            >
              {filters.coordEnabled ? 'On' : 'Off'}
            </button>
            <input
              type="text"
              value={filters.coordRa}
              onChange={(e) => updateFilter('coordRa', e.target.value)}
              placeholder="RA (12:30:00 or 180)"
              disabled={!filters.coordEnabled}
              className={`${inputClass} w-36 ${!filters.coordEnabled ? 'opacity-50' : ''}`}
            />
            <input
              type="text"
              value={filters.coordDec}
              onChange={(e) => updateFilter('coordDec', e.target.value)}
              placeholder="Dec (-45:00:00 or -45)"
              disabled={!filters.coordEnabled}
              className={`${inputClass} w-40 ${!filters.coordEnabled ? 'opacity-50' : ''}`}
            />
            <input
              type="number"
              value={filters.coordRadius}
              onChange={(e) => updateFilter('coordRadius', e.target.value)}
              placeholder="Radius"
              disabled={!filters.coordEnabled}
              min="0"
              step="1"
              className={`${inputClass} w-20 ${!filters.coordEnabled ? 'opacity-50' : ''}`}
            />
            <button
              onClick={() => updateFilter('coordRadiusUnit', filters.coordRadiusUnit === 'arcmin' ? 'deg' : 'arcmin')}
              disabled={!filters.coordEnabled}
              className={`px-2 py-1.5 rounded text-sm transition-colors ${
                !filters.coordEnabled
                  ? 'bg-surface-hover text-content-muted opacity-50'
                  : 'bg-surface-hover text-content-secondary hover:bg-surface-hover'
              }`}
              title="Toggle radius unit"
            >
              {filters.coordRadiusUnit === 'arcmin' ? "'" : '°'}
            </button>
          </div>
        </div>

        {/* Clear All */}
        {hasActiveFilters && (
          <div className="flex flex-col justify-end">
            <button
              onClick={clearAllFilters}
              className="flex items-center gap-1 px-3 py-1.5 bg-surface-hover hover:bg-surface-hover text-content-secondary rounded text-sm transition-colors"
            >
              <X size={14} />
              Clear All
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// Coordinate parsing utilities
export function parseRa(input: string): number | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

  // Try decimal degrees first
  const decimal = parseFloat(trimmed);
  if (!isNaN(decimal) && decimal >= 0 && decimal <= 360) {
    return decimal;
  }

  // Try HMS format: "12:30:45" or "12 30 45" or "12h30m45s"
  const hmsMatch = trimmed.match(/(\d+)[h:\s]+(\d+)[m:\s]+(\d+(?:\.\d+)?)/i);
  if (hmsMatch) {
    const h = parseFloat(hmsMatch[1]);
    const m = parseFloat(hmsMatch[2]);
    const s = parseFloat(hmsMatch[3]);
    if (h >= 0 && h < 24 && m >= 0 && m < 60 && s >= 0 && s < 60) {
      return (h + m / 60 + s / 3600) * 15; // Convert hours to degrees
    }
  }

  // Try simpler HMS format: "12:30" (no seconds)
  const hmMatch = trimmed.match(/^(\d+)[h:\s]+(\d+(?:\.\d+)?)$/i);
  if (hmMatch) {
    const h = parseFloat(hmMatch[1]);
    const m = parseFloat(hmMatch[2]);
    if (h >= 0 && h < 24 && m >= 0 && m < 60) {
      return (h + m / 60) * 15;
    }
  }

  return null;
}

export function parseDec(input: string): number | null {
  const trimmed = input.trim();
  if (!trimmed) return null;

  // Try decimal degrees first
  const decimal = parseFloat(trimmed);
  if (!isNaN(decimal) && decimal >= -90 && decimal <= 90) {
    return decimal;
  }

  // Try DMS format: "-45:30:15" or "-45 30 15" or "-45d30m15s"
  const dmsMatch = trimmed.match(/([+-]?\d+)[d°:\s]+(\d+)[m':\s]+(\d+(?:\.\d+)?)/i);
  if (dmsMatch) {
    const sign = dmsMatch[1].startsWith('-') ? -1 : 1;
    const d = Math.abs(parseFloat(dmsMatch[1]));
    const m = parseFloat(dmsMatch[2]);
    const s = parseFloat(dmsMatch[3]);
    if (d <= 90 && m < 60 && s < 60) {
      return sign * (d + m / 60 + s / 3600);
    }
  }

  // Try simpler DMS format: "-45:30" (no seconds)
  const dmMatch = trimmed.match(/^([+-]?\d+)[d°:\s]+(\d+(?:\.\d+)?)$/i);
  if (dmMatch) {
    const sign = dmMatch[1].startsWith('-') ? -1 : 1;
    const d = Math.abs(parseFloat(dmMatch[1]));
    const m = parseFloat(dmMatch[2]);
    if (d <= 90 && m < 60) {
      return sign * (d + m / 60);
    }
  }

  return null;
}

export function angularDistance(ra1: number, dec1: number, ra2: number, dec2: number): number {
  const toRad = (deg: number) => deg * Math.PI / 180;
  const dRa = toRad(ra2 - ra1);
  const dDec = toRad(dec2 - dec1);
  const a = Math.sin(dDec / 2) ** 2 + Math.cos(toRad(dec1)) * Math.cos(toRad(dec2)) * Math.sin(dRa / 2) ** 2;
  return 2 * Math.asin(Math.sqrt(a)) * 180 / Math.PI;
}

export function countActiveFilters(filters: ObjectsFilterState): number {
  let count = 0;
  if (filters.nameSearch.trim()) count++;
  if (filters.dateFrom || filters.dateTo) count++;
  if (filters.coordEnabled) count++;
  return count;
}
