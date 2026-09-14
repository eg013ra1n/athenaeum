async page => {
  await page.unrouteAll();
  await page.route('**/src/api/index.ts*', route =>
    route.fulfill({
      contentType: 'application/javascript',
      body: `
    window.__exposureIds=[1,4]; window.__statsFail=false;
    export const api={invoke:async(command)=>{if(command==='get_effective_exposure_frame_ids'){if(window.__statsFail)throw Error('Fixture statistics unavailable');return window.__exposureIds;}return null;},listen:async()=>()=>{}};
  `,
    }),
  );
  await page.route('**/src/App.tsx*', async route => {
    const original = await (await route.fetch()).text();
    const router = original.match(/["']([^"']*react-router-dom[^"']*)["']/)[1];
    await route.fulfill({
      contentType: 'application/javascript',
      body: `
      import React from '/node_modules/.vite/deps/react.js';
      import {BrowserRouter} from '${router}';
      import {NotificationProvider} from '/src/contexts/NotificationContext.tsx';
      import {LightsAnalysisTable} from '/src/components/calibration/LightsAnalysisTable.tsx';
      import {CalibrationTableView} from '/src/components/calibration/CalibrationTableView.tsx';
      const frames=[1,2,3,4].map(id=>({frame_id:id,filename:'version_'+id+'.fits',file_path:'/fixture/'+id,exptime:id===4?60:300,date_obs:'2026-05-21T11:31:25Z',camera:'QHY',filter:'G',ccd_temp:-10,focallen:500,calibration_status:{dark_set_id:null,flat_set_id:null,bias_set_id:null},warnings:[],ra:270,dec:-20}));
      const analysis=new Map(frames.map(f=>[f.frame_id,{frame_id:f.frame_id,stars_detected:100,median_fwhm:2,median_eccentricity:0.4,median_snr:10,frame_snr:20,psf_signal:1,snr_weight:1,trail_r_squared:0,median_beta:null}]));
      const hierarchy={date_groups:[{date:'2026-05-21',date_display:'2026-05-21',camera_groups:[{instrume:'QHY',filter_groups:[{filter:'G',filter_display:'G',exptime:300,light_frames:frames,flat_sets:[],dark_sets:[],bias_sets:[],frame_count:4}],frame_count:4}],frame_count:4}],total_frames:4,calibrated_frames:0,uncalibrated_frames:4};
      const h=React.createElement;
      export default function App(){return h(BrowserRouter,null,h(NotificationProvider,null,h('main',{className:'p-4'},h('section',{'aria-label':'Analysis statistics'},h(LightsAnalysisTable,{frames,analysisData:analysis,selectedFrameIds:new Set(),onSelectionChange:()=>{}})),h('section',{'aria-label':'Calibration statistics',style:{height:500}},h(CalibrationTableView,{data:hierarchy,allFrames:frames,analysisData:analysis})))));}
    `,
    });
  });
  await page.goto('http://127.0.0.1:1420/objects');
  const analysis = page.getByRole('region', { name: 'Analysis statistics' });
  await analysis.getByText(/4 files · 2 exposures after confirmed links/).waitFor();
  await analysis
    .getByRole('button', { name: /Exposure/ })
    .filter({ hasText: '6.0m' })
    .waitFor();
  const calibration = page.getByRole('region', { name: 'Calibration statistics' });
  await calibration.getByText('6m 0s', { exact: true }).waitFor();
  await calibration.getByRole('cell', { name: '23.0', exact: true }).waitFor();
  await page.evaluate(() => {
    window.__exposureIds = [1, 2, 4];
    window.dispatchEvent(new Event('exposure-versions-changed'));
  });
  await analysis.getByText(/4 files · 3 exposures after confirmed links/).waitFor();
  await analysis
    .getByRole('button', { name: /Exposure/ })
    .filter({ hasText: '11.0m' })
    .waitFor();
  await page.evaluate(() => {
    window.__statsFail = true;
    window.dispatchEvent(new Event('exposure-versions-changed'));
  });
  await analysis
    .getByRole('button', { name: /Exposure/ })
    .filter({ hasText: 'Unavailable' })
    .waitFor();
  await page.evaluate(() => {
    window.__statsFail = false;
    window.__exposureIds = [1, 4];
    window.dispatchEvent(new Event('exposure-versions-changed'));
  });
  await analysis.getByText(/4 files · 2 exposures after confirmed links/).waitFor();
  console.log(
    'Statistics reflect confirmed exposure IDs, refresh after unlink, and hide stale totals after a failure.',
  );
};
