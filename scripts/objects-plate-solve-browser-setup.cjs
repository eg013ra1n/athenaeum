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
    window.__failures=[{frameId:102,filename:'timed-out.fits',path:'/fixture/timed-out.fits',status:'failed',code:'TIMEOUT',error:'Plate solve exceeded the 60 second per-frame time limit',attemptedAt:'2026-09-08T12:00:00Z'}];
    window.__calls=[]; window.__listeners={}; window.__catalog=true;
    window.__emit=(name,payload)=>(window.__listeners[name]||[]).forEach(cb=>cb(payload));
    const sets=[['Orion',false,false],['Andromeda',false,false],['Rosette',true,false],['Archived',true,true]].map(([name,is_custom,is_archived],i)=>({frames_set:{id:i+1,name,is_custom,is_archived, objctra:'05 35 00',objctdec:'-05 23 00',date_obs_start:'2026-01-01',date_obs_end:'2026-01-02',total_exp_time:3600},member_count:2}));
    const settings={};
    export const api={
      listen:async(name,cb)=>{(window.__listeners[name]??=[]).push(cb);return()=>window.__listeners[name]=window.__listeners[name].filter(x=>x!==cb)},
      invoke:async(command,args={})=>{
        window.__calls.push({command,args});
        if(command==='get_frames_sets')return sets;
        if(command==='get_setting')return settings[args.key]??args.defaultValue;
        if(command==='set_setting'){settings[args.key]=args.value;return;}
        if(command==='get_excluded_frames_count')return 0;
        if(command==='get_plate_solve_attempts')return window.__failures.filter(r=>!args.frameIds.length||args.frameIds.includes(r.frameId));
        if(command==='get_files_with_frames_by_ids')return args.frameIds.map(id=>({file:{id,filename:'frame-'+id+'.fits',path:'/fixture/frame-'+id+'.fits'},frame:{id,imagetyp:'Light'}}));
        if(command==='get_catalog_status')return [{installed:window.__catalog}];
        if(command==='get_object_plate_solve_frame_ids'){
          if(window.__resolveError)throw Error('Fixture resolution failure');
          return window.__empty?[]:args.framesSetIds.flatMap(id=>id===1?[101,102]:id===2?[102,103]:[104]);
        }
        if(command==='plate_solve_batch'){ if(window.__webImmediate){ window.__finish=()=>window.__emit('plate-solve-complete',{solved:1,failed:1,total:args.frameIds.length,totalTimeMs:50,cancelled:true,notProcessed:1});return; } return new Promise(resolve=>{window.__finish=()=>{window.__emit('plate-solve-complete',{solved:1,failed:1,total:args.frameIds.length,totalTimeMs:50});resolve();}}); }
        if(command==='cancel_plate_solve'){window.__finish();return;}
        return null;
      }
    };
  `,
    }),
  );
  await page.goto('http://127.0.0.1:1420/objects');
};
