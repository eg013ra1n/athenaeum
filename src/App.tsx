import { useState, useEffect } from 'react';
import { BrowserRouter, Routes, Route, Navigate } from 'react-router-dom';
import { api } from './api';
import { isTauri } from './utils/platform';
import Layout from './components/Layout';
import WelcomeScreen from './components/WelcomeScreen';
import WebAuthGate from './components/WebAuthGate';
import FileManager from './pages/FileManager';
import ShootCalendar from './pages/ShootCalendar';
import Objects from './pages/Objects';
import FrameSetDetail from './pages/FrameSetDetail';
import Projects from './pages/Projects';
import Equipment from './pages/Equipment';
import BlackHole from './pages/BlackHole';
import SkyChart from './pages/SkyChart';
import Settings from './pages/Settings';
import About from './pages/About';
import ExcludedFrames from './pages/ExcludedFrames';

function App() {
  if (!isTauri) {
    return (
      <WebAuthGate>
        <AppContent />
      </WebAuthGate>
    );
  }
  return <AppContent />;
}

function AppContent() {
  const [dbInitialized, setDbInitialized] = useState(false);
  const [initError, setInitError] = useState<string | null>(null);

  useEffect(() => {
    const initializeDb = async () => {
      try {
        await api.invoke('initialize_database');
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
      <div className="min-h-screen bg-surface flex items-center justify-center">
        <div className="text-center max-w-md">
          <h1 className="text-2xl font-bold text-error mb-4">Database Initialization Error</h1>
          <p className="text-content-muted mb-4">{initError}</p>
          <button
            onClick={() => window.location.reload()}
            className="px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-lg transition"
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
          <Route path="projects" element={<Projects />} />
          <Route path="excluded" element={<ExcludedFrames />} />
          <Route path="skychart" element={<SkyChart />} />
          <Route path="equipment" element={<Equipment />} />
          <Route path="blackhole" element={<BlackHole />} />
          <Route path="settings" element={<Settings />} />
          <Route path="about" element={<About />} />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}

export default App;
