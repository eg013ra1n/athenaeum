import { getFieldLegend, isSkyFieldMetric, type SkyFieldMetric } from '../utils/skyFieldStyle';
import type { SkyBackground } from '../hooks/useSkyBackground';

interface SkyFieldControlsProps {
  metric: SkyFieldMetric;
  disabled: boolean;
  onMetricChange: (metric: SkyFieldMetric) => void;
  background: SkyBackground;
  backgroundBusy: boolean;
  onBackgroundChange: (background: SkyBackground) => void;
}

export function SkyFieldControls({
  metric,
  disabled,
  onMetricChange,
  background,
  backgroundBusy,
  onBackgroundChange,
}: SkyFieldControlsProps) {
  return (
    <div className="flex-shrink-0 flex flex-wrap items-center gap-x-5 gap-y-2 px-4 py-2 border-b border-border bg-surface-elevated text-xs">
      <div className="flex items-center gap-2">
        <label htmlFor="sky-background" className="text-content-muted">
          Background
        </label>
        <select
          id="sky-background"
          value={background}
          disabled={backgroundBusy}
          onChange={event => {
            if (event.target.value === 'original' || event.target.value === 'dss-color')
              onBackgroundChange(event.target.value);
          }}
          className="px-2 py-1 rounded bg-surface-hover text-content border border-border focus:outline-none focus:ring-2 focus:ring-accent disabled:opacity-50"
        >
          <option value="original">Original star chart</option>
          <optgroup label="HiPS (online)">
            <option value="dss-color">DSS colored</option>
          </optgroup>
        </select>
      </div>
      <div className="flex items-center gap-2">
        <label htmlFor="sky-field-metric" className="text-content-muted">
          Color fields by
        </label>
        <select
          id="sky-field-metric"
          value={metric}
          disabled={disabled}
          onChange={event => {
            if (isSkyFieldMetric(event.target.value)) onMetricChange(event.target.value);
          }}
          className="px-2 py-1 rounded bg-surface-hover text-content border border-border focus:outline-none focus:ring-2 focus:ring-accent disabled:opacity-50"
        >
          <option value="grouping">Grouping</option>
          <option value="frames">Exposure count</option>
          <option value="integration">Integration time</option>
        </select>
      </div>
      <ul
        aria-label="Field color legend"
        className="flex flex-wrap items-center gap-x-4 gap-y-1 text-content-secondary"
      >
        {getFieldLegend(metric).map(style => (
          <li key={style.label} className="flex items-center gap-1.5">
            <svg width="12" height="12" aria-hidden="true" className={style.className}>
              <rect
                x="1"
                y="1"
                width="10"
                height="10"
                fill="currentColor"
                fillOpacity="0.28"
                stroke="currentColor"
              />
            </svg>
            {style.label}
          </li>
        ))}
      </ul>
      {metric !== 'grouping' && (
        <span className="text-content-muted">Confirmed versions counted once; stacks excluded</span>
      )}
      {background === 'dss-color' && (
        <a
          href="https://aladin.cds.unistra.fr/hips/"
          target="_blank"
          rel="noreferrer"
          className="text-accent underline"
        >
          DSS · STScI/NASA · CDS / Aladin Lite · online
        </a>
      )}
    </div>
  );
}
