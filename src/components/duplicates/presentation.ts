// What the Duplicates page and the assistant's plan window share: the one byte format and the frame-type colours.

/** The Duplicates page's byte format: bytes under 1 KiB, else megabytes, else gigabytes. */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const mb = bytes / 1024 / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

const IMAGETYP_STYLES: Record<string, string> = {
  LIGHT: 'bg-accent/15 text-accent border-accent/40',
  FLAT: 'bg-warning-muted text-warning border-warning/50',
  DARK: 'bg-surface-hover text-content-secondary border-border',
  BIAS: 'bg-info-muted text-info border-info/50',
  DARKFLAT: 'bg-surface-hover text-content-secondary border-border',
};

/** The colours of a frame-type badge (`LIGHT`, `FLAT`, ...). */
export function imagetypClass(value: string): string {
  const key = value.toUpperCase();
  return IMAGETYP_STYLES[key] ?? 'bg-surface-hover text-content-secondary border-border';
}
