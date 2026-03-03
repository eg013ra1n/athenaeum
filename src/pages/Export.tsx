import { ExportWizard } from '../components/export';

export default function Export() {
  return (
    <div className="flex flex-col h-full">
      <div className="p-4 pt-3 pb-0">
        <h2 className="text-2xl font-bold">
          Export
          <span className="text-sm font-normal text-content-muted ml-3">Organize frame sets into PixInsight WBPP folder structure</span>
        </h2>
      </div>
      <div className="flex-1 min-h-0 p-4 pt-0">
        <ExportWizard />
      </div>
    </div>
  );
}
