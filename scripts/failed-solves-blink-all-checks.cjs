async page => {
  const assert = (value, message) => {
    if (!value) throw Error(message);
  };
  await page.route('**/src/components/BlinkViewer.tsx*', route =>
    route.fulfill({
      contentType: 'application/javascript',
      body: `
    import React from '/node_modules/.vite/deps/react.js';
    export default function BlinkViewer({frames,initialIndex,onClose}){
      window.__blink={ids:frames.map(f=>f.frame.id),initialIndex};
      return React.createElement('div',{role:'dialog','aria-label':'Blink fixture',style:{position:'fixed',inset:0,zIndex:100,background:'#222'}},React.createElement('p',null,'Blink '+frames.length+' frames from '+initialIndex),React.createElement('button',{onClick:onClose},'Close Blink'));
    }
  `,
    }),
  );
  await page.reload();
  await page.evaluate(
    () =>
      (window.__failures = Array.from({ length: 505 }, (_, i) => ({
        frameId: 1000 + i,
        filename: 'failed-' + i + '.fits',
        path: '/fixture/' + i + '.fits',
        status: 'failed',
        code: 'TIMEOUT',
        error: 'Fixture timeout',
        attemptedAt: '2026-09-09T00:00:00Z',
      }))),
  );
  await page.getByRole('button', { name: 'Failed solves', exact: true }).click();
  await page.getByRole('button', { name: 'Blink all failed (505)', exact: true }).click();
  await page.getByRole('dialog', { name: 'Blink fixture' }).waitFor();
  let blink = await page.evaluate(() => window.__blink);
  assert(blink.ids.length === 505 && blink.initialIndex === 0, 'all pages passed to Blink');
  assert(blink.ids[0] === 1000 && blink.ids[504] === 1504, 'review order preserved');
  const sizes = await page.evaluate(() =>
    window.__calls
      .filter(c => c.command === 'get_files_with_frames_by_ids')
      .map(c => c.args.frameIds.length),
  );
  assert(JSON.stringify(sizes) === JSON.stringify([400, 105]), 'bounded metadata queries');
  await page.getByRole('button', { name: 'Close Blink', exact: true }).click();
  await page.getByRole('button', { name: 'Next', exact: true }).click();
  await page.getByRole('button', { name: 'failed-50.fits', exact: true }).click();
  await page.getByRole('dialog', { name: 'Blink fixture' }).waitFor();
  blink = await page.evaluate(() => window.__blink);
  assert(
    blink.ids.length === 505 && blink.initialIndex === 50,
    'clicked second-page file starts at correct image with full collection',
  );
  console.log(
    'Blink all 505 failures, bounded queries, full-list navigation and second-page starting identity passed.',
  );
};
