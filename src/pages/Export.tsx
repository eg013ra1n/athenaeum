import { ExportWizard } from '../components/export';

export default function Export() {
  return (
    <div className="p-6">
      <div className="mb-6">
        <h2 className="text-3xl font-bold mb-2">Export</h2>
        <p className="text-content-muted">
          Organize frame sets into PixInsight WBPP folder structure
        </p>
      </div>
      <ExportWizard />
    </div>
  );
}
