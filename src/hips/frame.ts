import { isHipsView, stereographicFov, type HipsView } from './view';

// Run Aladin in its own local document: removing the iframe releases its WebGL
// context, animation loop and document listeners when the background is closed.
const surveyUrl = 'https://alasky.cds.unistra.fr/DSS/DSSColor';
let aladin: ReturnType<(typeof import('aladin-lite'))['default']['aladin']> | undefined;
let latestView: HipsView | undefined;

function report(status: 'ready' | 'error', message?: string) {
  window.parent.postMessage({ type: 'athenaeum-hips-status', status, message }, '*');
}

function syncView() {
  if (!aladin || !latestView) return;
  const { ra, dec, rotation, scale } = latestView;
  aladin.gotoRaDec(ra, dec);
  aladin.setFoV(stereographicFov(aladin.getSize()[0], scale));
  // Aladin's positive roll is opposite to D3's screen roll. Version 3.8.2
  // mistakenly ignores numeric zero in setRotation; 360 resets it equivalently.
  aladin.setRotation(-rotation || 360);
}

window.addEventListener('message', event => {
  if (event.source !== window.parent || event.data?.type !== 'athenaeum-hips-view') return;
  if (!isHipsView(event.data.view)) return;
  latestView = event.data.view;
  syncView();
});

async function initialize() {
  try {
    // Check an actual survey image as well as letting Aladin load metadata.
    // A blocked/offline image server must not replace the usable original sky
    // with an empty canvas. The HiPS renderer requests detailed tiles itself.
    const allSky = new Image();
    allSky.crossOrigin = 'anonymous';
    await new Promise<void>((resolve, reject) => {
      allSky.onload = () => resolve();
      allSky.onerror = () => reject(new Error('DSS survey images could not be loaded'));
      allSky.src = `${surveyUrl}/Norder3/Allsky.jpg`;
    });
    // The library eagerly initializes some survey metadata on import. Keep it
    // out of the offline path, and out of the original star-chart bundle.
    const { default: A } = await import('aladin-lite');
    await A.init;
    const survey = A.HiPS(surveyUrl, {
      name: 'DSS2 color',
      imgFormat: 'jpeg',
      successCallback: () => {
        // The survey may initialize the view asynchronously; apply the latest
        // chart position again after that initialization before showing it.
        requestAnimationFrame(() => {
          syncView();
          report('ready');
        });
      },
      errorCallback: (error: unknown) => {
        console.error('Failed to load DSS HiPS:', error);
        report('error', 'DSS color could not be loaded. Check your connection and retry.');
      },
    });
    aladin = A.aladin(document.getElementById('survey')!, {
      survey,
      projection: 'STG',
      cooFrame: 'ICRS',
      target: '0 0',
      fov: 90,
      showReticle: false,
      showCooGrid: false,
      showCooLocation: false,
      showZoomControl: false,
      showFullscreenControl: false,
      showLayersControl: false,
      showGotoControl: false,
      showSimbadPointerControl: false,
      showContextMenu: false,
      showSettingsControl: false,
      showShareControl: false,
      showFrame: false,
      showFov: false,
      showStatusBar: false,
      showProjectionControl: false,
    });
    syncView();
    new ResizeObserver(syncView).observe(document.getElementById('survey')!);
  } catch (error) {
    console.error('Failed to initialize DSS HiPS:', error);
    report(
      'error',
      'DSS color requires WebGL2 and an internet connection. The original chart is still available.',
    );
  }
}

void initialize();
