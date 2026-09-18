// Settings redesign (spec 2026-09-18 §2/§3): the one description of the
// Settings page. This file carries METADATA only — titles, labels, help,
// keywords, order. It never holds values, defaults or render code. Two
// consumers read it: `useSettingsSearch` (search index) and `SettingsSection`
// (looks its own id up to render the title/description and to register
// itself for `?section=`).
//
// Field labels here and in the hand-written panels must not drift: KV fields
// take their label from the registry entry (`useSettingField` returns it via
// `fieldMeta`), and the panels reference the same constants
// (`FIELDS.analysis.detection.maxStars.label`, see `fieldMeta` below for the
// real accessor since a JS property key can't contain a dot the way that
// looks).

import type { LucideIcon } from 'lucide-react';
import {
  Settings as SettingsIcon,
  Eye,
  BarChart3,
  ScanSearch,
  Crosshair,
  SquareStack,
  ArrowLeftRight,
} from 'lucide-react';

export type SettingsTabId =
  | 'general'
  | 'blink'
  | 'analysis'
  | 'plate_solving'
  | 'calibration'
  | 'stacking'
  | 'transfers';

export interface SettingsFieldMeta {
  /** Unique within the section. */
  id: string;
  /** The visible label. */
  label: string;
  /** The visible help text. */
  help?: string;
  /** Extra search terms, e.g. the setting key. */
  keywords?: string[];
}

export interface SettingsSectionMeta {
  /** 'general.updates' — stable, used by ?section= and search. */
  id: string;
  tab: SettingsTabId;
  title: string;
  description?: string;
  fields: SettingsFieldMeta[];
}

export const SETTINGS_TABS: { id: SettingsTabId; label: string; icon: LucideIcon }[] = [
  { id: 'general', label: 'General', icon: SettingsIcon },
  { id: 'blink', label: 'Blink', icon: Eye },
  { id: 'analysis', label: 'Analysis', icon: BarChart3 },
  { id: 'plate_solving', label: 'Plate Solving', icon: ScanSearch },
  { id: 'calibration', label: 'Calibration', icon: Crosshair },
  { id: 'stacking', label: 'Stacking', icon: SquareStack },
  { id: 'transfers', label: 'Transfers', icon: ArrowLeftRight },
];

