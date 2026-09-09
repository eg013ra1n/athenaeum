import type { EquipmentProfile } from '../../types/models';

// Editable starting values, not measurements or an inferred equipment match.
export const emptyProfile = (): EquipmentProfile => ({
  id: 0,
  revision: 0,
  name: '',
  telescope: '',
  camera: '',
  focalLengthMm: 500,
  opticalMultiplier: 1,
  pixelSizeUm: 3.76,
  binning: 1,
  tolerancePercent: 5,
});

export function EquipmentProfileForm({
  value,
  cameras,
  busy,
  onChange,
  onSave,
  onCancel,
}: {
  value: EquipmentProfile;
  cameras: string[];
  busy: boolean;
  onChange: (value: EquipmentProfile) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const renderNumericField = (
    key: 'focalLengthMm' | 'opticalMultiplier' | 'pixelSizeUm' | 'binning' | 'tolerancePercent',
    label: string,
    min: number,
    max?: number,
  ) => (
    <label className="flex flex-col gap-1 text-xs">
      {label}
      <input
        type="number"
        required
        min={min}
        max={max}
        step={key === 'binning' ? 1 : 'any'}
        value={value[key]}
        onChange={e => onChange({ ...value, [key]: Number(e.target.value) })}
        className="bg-surface border border-border rounded p-2"
      />
    </label>
  );
  return (
    <form
      onSubmit={e => {
        e.preventDefault();
        onSave();
      }}
    >
      <fieldset disabled={busy} className="grid grid-cols-2 lg:grid-cols-4 gap-3 my-3">
        <label className="flex flex-col gap-1 text-xs">
          Configuration name
          <input
            required
            maxLength={256}
            value={value.name}
            onChange={e => onChange({ ...value, name: e.target.value })}
            className="bg-surface border border-border rounded p-2"
          />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          Telescope / optical train
          <input
            required
            maxLength={256}
            value={value.telescope}
            onChange={e => onChange({ ...value, telescope: e.target.value })}
            className="bg-surface border border-border rounded p-2"
          />
        </label>
        <label className="flex flex-col gap-1 text-xs">
          Exact camera name
          <input
            required
            maxLength={256}
            list="equipment-camera-names"
            value={value.camera}
            onChange={e => onChange({ ...value, camera: e.target.value })}
            className="bg-surface border border-border rounded p-2"
          />
        </label>
        <datalist id="equipment-camera-names">
          {cameras.map(camera => (
            <option key={camera} value={camera} />
          ))}
        </datalist>
        {renderNumericField('focalLengthMm', 'Native focal length (mm)', 0.001)}
        {renderNumericField('opticalMultiplier', 'Reducer / Barlow factor (1 = unchanged)', 0.001)}
        {renderNumericField('pixelSizeUm', 'Unbinned pixel pitch (µm)', 0.001)}
        {renderNumericField('binning', 'Symmetric binning', 1, 16)}
        {renderNumericField('tolerancePercent', 'Scale tolerance (%)', 0.1, 50)}
        <div className="flex gap-3 items-end">
          <button type="submit" className="bg-accent text-on-accent rounded px-3 py-2">
            Save configuration
          </button>
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
        </div>
      </fieldset>
      <p className="text-xs text-content-muted">
        Use physical unbinned pixel pitch. Resampled images can have a different scale. Editing a
        configuration makes its previous confirmations require review.
      </p>
    </form>
  );
}
