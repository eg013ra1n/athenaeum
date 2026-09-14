async page => {
  const assert = (value, message) => {
    if (!value) throw Error(message);
  };
  // Only the pixel renderer is stubbed: the real queue, dialogs and API wiring run.
  await page.route('**/src/components/BlinkViewer.tsx*', route =>
    route.fulfill({
      contentType: 'application/javascript',
      body: `
    import React from '/node_modules/.vite/deps/react.js';
    import {SolveFailureBadge} from '/src/components/plate-solve/FailedSolvesReview.tsx';
    export default function BlinkViewer({frames,onClose}){return React.createElement('div',{role:'dialog','aria-label':'Blink fixture',style:{position:'fixed',inset:0,zIndex:100,background:'#222'}},React.createElement(SolveFailureBadge,{frameId:frames[0].frame.id}),React.createElement('p',null,frames[0].file.filename),React.createElement('button',{onClick:onClose},'Close Blink'));}
  `,
    }),
  );
  await page.reload();
  await page.getByRole('button', { name: 'Failed solves', exact: true }).click();
  await page.getByRole('heading', { name: 'Failed solves (1)', exact: true }).waitFor();
  await page.getByRole('button', { name: 'timed-out.fits', exact: true }).click();
  await page.getByRole('dialog', { name: 'Blink fixture' }).waitFor();
  await page.getByText(/Plate solve timed out ·/).waitFor();
  await page.getByRole('button', { name: 'Close Blink', exact: true }).click();
  await page.getByRole('button', { name: 'Close', exact: true }).click();
  await page.evaluate(() => (window.__webImmediate = true));
  await page.getByRole('button', { name: 'Select All (2)', exact: true }).click();
  await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).click();
  await page.waitForFunction(() => window.__calls.some(c => c.command === 'plate_solve_batch'));
  assert(
    await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).isDisabled(),
    'HTTP acceptance must not finish queue',
  );
  await page.evaluate(() => {
    window.__emit('plate-solve-progress', {
      frameId: 101,
      current: 0,
      total: 3,
      status: 'solving',
      filename: 'active-one.fits',
    });
    window.__emit('plate-solve-progress', {
      frameId: 102,
      current: 0,
      total: 3,
      status: 'solving',
      filename: 'active-two.fits',
    });
  });
  await page.getByRole('button', { name: 'active-one.fits', exact: true }).waitFor();
  await page.getByRole('button', { name: 'active-two.fits', exact: true }).click();
  await page.getByRole('dialog', { name: 'Blink fixture' }).waitFor();
  assert(
    await page.evaluate(() =>
      window.__calls.some(
        c => c.command === 'get_files_with_frames_by_ids' && c.args.frameIds[0] === 102,
      ),
    ),
    'Blink uses active frame identity',
  );
  await page.getByRole('button', { name: 'Close Blink', exact: true }).click();
  await page.evaluate(() => {
    window.__emit('plate-solve-progress', {
      frameId: 102,
      current: 2,
      total: 3,
      status: 'failed',
      filename: 'active-two.fits',
      failureCode: 'TIMEOUT',
      error: 'Timeout fixture',
    });
    window.__emit('plate-solve-progress', {
      frameId: 101,
      current: 1,
      total: 3,
      status: 'solved',
      matchedStars: 20,
      rmsArcsec: 1,
    });
  });
  await page.getByText('1 solved · 1 failed (1 timed out)', { exact: true }).waitFor();
  await page.getByText('2 / 3', { exact: true }).waitFor();
  const eta = await page.evaluate(async () => {
    const { remainingSolveTime: f } =
      await import('/src/components/plate-solve/PlateSolveLiveDetails.tsx');
    return [f(1000, 9, 100, 61000), f(1000, 10, 20, 61000), f(1000, 10, 100, 61000)];
  });
  assert(
    JSON.stringify(eta) === JSON.stringify(['Calculating…', '~1m', '~9m']),
    'throughput ETA warm-up and estimates',
  );
  await page.getByRole('button', { name: 'Cancel plate-solve batch', exact: true }).click();
  await page.getByText('Batch solve cancelled', { exact: true }).waitFor();
  assert(
    await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).isEnabled(),
    'SSE completion releases queue',
  );
  await page.getByRole('button', { name: 'Failed solves', exact: true }).click();
  await page.getByRole('heading', { name: 'Failed solves (1)', exact: true }).waitFor();
  await page.screenshot({ path: 'output/playwright/failed-solves-review.png' });
  await page.evaluate(() => (window.__failures = []));
  await page.getByRole('button', { name: 'Refresh', exact: true }).click();
  await page.getByText('No failed solves recorded.', { exact: true }).waitFor();
  console.log(
    'Durable review/Blink wiring, concurrent filenames, timeout counts, out-of-order progress, ETA and asynchronous web completion passed.',
  );
};
