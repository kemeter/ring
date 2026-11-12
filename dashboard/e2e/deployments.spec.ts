import { expect, authedTest as test } from './fixtures';

test('creates a deployment, tails its logs, runs a command in it and deletes it', async ({
  page,
  namespace
}) => {
  await page.goto('/deployments');
  await page.getByRole('link', { name: 'New deployment' }).click();

  await page.getByLabel('Namespace').fill(namespace);
  await page.getByLabel('Name', { exact: true }).fill('ticker');
  await page.getByLabel('Image', { exact: true }).fill('busybox:latest');
  await page
    .getByLabel('Command')
    .fill('sh\n-c\ni=0; while true; do i=$((i+1)); echo tick $i; sleep 1; done');
  await page.getByRole('button', { name: 'Deploy' }).click();

  await expect(page).toHaveURL(/\/deployments\/[0-9a-f-]{36}$/);
  await expect(page.getByRole('heading', { name: 'ticker' })).toBeVisible();

  // The live tail goes through the dashboard proxy as a stream: new lines
  // keep arriving instead of waiting for the response to end.
  await expect(page.getByText(/^tick \d+$/).first()).toBeVisible({ timeout: 60_000 });
  const ticks = page.getByText(/^tick \d+$/);
  const before = await ticks.count();
  await expect.poll(() => ticks.count(), { timeout: 15_000 }).toBeGreaterThan(before);

  // The terminal is a WebSocket relayed by the same proxy.
  await page.getByRole('button', { name: 'Connect' }).click();
  await expect(page.getByText('connected')).toBeVisible();
  await page.locator('.xterm').click();
  await page.keyboard.type('echo e2e-$((6*7))');
  await page.keyboard.press('Enter');
  await expect(page.locator('.xterm-rows')).toContainText('e2e-42');
  await page.getByRole('button', { name: 'Disconnect' }).click();

  await page.getByRole('button', { name: 'Delete' }).click();
  await page
    .getByRole('group', { name: `Delete ${namespace}/ticker?` })
    .getByRole('button', { name: 'Delete' })
    .click();
  await expect(page).toHaveURL(/\/deployments$/);
  await expect(
    page.getByRole('row', { name: new RegExp(`${namespace}.*ticker.*deleted`) })
  ).toBeVisible();
});
