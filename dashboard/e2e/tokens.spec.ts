import { expect, authedTest as test } from './fixtures';

test('creates a token shown once, then rotates it', async ({ page, api }) => {
  const name = `ci-${Date.now().toString(36)}`;

  await page.goto('/tokens');
  await page.getByRole('button', { name: 'New token' }).click();
  await page.getByLabel('Name', { exact: true }).fill(name);
  await page.getByRole('button', { name: 'Create' }).click();

  const reveal = page.getByRole('status');
  await expect(reveal).toContainText(`Token ${name}`);
  const first = (await reveal.locator('code').textContent())?.trim() ?? '';
  expect(first).toMatch(/^ring_pat_/);
  expect(await api.statusWith(first, '/deployments')).toBe(200);

  await reveal.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByText(first)).toBeHidden();

  const row = page.getByRole('row', { name: new RegExp(`${name}.*active`) });
  await row.getByRole('button', { name: 'Rotate' }).click();
  await row.getByRole('button', { name: 'Rotate' }).click();

  const second = (await page.getByRole('status').locator('code').textContent())?.trim() ?? '';
  expect(second).toMatch(/^ring_pat_/);
  expect(second).not.toBe(first);
  expect(await api.statusWith(first, '/deployments')).toBe(401);
  expect(await api.statusWith(second, '/deployments')).toBe(200);
});
