async page => {
  await page.unrouteAll();
  await page.route('**/src/App.tsx*', async route => {
    const original = await (await route.fetch()).text();
    const router = original.match(/["']([^"']*react-router-dom[^"']*)["']/)[1];
    await route.fulfill({
      contentType: 'application/javascript',
      body: `
      import React from '/node_modules/.vite/deps/react.js';
      import {BrowserRouter, Routes, Route} from '${router}';
      import Objects from '/src/pages/Objects.tsx';
      import {NotificationProvider} from '/src/contexts/NotificationContext.tsx';
      import {SessionStateProvider} from '/src/contexts/SessionStateContext.tsx';
      import {NavHistoryProvider} from '/src/contexts/NavHistoryContext.tsx';
      import {PlateSolveProgressProvider} from '/src/contexts/PlateSolveProgressContext.tsx';
      import {PlateSolveIndexMissingModal} from '/src/components/plate-solve/PlateSolveIndexMissingModal.tsx';
      const h=React.createElement;
      export default function App(){ return h(BrowserRouter,null,h(NotificationProvider,null,h(SessionStateProvider,null,h(NavHistoryProvider,null,h(PlateSolveProgressProvider,null,h(PlateSolveIndexMissingModal),h(Routes,null,h(Route,{path:'/objects',element:h(Objects)}),h(Route,{path:'/objects/:id',element:h('h1',null,'Object destination')}))))))); }
    `,
    });
  });
  await page.route('**/src/api/index.ts*', route =>
    route.fulfill({
      contentType: 'application/javascript',
      body: `
    window.__calls=[]; window.__listeners={}; window.__catalog=true;
    window.__emit=(name,payload)=>(window.__listeners[name]||[]).forEach(cb=>cb(payload));
    const sets=[['Orion',false,false],['Andromeda',false,false],['Rosette',true,false],['Archived',true,true]].map(([name,is_custom,is_archived],i)=>({frames_set:{id:i+1,name,is_custom,is_archived, objctra:'05 35 00',objctdec:'-05 23 00',date_obs_start:'2026-01-01',date_obs_end:'2026-01-02',total_exp_time:3600},member_count:2}));
    const settings={};
    const versions=['calibrated','registered','integrated','unknown'].map((stage,i)=>({frameId:i+1,filename:['siril_calibrated.fit','pixinsight_registered.xisf','dss_stack.fits','light.fit'][i],path:'/isolated-fixture/'+(i+1),dateObs:'2026-05-21T11:31:25Z',camera:'QHYminiCam8M',exposureSeconds:300,filter:'G',width:3856,height:2180,classification:{stage,steps:[stage],evidence:[stage==='integrated'?'NCOMBINE=20: multiple input images':'Fixture processing history'],confidence:'header evidence',sourceId:null,sourceName:null},exposureId:null,manualStage:false}));
    export const api={
      listen:async(name,cb)=>{(window.__listeners[name]??=[]).push(cb);return()=>window.__listeners[name]=window.__listeners[name].filter(x=>x!==cb)},
      invoke:async(command,args={})=>{
        window.__calls.push({command,args});
        if(command==='get_frames_sets')return sets;
        if(command==='get_exposure_version_review'){
          const linked=versions[0].exposureId && versions[0].exposureId===versions[1].exposureId;
          const count=versions.filter(v=>v.classification.stage!=='integrated').length-(linked?1:0);
          const suggestions=linked?[]:[{leftId:1,rightId:2,confidence:'needs review',evidence:['Observation time, camera, exposure duration and filter agree']}];
          return {versions,suggestions,suggestionCount:suggestions.length,exposureCount:count,exposureSeconds:count*300};
        }
        if(command==='confirm_exposure_version_link'){versions[0].exposureId=versions[1].exposureId='fixture-group';return;}
        if(command==='unlink_exposure_version'){versions.find(v=>v.frameId===args.frameId).exposureId=null;return;}
        if(command==='set_processing_stage'){
          if(window.__stageError)throw Error('Fixture update rejected');
          const v=versions.find(v=>v.frameId===args.frameId);v.classification.stage=args.stage||'unknown';v.manualStage=!!args.stage;return;
        }
        if(command==='get_setting')return settings[args.key]??args.defaultValue;
        if(command==='set_setting'){settings[args.key]=args.value;return;}
        if(command==='get_excluded_frames_count')return 0;
        if(command==='get_catalog_status')return [{installed:window.__catalog}];
        if(command==='get_object_plate_solve_frame_ids'){
          if(window.__resolveError)throw Error('Fixture resolution failure');
          return window.__empty?[]:args.framesSetIds.flatMap(id=>id===1?[101,102]:id===2?[102,103]:[104]);
        }
        if(command==='plate_solve_batch')return new Promise(resolve=>{window.__finish=()=>{window.__emit('plate-solve-complete',{solved:1,failed:1,total:args.frameIds.length,total_time_ms:50});resolve();}});
        if(command==='cancel_plate_solve'){window.__finish();return;}
        return null;
      }
    };
  `,
    }),
  );
  await page.goto('http://127.0.0.1:1420/objects');
};
