import Logo from '../assets/athenaeum.svg';

export default function WelcomeScreen() {
  return (
    <div className="min-h-screen bg-surface flex items-center justify-center">
      <div className="text-center">
        <img src={Logo} alt="Athenaeum" className="w-24 h-24 mx-auto mb-4" />
        <h1 className="text-5xl font-medium text-success font-antiqua tracking-wide mb-2">ATHENAEUM</h1>
        <p className="text-content-muted mb-8">Astrophotography Library</p>

        <div className="flex items-center justify-center gap-3 text-content-muted">
          <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-success"></div>
          <span>Initializing database...</span>
        </div>
      </div>
    </div>
  );
}
