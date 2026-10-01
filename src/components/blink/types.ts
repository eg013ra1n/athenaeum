import type { BlinkSource, CollabFrameRef, FileWithFrame, FrameAnalysis } from "../../types/models";

/** One Blink entry. Every existing caller passes plain `FileWithFrame`s; a
 * project caller adds the fields below (spec §9.2). */
export type BlinkFrame = FileWithFrame & {
  /** Stable identity across a fresh `frames` array — badge updates match on it. */
  key?: string;
  /** Set for calibrated and replica entries: both image loads go through get_collab_frame_image. */
  imageRef?: { projectId: string; frame: CollabFrameRef };
  source?: BlinkSource;
  /** A short state label (e.g. `withheld`, `excluded`) shown on the row and in the context strip. */
  badge?: string;
};

/** A caller action offered on the selection in project mode, in place of the Black Hole. */
export interface BlinkAction {
  id: string;
  /** The button label for `n` eligible entries of the selection, e.g. "Don't publish (2)". */
  label: (n: number) => string;
  eligible: (f: BlinkFrame) => boolean;
  tone: 'default' | 'warn' | 'danger';
  /** Gets the eligible entries of the selection only. */
  run: (frames: BlinkFrame[]) => Promise<void> | void;
}

/** Props for the main BlinkViewer component */
export interface BlinkViewerProps {
  frames: BlinkFrame[];
  initialIndex?: number;
  onClose: () => void;
  /** Context for actions - 'light' or 'calibration' */
  sourceType?: 'light' | 'calibration';
  /** Frame set ID — used to load analysis data for the frame list */
  frameSetId?: number;
  /** Callback when frames are removed (sent to blackhole) */
  onFramesRemoved?: (frameIds: number[]) => void;
  /** Project mode: given (even empty), Blink snapshots `frames`, shows these
   * actions instead of the Black Hole, hides "Locate in file browser" and
   * renders the context strip. Omitted, Blink behaves exactly as before. */
  actions?: BlinkAction[];
  /** Project mode: the hint shown in the context strip for the current entry. */
  contextLabel?: (f: BlinkFrame) => string;
  /** Project mode: shows the "View only" chip. */
  viewOnly?: boolean;
}

/** Props for the ToolBar component */
export interface ToolBarProps {
  // Playback
  currentIndex: number;
  totalFrames: number;
  isPlaying: boolean;
  blinkSpeed: number;
  onPrevious: () => void;
  onNext: () => void;
  onTogglePlay: () => void;
  onSpeedChange: (speed: number) => void;

  // Selection
  selectionCount: number;
  blackholedInSelectionCount: number;
  nonBlackholedInSelectionCount: number;
  onClearSelection: () => void;
  onBlackhole: () => void;
  onRestore: () => void;
  isBlackholing: boolean;
  /** Project mode: these buttons replace Restore / Blackhole in the selection block.
   *  `disabled` is set on every action while any one runs — the running one may
   *  be hidden by then (its targets left the eligible set). */
  projectActions?: { id: string; label: string; tone: 'default' | 'warn' | 'danger'; busy: boolean; disabled: boolean; onClick: () => void }[];

  // Annotations
  showAnnotations: boolean;
  onToggleAnnotations: () => void;

  // Full resolution
  fullResMode: boolean;
  loadingFullRes: boolean;
  onToggleFullRes: () => void;

  // Caching
  isCaching: boolean;
  cacheProgress: { current: number; total: number };
  cacheStats: { elapsedMs: number; frameCount: number } | null;

  // Help overlay
  showHelp: boolean;
  onToggleHelp: () => void;

  // Flat contour plot — toggle (PixInsight FlatContourPlot port).
  // Enabled only when the current frame is a FLAT or MASTERFLAT.
  // `contourActive` controls the "pressed" visual state of the button.
  canShowContour: boolean;
  contourActive: boolean;
  onShowContourPlot: () => void;

  // Close
  onClose: () => void;
}

export type SortField = 'time' | 'filter' | 'exptime' | 'fwhm' | 'eccentricity' | 'frame_snr';
export type SortDirection = 'asc' | 'desc';

/** Props for the FrameList component */
export interface FrameListProps {
  frames: BlinkFrame[];
  currentIndex: number;
  selectedFrames: Set<number>;
  blackholedFileIds: Set<number>;
  loadingIndices: Set<number>;
  analysisMap: Map<number, FrameAnalysis>;
  sortField: SortField;
  sortDirection: SortDirection;
  onSortChange: (field: SortField) => void;
  onFrameClick: (index: number, e: React.MouseEvent) => void;
  onCheckboxClick: (index: number, e: React.MouseEvent) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
  onInvertSelection: () => void;
  /** Project mode: Collaboration-root paths are not browsable in the file browser. */
  hideLocate?: boolean;
}

/** Props for the FrameInfoPanel component */
export interface FrameInfoPanelProps {
  currentFrame: FileWithFrame | undefined;
  metrics: FrameAnalysis | null;
}

/** Cache progress state */
export interface CacheProgress {
  current: number;
  total: number;
}
