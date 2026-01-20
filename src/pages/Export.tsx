import { ExportWizard } from '../components/export';

export default function Export() {
  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">Export to Siril</h2>
        <p className="text-content-muted">
          Export frame sets for processing with Siril - organize files, generate scripts, and run calibration
        </p>
      </div>

      <ExportWizard />
    </div>
  );
}
