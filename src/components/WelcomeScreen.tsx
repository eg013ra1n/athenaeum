export default function WelcomeScreen() {
  return (
    <div className="min-h-screen bg-surface flex items-center justify-center">
      <div className="text-center">
        <h1 className="text-5xl font-bold text-content mb-4">Athenaeum</h1>
        <p className="text-content-muted mb-8">Astrophotography Image Management</p>

        <div className="flex items-center justify-center gap-3 text-content-muted">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-accent"></div>
          <span>Initializing database...</span>
        </div>
      </div>
    </div>
  );
}
