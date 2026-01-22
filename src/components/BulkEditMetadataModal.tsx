import { useState, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { X, Check, RotateCcw, Pencil } from "lucide-react";
import { CalibrationSetDetail, CalibrationMetadataEdits } from "../types/models";

interface BulkEditMetadataModalProps {
  isOpen: boolean;
  sets: CalibrationSetDetail[];
  customMetadataSetIds: number[];
  onClose: () => void;
  onSave: () => void | Promise<void>;
}

interface EditFields {
  ccd_temp: { apply: boolean; value: string };
  gain: { apply: boolean; value: string };
  offset: { apply: boolean; value: string };
  binning: { apply: boolean; value: string };
  exptime: { apply: boolean; value: string };
}

const BINNING_OPTIONS = ["1x1", "2x2", "3x3", "4x4"];

export default function BulkEditMetadataModal({
  isOpen,
  sets,
  customMetadataSetIds,
  onClose,
  onSave,
}: BulkEditMetadataModalProps) {
  const [selectedSetIds, setSelectedSetIds] = useState<Set<number>>(new Set());
  const [editFields, setEditFields] = useState<EditFields>({
    ccd_temp: { apply: false, value: "" },
    gain: { apply: false, value: "" },
    offset: { apply: false, value: "" },
    binning: { apply: false, value: "1x1" },
    exptime: { apply: false, value: "" },
  });
  const [saving, setSaving] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Reset state when modal opens
  useEffect(() => {
    if (isOpen) {
      setSelectedSetIds(new Set());
      setEditFields({
        ccd_temp: { apply: false, value: "" },
        gain: { apply: false, value: "" },
        offset: { apply: false, value: "" },
        binning: { apply: false, value: "1x1" },
        exptime: { apply: false, value: "" },
      });
      setError(null);
    }
  }, [isOpen]);

  // Check if any selected set has custom metadata
  const selectedSetsWithCustomMetadata = useMemo(() => {
    return Array.from(selectedSetIds).filter(id => customMetadataSetIds.includes(id));
  }, [selectedSetIds, customMetadataSetIds]);

  const hasAnyFieldToApply = useMemo(() => {
    return Object.values(editFields).some(field => field.apply);
  }, [editFields]);

  const handleSelectAll = () => {
    setSelectedSetIds(new Set(sets.map(s => s.id!)));
  };

  const handleDeselectAll = () => {
    setSelectedSetIds(new Set());
  };

  const handleToggleSet = (setId: number) => {
    const newSelected = new Set(selectedSetIds);
    if (newSelected.has(setId)) {
      newSelected.delete(setId);
    } else {
      newSelected.add(setId);
    }
    setSelectedSetIds(newSelected);
  };

  const handleFieldChange = (
    field: keyof EditFields,
    key: "apply" | "value",
    value: boolean | string
  ) => {
    setEditFields(prev => ({
      ...prev,
      [field]: { ...prev[field], [key]: value },
    }));
  };

  const handleApplyChanges = async () => {
    if (selectedSetIds.size === 0) {
      setError("Please select at least one set to update");
      return;
    }

    if (!hasAnyFieldToApply) {
      setError("Please check at least one field to apply");
      return;
    }

    try {
      setSaving(true);
      setError(null);

      // Build edits object with only the fields that should be applied
      const edits: CalibrationMetadataEdits = {};

      if (editFields.ccd_temp.apply && editFields.ccd_temp.value !== "") {
        edits.ccd_temp = parseFloat(editFields.ccd_temp.value);
      }
      if (editFields.gain.apply && editFields.gain.value !== "") {
        edits.gain = parseFloat(editFields.gain.value);
      }
      if (editFields.offset.apply && editFields.offset.value !== "") {
        edits.offset = parseFloat(editFields.offset.value);
      }
      if (editFields.binning.apply && editFields.binning.value !== "") {
        edits.binning = editFields.binning.value;
      }
      if (editFields.exptime.apply && editFields.exptime.value !== "") {
        edits.exptime = parseFloat(editFields.exptime.value);
      }

      const setIds = Array.from(selectedSetIds);

      console.log("Updating calibration metadata:", { setIds, edits });

      await invoke("bulk_update_calibration_metadata", {
        setIds,
        edits,
      });

      console.log("Update complete, refreshing data...");

      // Wait for the data to be refreshed before closing
      await onSave();
      onClose();
    } catch (err) {
      setError(err as string);
    } finally {
      setSaving(false);
    }
  };

  const handleRestoreOriginals = async () => {
    if (selectedSetsWithCustomMetadata.length === 0) {
      setError("No selected sets have custom metadata to restore");
      return;
    }

    try {
      setRestoring(true);
      setError(null);

      await invoke("bulk_restore_calibration_metadata", {
        setIds: selectedSetsWithCustomMetadata,
      });

      onSave();
      onClose();
    } catch (err) {
      setError(err as string);
    } finally {
      setRestoring(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-surface-elevated rounded-lg shadow-xl max-w-4xl w-full max-h-[90vh] flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <h2 className="text-xl font-semibold">Edit Calibration Metadata</h2>
          <button
            onClick={onClose}
            className="p-1 hover:bg-surface-hover rounded transition-colors"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="flex-1 overflow-auto p-4 space-y-6">
          {/* Error message */}
          {error && (
            <div className="bg-error-muted border border-error/50 rounded-lg p-3">
              <p className="text-error text-sm">{error}</p>
            </div>
          )}

          {/* Selection Section */}
          <div>
            <div className="flex items-center justify-between mb-3">
              <h3 className="text-lg font-medium">Select Calibration Sets</h3>
              <div className="flex gap-2">
                <button
                  onClick={handleSelectAll}
                  className="px-3 py-1 text-sm bg-surface-hover hover:brightness-110 rounded transition-colors"
                >
                  Select All
                </button>
                <button
                  onClick={handleDeselectAll}
                  className="px-3 py-1 text-sm bg-surface-hover hover:brightness-110 rounded transition-colors"
                >
                  Deselect All
                </button>
              </div>
            </div>

            {/* Sets table */}
            <div className="max-h-60 overflow-y-auto border border-border rounded-lg">
              <table className="w-full text-sm">
                <thead className="bg-surface sticky top-0">
                  <tr>
                    <th className="px-3 py-2 text-left w-10"></th>
                    <th className="px-3 py-2 text-left">Type</th>
                    <th className="px-3 py-2 text-left">Temp</th>
                    <th className="px-3 py-2 text-left">Gain</th>
                    <th className="px-3 py-2 text-left">Offset</th>
                    <th className="px-3 py-2 text-left">Binning</th>
                    <th className="px-3 py-2 text-left">Exp</th>
                    <th className="px-3 py-2 text-left">Frames</th>
                    <th className="px-3 py-2 text-left w-10"></th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-border">
                  {sets.map(set => {
                    const isSelected = selectedSetIds.has(set.id!);
                    const hasCustomMetadata = customMetadataSetIds.includes(set.id!);

                    return (
                      <tr
                        key={set.id}
                        onClick={() => handleToggleSet(set.id!)}
                        className={`cursor-pointer transition-colors ${
                          isSelected ? "bg-accent/10" : "hover:bg-surface-hover"
                        }`}
                      >
                        <td className="px-3 py-2">
                          <input
                            type="checkbox"
                            checked={isSelected}
                            onChange={() => handleToggleSet(set.id!)}
                            className="rounded border-border"
                          />
                        </td>
                        <td className="px-3 py-2">{set.imagetyp}</td>
                        <td className="px-3 py-2">{set.ccd_temp.toFixed(1)}°C</td>
                        <td className="px-3 py-2">{set.gain ?? "N/A"}</td>
                        <td className="px-3 py-2">{set.offset ?? "N/A"}</td>
                        <td className="px-3 py-2">{set.binning ?? "N/A"}</td>
                        <td className="px-3 py-2">{set.exptime ? `${set.exptime}s` : "N/A"}</td>
                        <td className="px-3 py-2">{set.frame_count}</td>
                        <td className="px-3 py-2">
                          {hasCustomMetadata && (
                            <span title="Has custom metadata">
                              <Pencil size={14} className="text-warning" />
                            </span>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            <p className="text-xs text-content-muted mt-2">
              {selectedSetIds.size} of {sets.length} sets selected
              {selectedSetsWithCustomMetadata.length > 0 && (
                <span className="ml-2 text-warning">
                  ({selectedSetsWithCustomMetadata.length} with custom metadata)
                </span>
              )}
            </p>
          </div>

          {/* Edit Section */}
          <div>
            <h3 className="text-lg font-medium mb-3">Edit Values</h3>
            <p className="text-sm text-content-muted mb-4">
              Check the fields you want to update and enter new values. Only checked fields will be applied.
            </p>

            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
              {/* Temperature */}
              <div className="flex items-center gap-3">
                <input
                  type="checkbox"
                  checked={editFields.ccd_temp.apply}
                  onChange={e => handleFieldChange("ccd_temp", "apply", e.target.checked)}
                  className="rounded border-border"
                />
                <div className="flex-1">
                  <label className="block text-sm font-medium mb-1">Temperature (°C)</label>
                  <input
                    type="number"
                    step="0.1"
                    value={editFields.ccd_temp.value}
                    onChange={e => handleFieldChange("ccd_temp", "value", e.target.value)}
                    disabled={!editFields.ccd_temp.apply}
                    placeholder="-10"
                    className="w-full px-3 py-2 bg-surface border border-border rounded text-sm disabled:opacity-50"
                  />
                </div>
              </div>

              {/* Gain */}
              <div className="flex items-center gap-3">
                <input
                  type="checkbox"
                  checked={editFields.gain.apply}
                  onChange={e => handleFieldChange("gain", "apply", e.target.checked)}
                  className="rounded border-border"
                />
                <div className="flex-1">
                  <label className="block text-sm font-medium mb-1">Gain</label>
                  <input
                    type="number"
                    step="1"
                    value={editFields.gain.value}
                    onChange={e => handleFieldChange("gain", "value", e.target.value)}
                    disabled={!editFields.gain.apply}
                    placeholder="100"
                    className="w-full px-3 py-2 bg-surface border border-border rounded text-sm disabled:opacity-50"
                  />
                </div>
              </div>

              {/* Offset */}
              <div className="flex items-center gap-3">
                <input
                  type="checkbox"
                  checked={editFields.offset.apply}
                  onChange={e => handleFieldChange("offset", "apply", e.target.checked)}
                  className="rounded border-border"
                />
                <div className="flex-1">
                  <label className="block text-sm font-medium mb-1">Offset</label>
                  <input
                    type="number"
                    step="1"
                    value={editFields.offset.value}
                    onChange={e => handleFieldChange("offset", "value", e.target.value)}
                    disabled={!editFields.offset.apply}
                    placeholder="30"
                    className="w-full px-3 py-2 bg-surface border border-border rounded text-sm disabled:opacity-50"
                  />
                </div>
              </div>

              {/* Binning */}
              <div className="flex items-center gap-3">
                <input
                  type="checkbox"
                  checked={editFields.binning.apply}
                  onChange={e => handleFieldChange("binning", "apply", e.target.checked)}
                  className="rounded border-border"
                />
                <div className="flex-1">
                  <label className="block text-sm font-medium mb-1">Binning</label>
                  <select
                    value={editFields.binning.value}
                    onChange={e => handleFieldChange("binning", "value", e.target.value)}
                    disabled={!editFields.binning.apply}
                    className="w-full px-3 py-2 bg-surface border border-border rounded text-sm disabled:opacity-50"
                  >
                    {BINNING_OPTIONS.map(opt => (
                      <option key={opt} value={opt}>
                        {opt}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              {/* Exposure Time */}
              <div className="flex items-center gap-3">
                <input
                  type="checkbox"
                  checked={editFields.exptime.apply}
                  onChange={e => handleFieldChange("exptime", "apply", e.target.checked)}
                  className="rounded border-border"
                />
                <div className="flex-1">
                  <label className="block text-sm font-medium mb-1">Exposure (s)</label>
                  <input
                    type="number"
                    step="0.1"
                    value={editFields.exptime.value}
                    onChange={e => handleFieldChange("exptime", "value", e.target.value)}
                    disabled={!editFields.exptime.apply}
                    placeholder="300"
                    className="w-full px-3 py-2 bg-surface border border-border rounded text-sm disabled:opacity-50"
                  />
                </div>
              </div>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between p-4 border-t border-border">
          <div>
            {selectedSetsWithCustomMetadata.length > 0 && (
              <button
                onClick={handleRestoreOriginals}
                disabled={restoring || saving}
                className="flex items-center gap-2 px-4 py-2 bg-warning/20 hover:bg-warning/30 text-warning rounded transition-colors disabled:opacity-50"
              >
                <RotateCcw size={16} className={restoring ? "animate-spin" : ""} />
                Restore Original ({selectedSetsWithCustomMetadata.length})
              </button>
            )}
          </div>

          <div className="flex gap-3">
            <button
              onClick={onClose}
              disabled={saving || restoring}
              className="px-4 py-2 bg-surface-hover hover:brightness-110 rounded transition-colors disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              onClick={handleApplyChanges}
              disabled={
                saving ||
                restoring ||
                selectedSetIds.size === 0 ||
                !hasAnyFieldToApply
              }
              className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded transition-colors disabled:opacity-50"
            >
              <Check size={16} />
              {saving ? "Applying..." : "Apply to Selected"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
