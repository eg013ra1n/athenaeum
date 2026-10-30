import { useNavigate } from 'react-router-dom';
import { Chip } from '../ui/Chip';

/** What `navigate('/files', { state })` carries to open the Duplicates tab. */
export interface DuplicatesTabRequest {
  openDuplicates: true;
}
export const DUPLICATES_TAB_REQUEST: DuplicatesTabRequest = { openDuplicates: true };

export function duplicatesLabel(count: number): string {
  return `${count} duplicate${count === 1 ? '' : 's'}`;
}

/**
 * "N duplicates" on a frame set: second copies of frames the set already holds (two disk copies, or a disk
 * copy and a zip entry). They are not counted and never stacked, exported or measured; the click opens the
 * Duplicates page. Renders nothing when there are none.
 */
export function FrameSetDuplicatesChip({ count }: { count: number }) {
  const navigate = useNavigate();
  if (count <= 0) return null;
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        navigate('/files', { state: DUPLICATES_TAB_REQUEST });
      }}
      className="inline-flex"
    >
      <Chip
        tone="warn"
        className="cursor-pointer hover:opacity-80"
        title="Frames held twice (two disk copies, or a disk copy and a zip entry): one copy is used. Click to see them on the Duplicates page."
      >
        {duplicatesLabel(count)}
      </Chip>
    </button>
  );
}

/** The row chip on the copy a frame set does not use. */
export function DuplicateNotUsedChip({ className = '' }: { className?: string }) {
  return (
    <Chip tone="mute" className={className} title="Another copy of this frame is in the set and is the one used: this copy is not counted, stacked, exported or measured">
      duplicate — not used
    </Chip>
  );
}