export const SETTINGS_SECTIONS: SettingsSectionMeta[] = [
  // ── General ──────────────────────────────────────────────────────────────
  {
    id: 'general.updates',
    tab: 'general',
    title: 'Updates',
    fields: [
      {
        id: 'autoCheck',
        label: 'Automatically check for updates on startup',
        help: 'When enabled, Athenaeum checks for a newer version each time it starts and shows a notification if one is available — on the desktop app and in the web build alike. Disable to only check manually from the About page.',
        keywords: ['updates.auto_check'],
      },
      {
        id: 'checkBeta',
        label: 'Check for beta updates',
        help: 'When enabled, the update checker will also look for pre-release (beta) versions. Beta builds may contain new features that are still being tested.',
        keywords: ['updates.check_beta'],
      },
    ],
  },
  {
    id: 'general.grouping',
    tab: 'general',
    title: 'Frame set grouping',
    description: 'How lights are grouped into frame sets by sky position.',
    fields: [
      {
        id: 'threshold',
        label: 'Grouping threshold',
        help: "Lights whose centres lie within this distance of a set's centre join that set. Seed-and-grow, single link, great-circle distance; only lights not yet in any set take part. Changing it does not regroup existing sets — Find new images and the monitor use the new value from now on.",
        keywords: ['grouping.threshold.value', 'cluster', 'radius'],
      },
      {
        id: 'thresholdUnit',
        label: 'Threshold unit',
        keywords: ['grouping.threshold.unit', 'deg', 'arcmin', 'arcsec'],
      },
    ],
  },
  {
    id: 'general.sessions',
    tab: 'general',
    title: 'Session detection',
    fields: [
      {
        id: 'gapHours',
        label: 'Session gap threshold (hours)',
        help: 'A gap longer than this between two lights starts a new night. Typical night sessions can span midnight (e.g. 19:00 Day 1 → 03:00 Day 2 = one night). Default is 6 hours.',
        keywords: ['session_gap_threshold_hours', 'night', 'imaging night'],
      },
    ],
  },
  {
    id: 'general.monitoring',
    tab: 'general',
    title: 'Monitoring',
    fields: [
      {
        id: 'enabledGlobal',
        label: 'Enable background monitoring',
        help: 'Master switch. When off, no scan roots are polled even if individually marked as "Monitor". New files are still picked up on manual scan.',
        keywords: ['monitoring.enabled_global'],
      },
      {
        id: 'intervalMinutes',
        label: 'Polling interval (minutes)',
        help: 'How often to re-scan each monitor-enabled folder for new files. The scanner is idempotent, so short intervals are fine on local drives but may be costly for large NAS directories. Default is 10 minutes.',
        keywords: ['monitoring.interval_minutes'],
      },
    ],
  },
  {
    id: 'general.autoMerge',
    tab: 'general',
    title: 'Auto-merge',
    description:
      "When enabled, new unclustered light frames that fall within the grouping threshold of an existing frame set are automatically attached to that set. Every merge is recorded in the frame set's History tab so you can audit what the algorithm did. Both settings default to off.",
    fields: [
      {
        id: 'onButtonClick',
        label: 'Skip confirmation on "Find new images"',
        help: 'When on, clicking the button merges all candidates immediately without showing a preview dialog.',
        keywords: ['auto_merge.on_button_click'],
      },
      {
        id: 'onMonitorDetect',
        label: 'Auto-attach during background monitoring',
        help: "When on, background scans that discover new lights automatically attach them to the nearest matching frame set (within the grouping threshold) without user intervention. You'll see a toast + notification bell entry for each auto-merge.",
        keywords: ['auto_merge.on_monitor_detect'],
      },
    ],
  },
  {
    id: 'general.contentIndex',
    tab: 'general',
    title: 'Content index',
    description:
      'A sampled content hash of every catalogued file — the first, middle and last 512 KB. Used to skip files the other device already has when transferring, and to group the Duplicates view by content when the option below is on. Built in the background, never during a scan.',
    fields: [
      {
        id: 'useContentHash',
        label: 'Group the Duplicates view by content',
        help: 'Off — raw sub-frames are grouped by their stored FITS/XISF header (no extra reading); masters and processed files are compared by their full contents. On — everything, masters included, is grouped by the sampled hash. Run a deep verify before deleting masters in this mode.',
        keywords: ['duplicates.use_content_hash', 'content hash', 'sampled hash'],
      },
      {
        id: 'buildIndexNow',
        label: 'Build index now',
        help: 'Manually starts the content-index job. A running job can be stopped from the job card in the sidebar.',
        keywords: ['content index job', 'indexing'],
      },
    ],
  },
  {
    id: 'general.archive',
    tab: 'general',
    title: 'Archive',
    description: 'Manage destination folders for archives in File Manager → Archive Folders.',
    fields: [
      {
        id: 'compression',
        label: 'Compression',
        help: 'FITS files compress poorly; Store is the recommended default.',
        keywords: ['archive.compression', 'store', 'deflate'],
      },
    ],
  },
  {
    id: 'general.dataLocations',
    tab: 'general',
    title: 'Data file locations',
    description:
      'Where Athenaeum stores its catalog database and log files on disk (desktop only).',
    fields: [
      {
        id: 'databasePath',
        label: 'Database',
        help: 'Path to the catalog database on disk. Click the folder icon to reveal it in your file manager.',
        keywords: ['database path', 'get_database_path'],
      },
      {
        id: 'logFolder',
        label: 'Log folder',
        help: 'Path to the JSONL log files on disk.',
        keywords: ['log directory', 'get_log_path'],
      },
    ],
  },
  {
    id: 'general.logging',
    tab: 'general',
    title: 'Logging',
    description:
      'Controls what gets written to the JSONL log file. Debug is verbose — useful while diagnosing an issue, not recommended to leave on permanently. ATHENAEUM_LOG on the server overrides these settings while set.',
    fields: [
      {
        id: 'baseLevel',
        label: 'Base log level',
        help: 'Applies everywhere unless overridden per module below. Higher verbosity (Debug) writes more to the JSONL log file.',
        keywords: ['logging level', 'error', 'warn', 'info', 'debug'],
      },
      {
        id: 'moduleOverrides',
        label: 'Module overrides',
        help: 'Per-module log level overrides: Scanner, Plate Solver, Calibration, Archive / File Ops, Transport (iroh / relays).',
        keywords: ['scanner', 'plate solver', 'calibration', 'archive', 'file ops', 'transport', 'iroh', 'relays'],
      },
    ],
  },

  // ── Blink ────────────────────────────────────────────────────────────────
  {
    id: 'blink.viewer',
    tab: 'blink',
    title: 'Blink viewer',
    fields: [
      {
        id: 'resolution',
        label: 'Image Resolution',
        help: 'Resolution for blink viewer images. Thumbnail is fastest, Preview balances speed and quality, Full shows maximum detail.',
        keywords: ['blink.resolution', 'thumbnail', 'preview', 'full resolution'],
      },
      {
        id: 'qualityThumbnail',
        label: 'Thumbnail JPEG Quality',
        help: 'JPEG quality for thumbnail images. Default: 70.',
        keywords: ['rustafits.quality.thumbnail'],
      },
      {
        id: 'qualityPreview',
        label: 'Preview JPEG Quality',
        help: 'JPEG quality for preview/blink viewer images. Default: 85.',
        keywords: ['rustafits.quality.preview'],
      },
      {
        id: 'qualityFull',
        label: 'Full Resolution JPEG Quality',
        help: 'JPEG quality for full resolution images. Default: 95.',
        keywords: ['rustafits.quality.full'],
      },
      {
        id: 'threads',
        label: 'Concurrent Processing Threads',
        help: 'Number of concurrent image processing threads. Lower values use less memory, higher values process faster.',
        keywords: ['blink.threads'],
      },
      {
        id: 'cacheSize',
        label: 'Memory Cache Size (images)',
        help: 'Maximum number of images kept in the memory cache. Default: 200.',
        keywords: ['blink.memory_cache_size'],
      },
      {
        id: 'cacheMaxMb',
        label: 'Memory Cache Limit (MB)',
        help: 'Total memory the image cache may use, whichever limit is reached first. Default: 512 MB.',
        keywords: ['blink.memory_cache_max_mb'],
      },
      {
        id: 'retentionMinutes',
        label: 'Memory Cache Retention (minutes)',
        help: 'Cached images are automatically evicted after this many minutes of inactivity. Default: 30.',
        keywords: ['blink.memory_retention_minutes'],
      },
    ],
  },
  {
    id: 'blink.flatContour',
    tab: 'blink',
    title: 'Flat Contour Plot',
    description:
      "Defaults for the per-flat contour plot rendered in Blink (toolbar mountain icon). Values match PixInsight's FlatContourPlot v1.3.1 so the visual matches that script at default settings.",
    fields: [
      {
        id: 'resolutionPct',
        label: 'Resolution (%)',
        help: 'Resampling factor. Lower = faster, less detail. PI default: 50.',
        keywords: ['flat_contour.resolution_pct'],
      },
      {
        id: 'sigmaPx',
        label: 'Sigma (px)',
        help: 'Gaussian noise-reduction sigma. PI default: 1.0.',
        keywords: ['flat_contour.sigma_px'],
      },
      {
        id: 'contours',
        label: 'Contours',
        help: 'Number of discrete bands. PI default: 15.',
        keywords: ['flat_contour.contours'],
      },
      {
        id: 'gradientPct',
        label: 'Gradient (%)',
        help: 'Boundary-emphasis strength. Higher darkens band edges more. PI default: 50.',
        keywords: ['flat_contour.gradient_pct'],
      },
    ],
  },
  {
    id: 'blink.annotations',
    tab: 'blink',
    title: 'Star Annotation Display',
    description: 'Configure how star annotations appear when toggled on in the Blink Viewer.',
    fields: [
      {
        id: 'colorScheme',
        label: 'Color Scheme',
        keywords: ['blink.annotation_config', 'eccentricity', 'fwhm', 'uniform'],
      },
      {
        id: 'lineWidth',
        label: 'Line Width',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'showDirectionTick',
        label: 'Show direction tick on elongated stars',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'eccGood',
        label: 'Eccentricity Good (<)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'eccWarn',
        label: 'Eccentricity Warn (>)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'fwhmGood',
        label: 'FWHM Good (ratio <)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'fwhmWarn',
        label: 'FWHM Warn (ratio >)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'ellipseScale',
        label: 'Ellipse Scale (×FWHM)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'minRadius',
        label: 'Min Radius (px)',
        keywords: ['blink.annotation_config'],
      },
      {
        id: 'maxRadius',
        label: 'Max Radius (px)',
        keywords: ['blink.annotation_config'],
      },
    ],
  },

  // ── Analysis ─────────────────────────────────────────────────────────────
  {
    id: 'analysis.detection',
    tab: 'analysis',
    title: 'Star Detection Parameters',
    fields: [
      {
        id: 'detectionSigma',
        label: 'Detection Sigma',
        help: 'Threshold in sigma above background (1.0-20.0)',
        keywords: ['detection_sigma'],
      },
      {
        id: 'maxStars',
        label: 'Max Stars',
        help: 'Keep brightest N stars (10-2000)',
        keywords: ['max_stars'],
      },
      {
        id: 'minStarArea',
        label: 'Min Star Area (px)',
        keywords: ['min_star_area'],
      },
      {
        id: 'maxStarArea',
        label: 'Max Star Area (px)',
        keywords: ['max_star_area'],
      },
      {
        id: 'saturationFraction',
        label: 'Saturation Fraction',
        help: 'Reject saturated stars (0.5-1.0)',
        keywords: ['saturation_fraction'],
      },
      {
        id: 'trailThreshold',
        label: 'Trail Threshold (R²)',
        help: 'R² threshold for trail detection. Higher = less sensitive. (0.0-1.0)',
        keywords: ['trail_threshold'],
      },
    ],
  },
  {
    id: 'analysis.measurement',
    tab: 'analysis',
    title: 'Measurement Method',
    fields: [
      {
        id: 'mrsLayers',
        label: 'MRS Wavelet Layers',
        help: 'MRS wavelet noise estimation layers. Higher = more accurate noise on nebula-rich fields. Default: 0 (off, 0-10).',
        keywords: ['mrs_layers'],
      },
    ],
  },
  {
    id: 'analysis.psf',
    tab: 'analysis',
    title: 'PSF Fitting',
    fields: [
      {
        id: 'measureCap',
        label: 'Measure Cap',
        help: 'Max stars to PSF-fit. 0 = measure all. Default: 2000.',
        keywords: ['measure_cap'],
      },
      {
        id: 'fitMaxIter',
        label: 'Fit Max Iterations',
        help: 'LM max iterations. Increase for accuracy, decrease for speed. Default: 25.',
        keywords: ['fit_max_iter'],
      },
      {
        id: 'fitTolerance',
        label: 'Fit Tolerance',
        help: 'LM convergence tolerance. Lower = tighter convergence. Default: 0.0001.',
        keywords: ['fit_tolerance'],
      },
      {
        id: 'fitMaxRejects',
        label: 'Fit Max Rejects',
        help: 'LM consecutive reject bailout. Default: 5.',
        keywords: ['fit_max_rejects'],
      },
    ],
  },
  {
    id: 'analysis.batch',
    tab: 'analysis',
    title: 'Batch Processing',
    fields: [
      {
        id: 'concurrentFrames',
        label: 'Concurrent Frames',
        help: 'Auto: set based on CPU cores (~1 frame per 3 cores). Manual: number of frames analyzed simultaneously (~200MB per frame).',
        keywords: ['batch_concurrency', 'auto'],
      },
    ],
  },
  {
    id: 'analysis.rejection',
    tab: 'analysis',
    title: 'Default Rejection Thresholds',
    description:
      'Pre-fill the threshold bar on the Analysis tab. Leave a field empty to not set a default for it.',
    fields: [
      {
        id: 'fwhmUnit',
        label: 'FWHM unit',
        help: 'Pixels or arcseconds — controls which unit the Analysis tab opens in.',
        keywords: ['analysis.fwhm_default_unit', 'pixels', 'arcseconds'],
      },
      {
        id: 'fwhm',
        label: 'FWHM (px) >',
        keywords: ['analysis.rejection_defaults'],
      },
      {
        id: 'eccentricity',
        label: 'Ecc >',
        keywords: ['analysis.rejection_defaults', 'eccentricity'],
      },
      {
        id: 'frameSnr',
        label: 'Frame SNR (dB) <',
        keywords: ['analysis.rejection_defaults'],
      },
      {
        id: 'snrWeight',
        label: 'SNR Wt <',
        keywords: ['analysis.rejection_defaults'],
      },
      {
        id: 'trailed',
        label: 'Trailed',
        help: 'Reject trailed frames.',
        keywords: ['analysis.rejection_defaults', 'trail'],
      },
    ],
  },

  // ── Plate Solving ────────────────────────────────────────────────────────
  {
    id: 'plateSolving.catalog',
    tab: 'plate_solving',
    title: 'Star Catalog',
    description:
      'The astrometric plate solver matches detected stars against the downloadable Gaia DR3 density-tier catalog to compute a full WCS solution.',
    fields: [
      {
        id: 'tiers',
        label: 'Catalog Tiers',
        help: 'One row per density tier the catalog server publishes; a recommendation is computed from your light frames\' field of view.',
        keywords: ['gaia', 'density', 'catalog tiers'],
      },
      {
        id: 'download',
        label: 'Download',
        help: 'Downloads the selected tier and every lower one not yet installed.',
        keywords: ['download_catalog_layers'],
      },
    ],
  },
  {
    id: 'plateSolving.solver',
    tab: 'plate_solving',
    title: 'Solver Parameters',
    fields: [
      {
        id: 'verificationTolerance',
        label: 'Verification Tolerance (arcsec)',
        help: 'Base angular tolerance for the persisted-solve confidence gate. Default 8.0".',
        keywords: ['base_verification_tolerance_arcsec'],
      },
      {
        id: 'sipOrder',
        label: 'SIP Distortion Order',
        help: 'Polynomial order for the SIP distortion fit passed to the solver (2–5).',
        keywords: ['sip_order'],
      },
      {
        id: 'autofindTolerance',
        label: 'Autofind Object Tolerance (°)',
        help: 'Maximum great-circle distance between a frame\'s RA/Dec and a named DSO for the "Autofind Object" batch action to accept the match. Default 0.5°.',
        keywords: ['autofind_tolerance_deg'],
      },
      {
        id: 'batchConcurrency',
        label: 'Batch Concurrency',
        help: 'Worker threads for batch solving. 0 means auto — cores / 3, clamped to 2–8. Default 0.',
        keywords: ['batch_concurrency'],
      },
    ],
  },
  {
    id: 'plateSolving.inputGate',
    tab: 'plate_solving',
    title: 'Input Gate',
    description:
      'A frame is refused when its own analysis reports a median star eccentricity of at least the first value and a trail-fit R² of at least the second — both, never either alone.',
    fields: [
      {
        id: 'inputGateEnabled',
        label: 'Refuse trailed frames before solving',
        keywords: ['input_gate_enabled'],
      },
      {
        id: 'maxEccentricity',
        label: 'Max Median Eccentricity',
        help: 'How elongated the average star may be before the frame is a candidate for refusal. Default 0.85.',
        keywords: ['input_max_eccentricity'],
      },
      {
        id: 'minTrailR2',
        label: 'Min Trail R²',
        help: 'How well the elongation lines up along one direction — high means a tracking failure rather than soft seeing. Default 0.65.',
        keywords: ['input_min_trail_r2'],
      },
    ],
  },

  // ── Calibration ──────────────────────────────────────────────────────────
  {
    id: 'calibration.matching',
    tab: 'calibration',
    title: 'Calibration Matching Configuration',
    description:
      'Configure how calibration frames (Flats, Darks, Bias) are matched to source frames. Define which parameters must match exactly, warn on threshold, or be ignored.',
    fields: [
      {
        id: 'lights',
        label: 'For Lights',
        help: 'Per-parameter matching rules for Lights → Flat, Dark, Bias.',
        keywords: ['instrume', 'binning', 'gain', 'offset', 'exptime', 'focallen', 'filter', 'ccd_temp'],
      },
      {
        id: 'flats',
        label: 'For Flats',
        help: 'Per-parameter matching rules for Flats → DarkFlat, Dark, Bias, plus the fallback chain.',
        keywords: [
          'instrume', 'binning', 'gain', 'offset', 'exptime', 'focallen', 'ccd_temp',
          'use bias if no darks found', 'fallback chain', 'darkflat',
        ],
      },
      {
        id: 'darks',
        label: 'For Darks',
        help: 'Per-parameter matching rules for Darks → Bias.',
        keywords: ['instrume', 'binning', 'gain', 'offset', 'exptime', 'ccd_temp', 'use bias for dark optimization'],
      },
      {
        id: 'clustering',
        label: 'Clustering Parameters & Thresholds',
        help: 'Max age and time-cluster window per calibration type, plus the scoring weights (temperature match weight/sensitivity, exposure match weight/sensitivity) used when auto-linking.',
        keywords: [
          'max age days', 'time cluster', 'temp threshold', 'scoring',
          'temperature match weight', 'temperature sensitivity',
          'exposure match weight', 'exposure sensitivity', 'refresh all calibration sets',
        ],
      },
      {
        id: 'warnings',
        label: 'Date Warning Thresholds',
        help: 'Warn when calibration frames are older than these thresholds.',
        keywords: ['flat_date_warning_days', 'dark_date_warning_days', 'darkflat_date_warning_days'],
      },
      {
        id: 'preferences',
        label: 'Master Preferences',
        help: 'Choose whether to prefer Master calibration frames or frame sets when both are available.',
        keywords: ['master_preferences', 'prefer master', 'prefer frameset', 'no preference'],
      },
    ],
  },
  {
    id: 'calibration.memory',
    tab: 'calibration',
    title: 'Master Build Memory',
    fields: [
      {
        id: 'budget',
        label: 'Integration memory budget',
        help: 'Working memory one master build may use for reading frames. 0 = automatic. Larger values read the disk in fewer, longer sweeps; smaller values use less memory and take longer.',
        keywords: ['integration.band_budget_mb', 'get_integration_band_budget'],
      },
    ],
  },
  {
    id: 'calibration.masterFormat',
    tab: 'calibration',
    title: 'Master File Format',
    fields: [
      {
        id: 'format',
        label: 'Container for masters built here',
        help: 'Applies to masters built from now on. A rebuild keeps the container the master already has. XISF masters carry the same header cards, plus the image type WBPP reads.',
        keywords: ['calibration.master_format', 'fits', 'xisf'],
      },
    ],
  },

  // ── Stacking ─────────────────────────────────────────────────────────────
  {
    id: 'stacking.pipeline',
    tab: 'stacking',
    title: 'Pipeline defaults',
    description:
      'The same nine-stage pipeline every frame set runs unless it stores its own override.',
    fields: [
      { id: 'calibrate', label: 'Calibrate' },
      { id: 'debayer', label: 'Debayer' },
      { id: 'measure', label: 'Measure & select' },
      { id: 'reference', label: 'Reference' },
      { id: 'register', label: 'Register' },
      { id: 'normalize', label: 'Local normalization' },
      { id: 'integrate', label: 'Integrate' },
      { id: 'drizzle', label: 'Drizzle' },
      { id: 'output', label: 'Output' },
    ],
  },
  {
    id: 'stacking.folders',
    tab: 'stacking',
    title: 'Default folders',
    description: "What a frame set with no override falls back to. Both folders are saved together — an unavailable folder on the other card fails the save.",
    fields: [
      {
        id: 'working',
        label: 'Working folder',
        help: 'Where a stacking run stages registered/intermediate frames, unless a frame set overrides it.',
      },
      {
        id: 'output',
        label: 'Output folder',
        help: 'Where a stacking run writes its master(s), unless a frame set overrides it.',
      },
    ],
  },

  // ── Transfers ────────────────────────────────────────────────────────────
  {
    id: 'transfers.account',
    tab: 'transfers',
    title: 'Account',
    description:
      'Sign in to link this machine to your account for syncing frames between devices. Optional — every feature works without an account.',
    fields: [
      {
        id: 'email',
        label: 'Email',
        help: "We'll email a 6-digit code to sign in.",
        keywords: ['sign in', 'send code'],
      },
      {
        id: 'verificationCode',
        label: 'Verification code',
        keywords: ['sign in', 'verify code'],
      },
      {
        id: 'deviceName',
        label: 'Device name',
        help: 'Shown to your other devices in sync history and transfers. Must be unique across your account.',
        keywords: ['rename_device'],
      },
      {
        id: 'hub',
        label: 'Hub',
        help: 'Production is where your real devices live; Test is a separate registry for trying things out. Each hub keeps its own sign-in.',
        keywords: ['account.hub_url', 'production', 'test hub'],
      },
      {
        id: 'devices',
        label: 'Devices',
        help: 'Every device registered to your account, with capability, created/last-seen dates, and revoke.',
        keywords: ['revoke device', 'capability', 'full peer', 'send-only'],
      },
    ],
  },
  {
    id: 'transfers.sync',
    tab: 'transfers',
    title: 'Sync',
    description:
      'Send frames between your machines. A Capture device queues its frames to a paired Primary; the Primary receives and ingests them.',
    fields: [
      {
        id: 'status',
        label: 'Status',
        help: 'This machine is a full peer: it always receives, and sends are explicit and per-device.',
        keywords: ['receiver active', 'get_sync_status'],
      },
      {
        id: 'pairingTicket',
        label: 'Show pairing ticket (dev)',
        help: 'Dev-only: share this ticket with a device (e.g. a Perseus agent) so it can dial this machine.',
        keywords: ['sync.dev_ticket_pairing', 'get_sync_pairing_ticket'],
      },
    ],
  },
  {
    id: 'transfers.folders',
    tab: 'transfers',
    title: 'Folders',
    description: 'Where sends are staged and where downloads are verified before they land.',
    fields: [
      {
        id: 'outgoing',
        label: 'Outgoing staging folder',
        help: 'Prepared sends are staged here until the receiver confirms them.',
        keywords: ['sync.outgoing_staging_dir', 'get_transfer_paths'],
      },
      {
        id: 'working',
        label: 'Incoming working folder',
        help: 'Downloads are verified here before landing in your Incoming folder. Same disk as Incoming = no extra copy.',
        keywords: ['sync.incoming_working_dir', 'get_transfer_paths'],
      },
    ],
  },
  {
    id: 'transfers.upload',
    tab: 'transfers',
    title: 'Upload speed limit',
    fields: [
      {
        id: 'limit',
        label: 'Upload speed limit',
        help: "Caps this device's total sync upload bandwidth. Uploads only — downloads are capped by the sending device's limit. Empty or 0 means unlimited.",
        keywords: ['sync.max_upload_bytes_per_sec', 'set_sync_upload_limit', 'MB/s'],
      },
    ],
  },
  {
    id: 'transfers.receiving',
    tab: 'transfers',
    title: 'Simultaneous incoming transfers',
    fields: [
      {
        id: 'concurrent',
        label: 'Simultaneous incoming transfers',
        help: 'How many incoming transfers download at once. Others wait their turn — transfers from the same device always arrive in order. Default 2.',
        keywords: ['sync.max_concurrent_receives', 'set_sync_max_concurrent_receives'],
      },
    ],
  },
  {
    id: 'transfers.storage',
    tab: 'transfers',
    title: 'Transfer storage',
    fields: [
      {
        id: 'storage',
        label: 'Transfer storage',
        help: 'The on-disk footprint of packages, received data and blobs.',
        keywords: ['get_transfer_storage'],
      },
      {
        id: 'cleanup',
        label: 'Clean up finished transfers',
        help: "Removes finished transfers' temporary payloads and releases orphaned download data. Received files and transfer history are untouched.",
        keywords: ['cleanup_finished_transfers'],
      },
      {
        id: 'leftovers',
        label: 'Leftovers in previous folders',
        help: 'Bytes a folder move left behind in the previous outgoing/working folders.',
        keywords: ['cleanup_transfer_leftovers'],
      },
    ],
  },
];

/** Field metadata keyed by section id then field id — e.g.
 *  `FIELDS['general.updates'].autoCheck`. Prefer `fieldMeta(section, field)`
 *  when either id is a variable rather than a literal. */
export const FIELDS: Record<string, Record<string, SettingsFieldMeta>> = Object.fromEntries(
  SETTINGS_SECTIONS.map((s) => [s.id, Object.fromEntries(s.fields.map((f) => [f.id, f]))]),
);

/** Looks up one field's metadata. Throws in every environment on a miss — a
 *  dev-time typo here is a crash, never a silently unlabeled field (spec §3). */
export function fieldMeta(section: string, field: string): SettingsFieldMeta {
  const meta = FIELDS[section]?.[field];
  if (!meta) {
    throw new Error(`[settings/registry] no field "${field}" registered under section "${section}"`);
  }
  return meta;
}

/** Looks up one section's metadata by id. */
export function sectionById(id: string): SettingsSectionMeta {
  const section = SETTINGS_SECTIONS.find((s) => s.id === id);
  if (!section) {
    throw new Error(`[settings/registry] no section "${id}" registered`);
  }
  return section;
}
