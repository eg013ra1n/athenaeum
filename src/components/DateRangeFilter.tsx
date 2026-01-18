import { useState, useRef, useEffect } from 'react';
import { Calendar, X } from 'lucide-react';

export interface DateParts {
  day: string;
  month: string;
  year: string;
}

interface DateRangeFilterProps {
  dateFrom: DateParts | null;
  dateTo: DateParts | null;
  onFromChange: (date: DateParts | null) => void;
  onToChange: (date: DateParts | null) => void;
  yearRange?: [number, number];
}

// Generate arrays for options
const days = Array.from({ length: 31 }, (_, i) => String(i + 1).padStart(2, '0'));
const months = Array.from({ length: 12 }, (_, i) => String(i + 1).padStart(2, '0'));

interface ComboBoxProps {
  value: string;
  onChange: (value: string) => void;
  options: string[];
  placeholder: string;
  width: string;
  maxLength: number;
}

function ComboBox({ value, onChange, options, placeholder, width, maxLength }: ComboBoxProps) {
  const [isOpen, setIsOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Close dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setIsOpen(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  return (
    <div ref={containerRef} className={`relative ${width}`}>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onClick={() => setIsOpen(!isOpen)}
        placeholder={placeholder}
        maxLength={maxLength}
        className={`${width} px-1 py-1 bg-gray-700 border border-gray-600 rounded text-sm text-gray-100 text-center focus:outline-none focus:ring-1 focus:ring-blue-500 cursor-pointer`}
      />
      {isOpen && (
        <div
          className="fixed max-h-40 overflow-y-auto bg-gray-800 border border-gray-600 rounded shadow-xl"
          style={{ zIndex: 9999 }}
          ref={(el) => {
            if (el && containerRef.current) {
              const rect = containerRef.current.getBoundingClientRect();
              el.style.top = `${rect.bottom + 2}px`;
              el.style.left = `${rect.left}px`;
              el.style.width = `${rect.width}px`;
            }
          }}
        >
          {options.map((opt) => (
            <button
              key={opt}
              type="button"
              onClick={() => {
                onChange(opt);
                setIsOpen(false);
              }}
              className={`w-full py-1 text-center text-sm hover:bg-gray-600 ${
                value === opt ? 'bg-blue-600 text-white' : 'text-gray-100'
              }`}
            >
              {opt}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export interface DateInputGroupProps {
  value: DateParts | null;
  onChange: (date: DateParts | null) => void;
  years: string[];
}

export function DateInputGroup({ value, onChange, years }: DateInputGroupProps) {
  const day = value?.day || '';
  const month = value?.month || '';
  const year = value?.year || '';

  const updateField = (field: keyof DateParts, newValue: string) => {
    const updated: DateParts = {
      day: value?.day || '',
      month: value?.month || '',
      year: value?.year || '',
      [field]: newValue,
    };

    // If all fields are empty, set to null
    if (!updated.day && !updated.month && !updated.year) {
      onChange(null);
    } else {
      onChange(updated);
    }
  };

  return (
    <div className="flex items-center">
      <ComboBox
        value={day}
        onChange={(v) => updateField('day', v)}
        options={days}
        placeholder="DD"
        width="w-9"
        maxLength={2}
      />

      <span className="text-gray-500 px-0.5">/</span>

      <ComboBox
        value={month}
        onChange={(v) => updateField('month', v)}
        options={months}
        placeholder="MM"
        width="w-9"
        maxLength={2}
      />

      <span className="text-gray-500 px-0.5">/</span>

      <ComboBox
        value={year}
        onChange={(v) => updateField('year', v)}
        options={years}
        placeholder="YYYY"
        width="w-14"
        maxLength={4}
      />
    </div>
  );
}

export function DateRangeFilter({
  dateFrom,
  dateTo,
  onFromChange,
  onToChange,
  yearRange,
}: DateRangeFilterProps) {
  // Generate years array
  const currentYear = new Date().getFullYear();
  const startYear = yearRange?.[0] || currentYear - 10;
  const endYear = yearRange?.[1] || currentYear;
  const years = Array.from(
    { length: endYear - startYear + 1 },
    (_, i) => String(endYear - i) // Descending order (most recent first)
  );

  const hasFilter = dateFrom !== null || dateTo !== null;

  const clearFilters = () => {
    onFromChange(null);
    onToChange(null);
  };

  return (
    <div className="flex items-center gap-2">
      <Calendar size={14} className="text-gray-400 flex-shrink-0" />

      <DateInputGroup
        value={dateFrom}
        onChange={onFromChange}
        years={years}
      />

      <span className="text-gray-500 text-sm">to</span>

      <DateInputGroup
        value={dateTo}
        onChange={onToChange}
        years={years}
      />

      {hasFilter && (
        <button
          onClick={clearFilters}
          className="p-1.5 text-gray-400 hover:text-gray-200 hover:bg-gray-700 rounded transition"
          title="Clear date filter"
        >
          <X size={16} />
        </button>
      )}
    </div>
  );
}

// Helper to convert DateParts to ISO date string for comparison
export function toISODate(d: DateParts | null): string | null {
  if (!d || !d.year || !d.month || !d.day) return null;
  const year = d.year.padStart(4, '0');
  const month = d.month.padStart(2, '0');
  const day = d.day.padStart(2, '0');
  return `${year}-${month}-${day}`;
}

// Helper to convert ISO date string to DateParts
export function isoToDateParts(iso: string | null): DateParts | null {
  if (!iso) return null;
  const parts = iso.split('-');
  if (parts.length !== 3) return null;
  const [year, month, day] = parts;
  if (!year || !month || !day) return null;
  return { day, month, year };
}

// Helper to generate years array
export function generateYears(startYear?: number, endYear?: number): string[] {
  const currentYear = new Date().getFullYear();
  const start = startYear || currentYear - 10;
  const end = endYear || currentYear;
  return Array.from(
    { length: end - start + 1 },
    (_, i) => String(end - i)
  );
}
