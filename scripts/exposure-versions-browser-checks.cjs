async page => {
  const assert = (ok, message) => {
    if (!ok) throw Error(message);
  };
  await page.getByRole('button', { name: 'Select All (2)', exact: true }).click();
  await page.getByRole('button', { name: 'Review processing / versions', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Processing and exposure versions' });
  await dialog.getByText(/4 available files · 3 exposure candidates/).waitFor();
  await dialog.getByText('NCOMBINE=20: multiple input images', { exact: true }).waitFor();
  await dialog.getByRole('button', { name: 'Confirm same exposure', exact: true }).click();
  await dialog.getByText(/4 available files · 2 exposure candidates/).waitFor();
  assert(
    (await dialog.getByRole('button', { name: 'Unlink version', exact: true }).count()) === 2,
    'Both available versions retained',
  );
  await dialog.getByRole('button', { name: 'Unlink version', exact: true }).first().click();
  await dialog.getByText(/4 available files · 3 exposure candidates/).waitFor();
  await dialog
    .getByRole('combobox', { name: 'Processing stage for frame 4', exact: true })
    .selectOption('raw');
  await page.waitForFunction(() =>
    window.__calls.some(c => c.command === 'set_processing_stage' && c.args.stage === 'raw'),
  );
  await page.evaluate(() => (window.__stageError = true));
  await dialog
    .getByRole('combobox', { name: 'Processing stage for frame 4', exact: true })
    .selectOption('calibrated');
  await dialog.getByRole('alert').filter({ hasText: 'Fixture update rejected' }).waitFor();
  await page.evaluate(() => (window.__stageError = false));
  await dialog
    .getByRole('combobox', { name: 'Processing stage for frame 4', exact: true })
    .selectOption('');
  await dialog.getByRole('button', { name: 'Close review', exact: true }).click();
  assert((await dialog.count()) === 0, 'Review closes');
  console.log(
    'Processing review, confirmation, unlink, stage override/reset and update errors passed.',
  );
};
