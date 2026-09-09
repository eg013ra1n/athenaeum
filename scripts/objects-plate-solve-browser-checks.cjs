async page => {
  const assert = (ok, msg) => {
    if (!ok) throw Error(msg);
  };
  await page.getByRole('checkbox', { name: 'Select Orion (object 1)', exact: true }).check();
  assert(
    await page.getByRole('button', { name: 'Plate Solve Selected (1)', exact: true }).isEnabled(),
    'single selection',
  );
  await page.getByRole('button', { name: 'Select All (2)', exact: true }).click();
  await page.getByTitle('Table view', { exact: true }).click();
  assert(
    (await page.getByRole('checkbox', { checked: true }).count()) === 2,
    'selection preserved between views',
  );
  await page.getByRole('columnheader', { name: /Name/ }).click();
  assert(
    (await page.getByRole('checkbox', { checked: true }).count()) === 2,
    'sort preserves selection',
  );
  await page.getByTitle('Filter frame sets', { exact: true }).click();
  await page.getByPlaceholder('Search...', { exact: true }).fill('Orion');
  assert((await page.getByRole('checkbox').count()) === 1, 'filter scoped');
  await page.getByRole('button', { name: 'Plate Solve Selected (1)', exact: true }).waitFor();
  await page.getByPlaceholder('Search...', { exact: true }).fill('');
  assert(
    (await page.getByRole('checkbox', { checked: true }).count()) === 1,
    'hidden selection removed',
  );
  await page.getByRole('button', { name: 'Work In Progress 1', exact: true }).click();
  assert(
    (await page.getByRole('checkbox', { checked: true }).count()) === 0,
    'tab change clears selection',
  );
  await page.getByRole('button', { name: 'Select All (1)', exact: true }).click();
  await page.getByTitle('Enter Merge Mode (M)', { exact: true }).click();
  assert(
    (await page.getByRole('checkbox', { checked: true }).count()) === 0,
    'merge clears selection',
  );
  assert(await page.getByRole('checkbox').first().isDisabled(), 'merge disables selection');
  await page.getByTitle('Exit Merge Mode (M)', { exact: true }).first().click();
  await page.getByRole('button', { name: 'Stage 2', exact: true }).click();
  await page.getByRole('button', { name: 'Select All (2)', exact: true }).click();
  await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).click();
  await page.waitForFunction(() => window.__calls.some(c => c.command === 'plate_solve_batch'));
  const calls = await page.evaluate(() => window.__calls);
  assert(
    JSON.stringify(
      calls.find(c => c.command === 'get_object_plate_solve_frame_ids').args.framesSetIds,
    ) === '[1,2]',
    'object IDs',
  );
  assert(
    JSON.stringify(calls.find(c => c.command === 'plate_solve_batch').args.frameIds) ===
      '[101,102,103]',
    'unique frame IDs',
  );
  assert(
    await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).isDisabled(),
    'prevent duplicate active solve',
  );
  await page.getByText('Preparing...', { exact: true }).waitFor();
  await page.evaluate(() =>
    window.__emit('plate-solve-progress', {
      frameId: 101,
      current: 1,
      total: 3,
      status: 'solving',
    }),
  );
  await page.getByText('Solving Frame #101', { exact: true }).waitFor();
  await page.evaluate(() =>
    window.__emit('plate-solve-progress', {
      frameId: 102,
      current: 2,
      total: 3,
      status: 'failed',
      error: 'Fixture no solution',
      filename: 'copied-fixture.fits',
    }),
  );
  await page.getByRole('button', { name: 'Cancel plate-solve batch', exact: true }).click();
  await page.getByText('Batch solve complete', { exact: true }).waitFor();
  await page.getByText('Fixture no solution', { exact: true }).waitFor();
  assert(
    await page.evaluate(() => window.__calls.some(c => c.command === 'cancel_plate_solve')),
    'cancellation forwarded',
  );
  assert(
    (await page.evaluate(
      () => window.__calls.filter(c => c.command === 'get_frames_sets').length,
    )) >= 2,
    'refresh after solve',
  );
  await page.evaluate(() => (window.__empty = true));
  await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).click();
  await page.waitForFunction(
    () => window.__calls.filter(c => c.command === 'get_object_plate_solve_frame_ids').length === 2,
  );
  assert(
    (await page.evaluate(
      () => window.__calls.filter(c => c.command === 'plate_solve_batch').length,
    )) === 1,
    'empty eligible list not queued',
  );
  await page.evaluate(() => {
    window.__empty = false;
    window.__resolveError = true;
  });
  await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).click();
  await page.waitForFunction(
    () => window.__calls.filter(c => c.command === 'get_object_plate_solve_frame_ids').length === 3,
  );
  assert(
    (await page.evaluate(
      () => window.__calls.filter(c => c.command === 'plate_solve_batch').length,
    )) === 1,
    'resolution failure not queued',
  );
  await page.reload();
  await page.evaluate(() => (window.__catalog = false));
  await page.getByRole('button', { name: 'Select All (2)', exact: true }).click();
  await page.getByRole('button', { name: 'Plate Solve Selected (2)', exact: true }).click();
  await page.getByText('Star catalog not downloaded', { exact: true }).waitFor();
  assert(
    (await page.evaluate(
      () => window.__calls.filter(c => c.command === 'plate_solve_batch').length,
    )) === 0,
    'missing catalog gate',
  );
  console.log(
    'Objects selection, filtering, views, merge mode, deduplication, progress, cancellation, errors, refresh and catalog gate passed.',
  );
};
