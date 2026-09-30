import type { JSX } from 'react';
import { Chip, EmptyState, FilterDot, KV, MemberDot, SidePanel, StatusDot } from '../../ui';
import { formatSize } from '../format';
import { filterOrder } from './table/model';
import { PANEL_H3 } from './panelStyle';
import { useMemberColor } from './MemberColorsContext';
import type { CameraQuality, MemberSummary } from '../../../types/models';

export function roleLabel(dataRole: string): string {
  if (dataRole === 'send_receive') return 'Processor';
  if (dataRole === 'send') return 'Contributor';
  return dataRole;
}

function cameraLabel(camera: string): string {
  return camera === '' ? 'Unknown camera' : camera;
}

function groupByCamera(quality: CameraQuality[]): [string, CameraQuality[]][] {
  const groups = new Map<string, CameraQuality[]>();
  for (const q of quality) {
    const list = groups.get(q.camera);
    if (list) list.push(q);
    else groups.set(q.camera, [q]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => cameraLabel(a).localeCompare(cameraLabel(b)))
    .map(([camera, list]) => [camera, [...list].sort((a, b) => filterOrder(a.filter, b.filter))]);
}


/** Docked member card (wave 5.5 Task 13): cameras, devices and holdings. */
export default function MemberPanel({ member, onClose }: { member: MemberSummary; onClose: () => void }): JSX.Element {
  const colorOf = useMemberColor();
  const cameras = groupByCamera(member.qualityByCamera);
  return (
    <SidePanel
      label="Member details"
      onClose={onClose}
      title={
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="inline-flex items-center gap-[5px] text-[13px] font-semibold text-content">
            <MemberDot color={colorOf(member.accountId)} />
            {member.displayName}
          </span>
          {member.coordinator && <Chip tone="info">Coordinator</Chip>}
          <Chip tone="mute">{roleLabel(member.dataRole)}</Chip>
        </div>
      }
    >
      <h4 className={PANEL_H3}>Cameras</h4>
      {cameras.length === 0 ? (
        <EmptyState>Nothing published yet.</EmptyState>
      ) : (
        <div className="space-y-2">
          {cameras.map(([camera, list]) => (
            <div key={camera}>
              <b className="font-semibold text-content">{cameraLabel(camera)}</b>
              <div className="mt-0.5 space-y-0.5 text-content-secondary">
                {list.map((q, i) => (
                  <div key={`${q.filter}\u0000${i}`}>
                    <FilterDot filter={q.filter} />
                    {`${q.filter} ${q.frames} fr · x̃ FWHM ${q.medianFwhm !== null ? `${q.medianFwhm.toFixed(2)}″` : '—'} · x̃ ecc ${q.medianEcc !== null ? q.medianEcc.toFixed(2) : '—'}`}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}

      <h4 className={PANEL_H3}>Devices</h4>
      {member.devices.length === 0 ? (
        <EmptyState>No devices.</EmptyState>
      ) : (
        <KV
          items={member.devices.map((d): [string, JSX.Element] => [
            d.name ?? d.device.slice(0, 8),
            <span key={d.device} className="inline-flex items-center gap-[5px]">
              <StatusDot state={d.online ? 'online' : 'offline'} />
              {d.online ? 'online' : 'offline'}
            </span>,
          ])}
        />
      )}

      <h4 className={PANEL_H3}>Holds</h4>
      <p className="text-content-secondary">
        {`${member.holdsFrames.toLocaleString('en-US')} fr · ${formatSize(member.holdsBytes)} · ${Math.round(member.holdsShare * 100)} % of the project`}
      </p>
    </SidePanel>
  );
}
