import { ExportWizard } from '../components/export';

export default function Export() {
  return (
    <div className="flex flex-col h-full">
      <div className="p-6 pb-0">
        <h2 className="text-3xl font-bold mb-2">Export</h2>
        <p className="text-content-muted">
          Organize frame sets into PixInsight WBPP folder structure
        </p>
      </div>
      <div className="flex-1 min-h-0 p-6">
        <ExportWizard />
      </div>
    </div>
  );
}
