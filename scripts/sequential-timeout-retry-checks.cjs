async page => {
  await page.evaluate(() => {
    window.__failures = [
      { frameId: 102, filename: 'timeout.fits', path: '/fixture/timeout.fits', code: 'TIMEOUT' },
      {
        frameId: 103,
        filename: 'no-solution.fits',
        path: '/fixture/no-solution.fits',
        code: 'NO_SOLUTION',
      },
      { frameId: 104, filename: 'timeout2.fits', path: '/fixture/timeout2.fits', code: 'TIMEOUT' },
    ].map(row => ({ ...row, status: 'failed', attemptedAt: '2026-09-09T00:00:00Z' }));
  });
  await page.getByRole('button', { name: 'Failed solves', exact: true }).click();
  const retry = page.getByRole('button', { name: 'Retry timed out sequentially (2)', exact: true });
  await retry.click();
  await page.waitForFunction(() => window.__calls.some(c => c.command === 'plate_solve_batch'));
  const calls = await page.evaluate(() =>
    window.__calls.filter(c => c.command === 'plate_solve_batch'),
  );
  if (
    calls.length !== 1 ||
    !calls[0].args.sequential ||
    JSON.stringify(calls[0].args.frameIds) !== '[102,104]'
  )
    throw Error('Retry must select only timeout frames and request a single sequential batch');
  if (!(await retry.isDisabled())) throw Error('Active retry must prevent duplicate enqueue');
  await page.evaluate(() => window.__finish());
  await page.waitForFunction(() => !!window.__finish);
  console.log(
    'Sequential retry selects only timeouts, sends one bounded batch and disables duplicate enqueue.',
  );
};
