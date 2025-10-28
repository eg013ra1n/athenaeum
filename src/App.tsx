import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import Layout from './components/Layout';
import FileManager from './pages/FileManager';
import ShootCalendar from './pages/ShootCalendar';
import Objects from './pages/Objects';
import FrameSetDetail from './pages/FrameSetDetail';
import Equipment from './pages/Equipment';
import Export from './pages/Export';
import Settings from './pages/Settings';

function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route path="/" element={<Layout />}>
          <Route index element={<Navigate to="/files" replace />} />
          <Route path="files" element={<FileManager />} />
          <Route path="calendar" element={<ShootCalendar />} />
          <Route path="objects" element={<Objects />} />
          <Route path="objects/:id" element={<FrameSetDetail />} />
          <Route path="equipment" element={<Equipment />} />
          <Route path="export" element={<Export />} />
          <Route path="settings" element={<Settings />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}

export default App;
