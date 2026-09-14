import { api } from '../api';
import type { FrameSetDetail } from '../types/models';
import { FileLocationActions } from './FileLocationActions';

/** Reuse complete membership, including archived ZIP locations, without solve filters. */
export function ObjectFileLocations({
  ids,
  compact = false,
}: {
  ids: number[];
  compact?: boolean;
}) {
  return (
    <FileLocationActions
      compact={compact}
      label={ids.length > 1 ? 'Selected object locations' : 'Object file locations'}
      loadPaths={async () => {
        const paths: string[] = [];
        for (const framesSetId of ids) {
          const detail = await api.invoke<FrameSetDetail>('get_frame_set_detail', { framesSetId });
          for (const night of detail.nights)
            for (const session of night.sessions)
              for (const { file } of session.frames) paths.push(file.archive_zip_path || file.path);
        }
        return paths;
      }}
    />
  );
}
