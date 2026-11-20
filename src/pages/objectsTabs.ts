import type { FramesSetWithCount } from '../types/helpers';

/** The Objects page's tab a frame set belongs to: Stage = auto-generated and not archived, WIP = custom and not
 *  archived, Archive = archived (custom or not). The rule the Rust core's truth-table test pins
 *  (`crates/athenaeum-core/tests/fixtures/frontend_truth_tables.json`, `frameSetTab`). */
export function frameSetTab(fs: FramesSetWithCount): 'stage' | 'wip' | 'archive' {
  if (fs.frames_set.is_archived) return 'archive';
  return fs.frames_set.is_custom ? 'wip' : 'stage';
}
