import type { FileWithFrame, FrameAnalysis } from "../../types/models";

/** Props for the main BlinkViewer component */
export interface BlinkViewerProps {
  frames: FileWithFrame[];
  initialIndex?: number;
  onClose: () => void;
  /** Context for actions - 'light' or 'calibration' */
  sourceType?: 'light' | 'calibration';
  /** Callback when frames are removed (sent to blackhole) */
  onFramesRemoved?: (frameIds: number[]) => void;
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

  // Annotations
  showAnnotations: boolean;
  onToggleAnnotations: () => void;

  // Caching
  isCaching: boolean;
  cacheProgress: { current: number; total: number };
  cacheStats: { elapsedMs: number; frameCount: number } | null;

  // Close
  onClose: () => void;
}

/** Props for the FrameList component */
export interface FrameListProps {
  frames: FileWithFrame[];
  currentIndex: number;
  selectedFrames: Set<number>;
  blackholedFileIds: Set<number>;
  loadingIndices: Set<number>;
  onFrameClick: (index: number, e: React.MouseEvent) => void;
  onCheckboxClick: (index: number, e: React.MouseEvent) => void;
  onSelectAll: () => void;
  onClearSelection: () => void;
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
