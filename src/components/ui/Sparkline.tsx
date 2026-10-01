/** Mockup `spark()` — 120×26 area + line, fill 12%. Needs ≥ 2 finite
 *  samples; a non-finite one is dropped, never drawn as a NaN point. */
export function Sparkline({ values: raw, color }: { values: number[]; color: string }) {
  const values = raw.filter(Number.isFinite);
  if (values.length < 2) return <svg width="120" height="26" aria-hidden />;
  const max = Math.max(...values) * 1.1 || 1;
  const pts = values.map((v, i) => `${((i / (values.length - 1)) * 120).toFixed(1)},${(24 - (v / max) * 22).toFixed(1)}`).join(' ');
  return (
    <svg width="120" height="26" viewBox="0 0 120 26" aria-hidden>
      <polyline points={`0,25 ${pts} 120,25`} fill={color} fillOpacity={0.12} stroke="none" />
      <polyline points={pts} fill="none" stroke={color} strokeWidth={1.4} />
    </svg>
  );
}
