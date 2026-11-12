import { ADMIN, expect, test } from './fixtures';

test('signs in with the login form and signs out', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveURL(/\/login$/);

  await page.getByLabel('Username').fill(ADMIN.username);
  await page.getByLabel('Password').fill(ADMIN.password);
  await page.getByRole('button', { name: 'Sign in' }).click();

  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page).toHaveURL(/\/login$/);
});

test('shows the reason of a refused sign-in', async ({ page }) => {
  await page.goto('/login');
  await page.getByLabel('Username').fill(ADMIN.username);
  await page.getByLabel('Password').fill('not-the-password');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByText(/401/)).toBeVisible();
});
