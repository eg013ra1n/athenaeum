import { useState, useEffect } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { invoke } from '@tauri-apps/api/core';
import Layout from './components/Layout';
import WelcomeScreen from './components/WelcomeScreen';
import FileManager from './pages/FileManager';
import ShootCalendar from './pages/ShootCalendar';
import Objects from './pages/Objects';
import FrameSetDetail from './pages/FrameSetDetail';
import Equipment from './pages/Equipment';
import Export from './pages/Export';
import BlackHole from './pages/BlackHole';
import Settings from './pages/Settings';

function App() {
  const [dbInitialized, setDbInitialized] = useState(false);
  const [initError, setInitError] = useState<string | null>(null);

  useEffect(() => {
    const initializeDb = async () => {
      try {
        await invoke('initialize_database');
        setDbInitialized(true);
      } catch (error) {
        console.error('Failed to initialize database:', error);
        setInitError(String(error));
      }
    };

    initializeDb();
  }, []);

  if (initError) {
    return (
      <div className="min-h-screen bg-gray-900 flex items-center justify-center">
        <div className="text-center max-w-md">
          <h1 className="text-2xl font-bold text-red-400 mb-4">Database Initialization Error</h1>
          <p className="text-gray-400 mb-4">{initError}</p>
          <button
            onClick={() => window.location.reload()}
            className="px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-lg transition"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (!dbInitialized) {
    return <WelcomeScreen />;
  }

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
          <Route path="blackhole" element={<BlackHole />} />
          <Route path="settings" element={<Settings />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}

export default App;
